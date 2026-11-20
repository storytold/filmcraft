"""FilmCraft GoogleBook MCP companion for a local, user-paired Android editor."""

import argparse
from contextlib import contextmanager
from decimal import Decimal
import hmac
import json
import os
import re
import socket
import subprocess
import time
from typing import Annotated, Literal
from urllib.parse import urlsplit

import httpx
from mcp.server.auth.provider import AccessToken
from mcp.server.auth.settings import AuthSettings
from mcp.server.fastmcp import FastMCP
from mcp.server.transport_security import TransportSecuritySettings
from mcp.types import ToolAnnotations
from pydantic import BaseModel, ConfigDict, Field
from starlette.responses import JSONResponse


TICKS = 254_016_000_000
READ_ONLY = ToolAnnotations(readOnlyHint=True, destructiveHint=False)
PROPOSE = ToolAnnotations(readOnlyHint=False, destructiveHint=False, openWorldHint=False)
SCOPE = "filmcraft.edit"

Seconds = Annotated[float, Field(ge=0, le=86_400)]
Clip = Annotated[int, Field(gt=0, le=2**53 - 1)]
Text = Annotated[str, Field(min_length=1, max_length=400)]


class Model(BaseModel):
    model_config = ConfigDict(extra="forbid")


class Split(Model):
    action: Literal["split"]
    clip: Clip
    at_seconds: Seconds


class Trim(Model):
    action: Literal["trim"]
    clip: Clip
    edge: Literal["in", "out"]
    frames: Annotated[int, Field(ge=-36_000, le=36_000)]
    mode: Literal["regular", "ripple"] = "regular"


class Move(Model):
    action: Literal["move"]
    clip: Clip
    at_seconds: Seconds
    track: Annotated[str, Field(pattern=r"^[VA][1-9][0-9]?$")]


class TextOverlay(Model):
    action: Literal["title", "caption"]
    at_seconds: Seconds
    text: Text
    duration_seconds: Annotated[float, Field(ge=0.05, le=300)] = 3


class Effect(Model):
    action: Literal["effect"]
    clip: Clip
    effect: Literal["gaussian_blur", "light_leaks", "sharpen", "tint"]


class Transition(Model):
    action: Literal["transition"]
    clip: Clip
    effect: Literal["cross_dissolve", "dip_to_black", "dip_to_white"]
    edge: Literal["in", "out"] = "in"
    frames: Annotated[int, Field(ge=1, le=120)] = 12


class Gain(Model):
    action: Literal["gain"]
    clip: Clip
    db: Annotated[float, Field(ge=-60, le=24)]


Edit = Annotated[Split | Trim | Move | TextOverlay | Effect | Transition | Gain, Field(discriminator="action")]


class Revision(Model):
    document: int
    sequence: int
    revision: int


COMMANDS = {
    "split": "timeline.razor",
    "trim": "timeline.trim",
    "move": "timeline.move",
    "title": "graphics.newText",
    "caption": "captions.add",
    "effect": "effects.apply",
    "transition": "sequence.applyVideoTransition",
    "gain": "clip.audioGain",
}


def engine_step(edit: Edit) -> dict:
    args = edit.model_dump(exclude={"action"})
    if "at_seconds" in args:
        args["time"] = round(Decimal(str(args.pop("at_seconds"))) * TICKS)
    if "duration_seconds" in args:
        args["durationSeconds"] = args.pop("duration_seconds")
    if edit.action == "trim":
        args["deltaFrames"] = args.pop("frames")
    return {"id": COMMANDS[edit.action], "params": args}


class Control:
    def __init__(self, port: int, token: str):
        self.port = port
        self.token = token

    def command(self, command: str, params: dict | None = None):
        request = {
            "id": 1,
            "token": self.token,
            "method": "engine.execute",
            "params": {"command": command, "params": params or {}},
        }
        try:
            with socket.create_connection(("127.0.0.1", self.port), timeout=7) as connection:
                connection.sendall((json.dumps(request) + "\n").encode())
                with connection.makefile("rb") as stream:
                    line = stream.readline(4 * 1024 * 1024)
            reply = json.loads(line)
        except (OSError, ValueError) as error:
            raise RuntimeError("Could not reach the editor. Check Connect AI and ADB forwarding.") from error
        if reply.get("id") != 1 or not reply.get("ok"):
            raise RuntimeError(reply.get("error", "Invalid response from FilmCraft."))
        return reply.get("result")


class OAuthVerifier:
    def __init__(self, resource: str, issuer: str, introspection: str, credentials: tuple[str, str]):
        self.resource = resource
        self.issuer = issuer
        self.introspection = introspection
        self.credentials = credentials

    async def verify_token(self, token: str) -> AccessToken | None:
        if not 1 <= len(token) <= 8192:
            return None
        try:
            async with httpx.AsyncClient(timeout=4, follow_redirects=False) as client:
                response = await client.post(self.introspection, data={"token": token}, auth=self.credentials)
                response.raise_for_status()
                claims = response.json()
        except (httpx.HTTPError, ValueError):
            return None
        if not isinstance(claims, dict) or claims.get("active") is not True:
            return None
        audience = claims.get("aud")
        audience = [audience] if isinstance(audience, str) else audience
        scope = claims.get("scope")
        expires = claims.get("exp")
        client_id = claims.get("client_id")
        if (
            not isinstance(audience, list) or self.resource not in audience
            or claims.get("iss", self.issuer) != self.issuer
            or not isinstance(scope, str) or SCOPE not in scope.split()
            or type(expires) is not int or expires <= time.time()
            or not isinstance(client_id, str) or not client_id
        ):
            return None
        return AccessToken(token=token, client_id=client_id, scopes=scope.split(), expires_at=expires, resource=self.resource)


