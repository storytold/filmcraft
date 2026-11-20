from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import socket
import socketserver
import subprocess
import sys
import threading
import time
import unittest
from unittest.mock import patch

import httpx
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.client.streamable_http import streamable_http_client
from filmcraft_mcp import OAuthVerifier, Control, make_server


SCRIPT = Path(__file__).with_name("filmcraft_mcp.py").resolve()
PAIR_CODE = "1" * 64
HTTP_KEY = "2" * 64


class Editor(socketserver.StreamRequestHandler):
    def handle(self):
        request = json.loads(self.rfile.readline())
        assert request["token"] == PAIR_CODE
        assert request["method"] == "engine.execute"
        command = request["params"]["command"]
        args = request["params"]["params"]
        self.server.commands.append((command, args))
        status = {"document": 3, "sequence": 4, "revision": 7}
        if command == "googlebook.propose":
            accepted = all(args[key] == value for key, value in status.items())
            self.server.pending |= accepted
            reply = {"ok": True, "result": {"status": "pending_review"}} if accepted else {"ok": False, "error": "stale version"}
        else:
            data = {
                "googlebook.reviewStatus": {**status, "pending": self.server.pending},
                "project.inspect": {"name": "Demo"},
                "sequence.inspect": {"video": [{"items": [{"clip": 15}]}]},
            }
            reply = {"ok": True, "result": data[command]}
        self.wfile.write((json.dumps({"id": request["id"], **reply}) + "\n").encode())


class CompanionTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.editor = socketserver.TCPServer(("127.0.0.1", 0), Editor)
        self.editor.commands = []
        self.editor.pending = False
        self.thread = threading.Thread(target=self.editor.serve_forever)
        self.thread.start()
        self.args = [str(SCRIPT), "--bridge-port", str(self.editor.server_address[1])]
        self.env = {**os.environ, "FILMCRAFT_CONTROL_TOKEN": PAIR_CODE, "FILMCRAFT_MCP_HTTP_TOKEN": HTTP_KEY}

    def tearDown(self):
        self.editor.shutdown()
        self.thread.join()
        self.editor.server_close()

    async def check_edit(self, session):
        await session.initialize()
        names = {tool.name for tool in (await session.list_tools()).tools}
        self.assertEqual(names, {"filmcraft_inspect", "filmcraft_propose", "filmcraft_review_status"})
        inspection = await session.call_tool("filmcraft_inspect", {})
        self.assertFalse(inspection.isError)
        proposal = {
            "revision": {"document": 3, "sequence": 4, "revision": 7},
            "edits": [
                {"action": "trim", "clip": 15, "edge": "out", "frames": -4},
                {"action": "caption", "text": "First line", "at_seconds": 2.5},
            ],
        }
        result = await session.call_tool("filmcraft_propose", proposal)
        self.assertFalse(result.isError, result)
        self.assertTrue(self.editor.pending)
        sent = self.editor.commands[-1][1]["steps"]
        self.assertEqual(sent[0]["params"]["deltaFrames"], -4)
        self.assertEqual(sent[1]["params"]["time"], 635_040_000_000)
        proposal["revision"]["revision"] = 6
        self.assertTrue((await session.call_tool("filmcraft_propose", proposal)).isError)
        proposal["edits"][0]["path"] = "/tmp/arbitrary"
        self.assertTrue((await session.call_tool("filmcraft_propose", proposal)).isError)
        self.assertNotIn("path", self.editor.commands[-1][1]["steps"][0]["params"])

    async def test_stdio_inspect_and_propose(self):
        args = StdioServerParameters(command=sys.executable, args=self.args, env=self.env)
        async with stdio_client(args) as (reader, writer), ClientSession(reader, writer) as session:
            await self.check_edit(session)

    @asynccontextmanager
    async def http_process(self):
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        with subprocess.Popen([sys.executable, *self.args, "--http-port", str(port)], env=self.env, stderr=subprocess.DEVNULL) as child:
            try:
                yield f"http://127.0.0.1:{port}/mcp"
            finally:
                child.terminate()

    async def test_http_authorization_and_proposal(self):
        import asyncio

        async with self.http_process() as url, httpx.AsyncClient() as public:
            for _ in range(40):
                try:
                    response = await public.get(url)
                    break
                except httpx.ConnectError:
                    await asyncio.sleep(0.05)
            else:
                self.fail("The HTTP companion did not start.")
            self.assertEqual(response.status_code, 401)
            async with httpx.AsyncClient(headers={"Authorization": f"Bearer {HTTP_KEY}"}) as private:
                async with streamable_http_client(url, http_client=private) as (reader, writer, _):
                    async with ClientSession(reader, writer) as session:
                        await self.check_edit(session)


class OAuthTest(unittest.IsolatedAsyncioTestCase):
    async def test_metadata_and_audience_scope_expiration(self):
        resource = "https://filmcraft.example/mcp"
        issuer = "https://auth.example"
        verifier = OAuthVerifier(resource, issuer, f"{issuer}/introspect", ("filmcraft", "key"))
        server = make_server(Control(9876, PAIR_CODE), 8767, verifier)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=server.streamable_http_app())) as client:
            metadata = await client.get("https://filmcraft.example/.well-known/oauth-protected-resource/mcp")
        self.assertEqual(metadata.json()["resource"], resource)
        self.assertEqual(metadata.json()["authorization_servers"], [f"{issuer}/"])

        claims = {"active": True, "aud": [resource], "iss": issuer, "scope": "filmcraft.edit", "client_id": "editor", "exp": int(time.time()) + 60}
        original = httpx.AsyncClient

        def provider(request):
            self.assertEqual(str(request.url), f"{issuer}/introspect")
            self.assertIsNotNone(request.headers.get("authorization"))
            return httpx.Response(200, json=claims)

        with patch("filmcraft_mcp.httpx.AsyncClient", side_effect=lambda **opts: original(transport=httpx.MockTransport(provider), **opts)):
            self.assertIsNotNone(await verifier.verify_token("valid"))
            for field, invalid in [("aud", ["https://other.example/mcp"]), ("scope", "other.edit"), ("exp", 0), ("active", False)]:
                normal = claims[field]
                claims[field] = invalid
                self.assertIsNone(await verifier.verify_token("valid"))
                claims[field] = normal


if __name__ == "__main__":
    unittest.main()
