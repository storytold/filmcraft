"""Create a ten-second FilmCraft clip by calling its existing MCP server.

Usage: python examples/mcp_10_second_clip.py PATH_TO_FILMCRAFT_CLI [OUTPUT_MP4]
Only the Python standard library is required.
"""

import json
import subprocess
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip(), file=sys.stderr)
        return 2

    cli = Path(sys.argv[1]).expanduser().resolve()
    if not cli.is_file():
        print(f"FilmCraft CLI not found: {cli}", file=sys.stderr)
        return 2
    output = Path(sys.argv[2] if len(sys.argv) == 3 else "target/filmcraft-mcp-demo.mp4").expanduser().resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    project = output.with_suffix(".fcproj")

    process = subprocess.Popen(
        [str(cli), "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        bufsize=1,
    )
    request_id = 0

    def send(message: dict) -> None:
        assert process.stdin is not None
        process.stdin.write(json.dumps(message, ensure_ascii=False) + "\n")
        process.stdin.flush()

    def request(method: str, params: dict) -> dict:
        nonlocal request_id
        request_id += 1
        send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        assert process.stdout is not None
        while line := process.stdout.readline():
            response = json.loads(line)
            if response.get("id") != request_id:
                continue  # MCP progress notifications have no matching request id.
            if "error" in response:
                raise RuntimeError(f"{method}: {response['error']}")
            return response["result"]
        raise RuntimeError(f"MCP server exited while waiting for {method}")

    def tool(name: str, arguments: dict) -> dict:
        result = request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(f"{name}: {result['content']}")
        return json.loads(result["content"][0]["text"])

    def command(name: str, params: dict) -> dict:
        return tool("command_run", {"id": name, "params": params})

    try:
        request(
            "initialize",
            {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "filmcraft-ten-second-example", "version": "1"}},
        )
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})

        command("file.newSequence", {"name": "MCP demo", "width": 320, "height": 180, "fps": 24})
        for color, start_frame in (("#153b76", 0), ("#e87b29", 120)):
            item = command("file.newColorMatte", {"color": color, "seconds": 5})["item"]
            command("timeline.place", {"item": item, "track": "V1", "frame": start_frame, "insert": False})
        for title, start_ticks in (("FilmCraft + MCP", 0), ("10 seconds with AI", 5 * 254_016_000_000)):
            command("graphics.newText", {"text": title, "position": [20, 95], "size": 25, "seconds": 5, "time": start_ticks})

        sequence = tool("sequence_inspect", {})
        if sequence.get("durationFrames") != 240:
            raise RuntimeError(f"Expected 240 frames, got {sequence.get('durationFrames')}")
        command("file.saveAs", {"path": str(project)})
        export = command("file.exportMedia", {"path": str(output), "format": "h264", "range": "entire", "wait": True})
        if export.get("result", {}).get("frames") != 240 or export.get("offlineMedia") != []:
            raise RuntimeError(f"Unexpected export result: {export}")
        if not output.is_file() or output.stat().st_size == 0:
            raise RuntimeError(f"Export did not create a video: {output}")
        print(json.dumps({"video": str(output), "project": str(project), "frames": 240, "export": export}, ensure_ascii=False))
        return 0
    finally:
        if process.stdin is not None:
            process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        if process.stderr is not None and process.returncode not in (0, None):
            error = process.stderr.read().strip()
            if error:
                print(error, file=sys.stderr)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, RuntimeError) as exc:
        print(f"FilmCraft MCP demo failed: {exc}", file=sys.stderr)
        sys.exit(1)