def make_server(control: Control, port: int, oauth: OAuthVerifier | None = None) -> FastMCP:
    security = None
    auth = None
    if oauth:
        auth = AuthSettings(issuer_url=oauth.issuer, resource_server_url=oauth.resource, required_scopes=[SCOPE])
        security = TransportSecuritySettings(allowed_hosts=["127.0.0.1:*", "localhost:*", urlsplit(oauth.resource).netloc])
    server = FastMCP(
        "FilmCraft GoogleBook",
        instructions="Inspect the active timeline, then submit a proposal for on-device approval. The proposal applies only after the editor user approves it.",
        host="127.0.0.1",
        port=port,
        json_response=True,
        stateless_http=True,
        max_request_body_size=65_536,
        token_verifier=oauth,
        auth=auth,
        transport_security=security,
    )

    @server.tool(annotations=READ_ONLY)
    def filmcraft_inspect():
        """Read the open project, active timeline, clip IDs, and revision needed to propose edits."""
        revision = control.command("googlebook.reviewStatus")
        project = control.command("project.inspect")
        timeline = control.command("sequence.inspect") if revision.get("sequence") is not None else None
        current = control.command("googlebook.reviewStatus")
        if any(revision[key] != current[key] for key in ("revision", "document", "sequence")):
            raise RuntimeError("The open timeline changed during inspection. Inspect again.")
        return {"revision": revision, "project": project, "timeline": timeline}

    @server.tool(annotations=PROPOSE)
    def filmcraft_propose(revision: Revision, edits: Annotated[list[Edit], Field(min_length=1, max_length=12)]):
        """Propose splits, trims, moves, titles, captions, effects, transitions, or gain for review.

        Copy document, sequence, and revision from filmcraft_inspect. Times are timeline seconds.
        This tool queues the proposal. The user can compare frames and approve or dismiss in FilmCraft.
        """
        return control.command("googlebook.propose", {**revision.model_dump(), "steps": [engine_step(edit) for edit in edits]})

    @server.tool(annotations=READ_ONLY)
    def filmcraft_review_status():
        """Check whether a FilmCraft proposal is pending, applied, dismissed, or failed."""
        return control.command("googlebook.reviewStatus")

    return server


def oauth_config() -> OAuthVerifier | None:
    resource = os.getenv("FILMCRAFT_MCP_RESOURCE_URL")
    if not resource:
        return None
    issuer = os.getenv("FILMCRAFT_MCP_OAUTH_ISSUER", "")
    introspection = os.getenv("FILMCRAFT_MCP_OAUTH_INTROSPECTION_URL", "")
    client_id = os.getenv("FILMCRAFT_MCP_OAUTH_CLIENT_ID", "")
    client_secret = os.getenv("FILMCRAFT_MCP_OAUTH_CLIENT_SECRET", "")
    for url in (resource, issuer, introspection):
        parts = urlsplit(url)
        if parts.scheme != "https" or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
            raise ValueError("Set valid HTTPS resource, OAuth issuer, and introspection URLs.")
    if urlsplit(resource).path != "/mcp" or not client_id or not client_secret:
        raise ValueError("Use an /mcp resource URL and configure the OAuth introspection client credentials.")
    return OAuthVerifier(resource, issuer, introspection, (client_id, client_secret))


def authenticated(app, key: bytes):
    async def serve(scope, receive, send):
        headers = dict(scope.get("headers", []))
        if scope["type"] == "http" and not hmac.compare_digest(headers.get(b"authorization", b""), b"Bearer " + key):
            await JSONResponse({"error": "Bearer authorization required"}, status_code=401)(scope, receive, send)
            return
        await app(scope, receive, send)
    return serve


@contextmanager
def forwarded_port(device_port: int | None, fallback: int):
    if device_port is None:
        yield fallback
        return
    port = int(subprocess.check_output(["adb", "forward", "tcp:0", f"tcp:{device_port}"], text=True).strip())
    try:
        yield port
    finally:
        subprocess.run(["adb", "forward", "--remove", f"tcp:{port}"], check=False, stdout=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge-port", type=int, default=9876)
    parser.add_argument("--adb-device-port", type=int, help="Forward the port shown in the APK while the companion is running.")
    parser.add_argument("--http-port", type=int, help="Serve Streamable HTTP on loopback. Default transport is stdio.")
    options = parser.parse_args()
    token = os.environ.get("FILMCRAFT_CONTROL_TOKEN", "")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", token):
        parser.error("Set FILMCRAFT_CONTROL_TOKEN to the editor's 64-character pairing code.")
    if any(not 1 <= p <= 65_535 for p in [options.bridge_port, options.adb_device_port, options.http_port] if p is not None):
        parser.error("Ports must be between 1 and 65535.")
    key = os.environ.get("FILMCRAFT_MCP_HTTP_TOKEN", "")
    try:
        oauth = oauth_config() if options.http_port else None
    except ValueError as error:
        parser.error(str(error))
    if options.http_port and not oauth and (not re.fullmatch(r"[0-9a-fA-F]{64}", key) or hmac.compare_digest(key, token)):
        parser.error("Set FILMCRAFT_MCP_HTTP_TOKEN to its own 64-hex-character secret.")
    with forwarded_port(options.adb_device_port, options.bridge_port) as port:
        server = make_server(Control(port, token), options.http_port or 8000, oauth)
        if options.http_port:
            import uvicorn

            app = server.streamable_http_app()
            uvicorn.run(app if oauth else authenticated(app, key.encode()), host="127.0.0.1", port=options.http_port)
        else:
            server.run(transport="stdio")


if __name__ == "__main__":
    main()
