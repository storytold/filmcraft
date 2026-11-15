"""Testes da ponte AutoCut -> FilmCraft, contra um servidor de controle falso.

Rodar:  python3 -m unittest discover -s integrations/autocut/tests -v
Precisa de ffmpeg/ffprobe no PATH (gera um video de teste com silencios).
"""
import json
import os
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import filmcraft_autocut as bridge  # noqa: E402

TOKEN = "t" * 64
SEQ_NAME = "clip_autocut"


class FakeFilmCraft(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.calls = []
        self.fail = {}      # comando -> mensagem de erro
        self.n_clips = 4

    def reply(self, command, params):
        if command in self.fail:
            raise ValueError(self.fail[command])
        if command == "project.inspect":
            return {"activeSequence": None, "root": {"children": [
                {"item": 7, "name": "outro", "type": "Sequence"},
                {"children": [{"item": 9, "name": SEQ_NAME, "type": "Sequence"}]}]}}
        if command == "sequence.inspect":
            items = [{"clip": 100 + i, "start": i * 10} for i in range(self.n_clips)]
            return {"video": [{"items": list(reversed(items))}], "audio": []}
        return {}


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            req = json.loads(line)
            if req.get("token") != TOKEN:
                self.wfile.write(b'{"ok": false, "error": "bad token, closing the connection"}\n')
                return
            p = req["params"]
            self.server.calls.append((p.get("command"), p.get("params")))
            try:
                out = {"id": req["id"], "ok": True, "result": self.server.reply(p["command"], p["params"])}
            except ValueError as err:
                out = {"id": req["id"], "ok": False, "error": str(err)}
            self.wfile.write((json.dumps(out) + "\n").encode())


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.srv = FakeFilmCraft()
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        os.environ["FILMCRAFT_CONTROL_TOKEN"] = TOKEN
        self.tmp = Path(tempfile.mkdtemp())

    def tearDown(self):
        self.srv.shutdown()
        self.srv.server_close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def video(self):
        """6 s: 1 s de tom, 2 s de silencio, 1 s de tom, 2 s de silencio."""
        out = self.tmp / "clip.mp4"
        af = "volume=enable='between(t,1,3)+between(t,4,6)':volume=0"
        subprocess.run(
            ["ffmpeg", "-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=320x240:r=25:d=6",
             "-f", "lavfi", "-i", "sine=f=440:d=6", "-af", af, "-shortest",
             "-c:v", "libx264", "-pix_fmt", "yuv420p", str(out)], check=True)
        return out

    def run_main(self, *extra):
        v = self.video()
        bridge.main([str(v), "--port", str(self.port), "--mode", "silence", *extra])
        return v

    def commands(self):
        return [c for c, _ in self.srv.calls]

    def test_import_open_zoom(self):
        v = self.run_main("--zoom", "--zoom-scale", "120")
        xml = v.with_name("clip_autocut.xml")
        self.assertTrue(xml.is_file())
        self.assertIn("<xmeml", xml.read_text(encoding="utf-8"))
        self.assertNotIn("Basic Motion", xml.read_text(encoding="utf-8"), "o zoom vem do FilmCraft, nao do XML")
        self.assertEqual(self.commands()[:3], ["file.import", "project.inspect", "sequence.open"])
        self.assertEqual(self.srv.calls[0][1], {"paths": [str(xml)]})
        self.assertEqual(self.srv.calls[2][1], {"item": 9})
        zooms = [p for c, p in self.srv.calls if c == "effects.setParam"]
        # 4 clipes, em ordem de tempo (100..103): so o 2o e o 4o recebem zoom
        self.assertEqual([z["clip"] for z in zooms], [101, 103])
        self.assertTrue(all(z["effect"] == "motion" and z["param"] == "scale" and z["value"] == 120.0 for z in zooms))

    def test_no_zoom_means_no_setparam(self):
        self.run_main()
        self.assertNotIn("effects.setParam", self.commands())

    def test_bad_token_is_a_clean_error(self):
        os.environ["FILMCRAFT_CONTROL_TOKEN"] = "x" * 64
        with self.assertRaises(SystemExit) as cm:
            self.run_main()
        self.assertIn("bad token", str(cm.exception))

    def test_missing_token(self):
        os.environ["FILMCRAFT_CONTROL_TOKEN"] = ""
        with self.assertRaises(SystemExit) as cm:
            self.run_main()
        self.assertIn("FILMCRAFT_CONTROL_TOKEN", str(cm.exception))

    def test_command_error_is_reported(self):
        self.srv.fail["file.import"] = "unsupported file"
        with self.assertRaises(SystemExit) as cm:
            self.run_main()
        self.assertIn("unsupported file", str(cm.exception))

    def test_no_server(self):
        self.srv.shutdown()
        self.srv.server_close()
        with self.assertRaises(SystemExit) as cm:
            self.run_main()
        self.assertIn("filmcraft --control", str(cm.exception))

    def test_animated_is_refused_before_anything_runs(self):
        with self.assertRaises(SystemExit):
            self.run_main("--animated")
        self.assertEqual(self.srv.calls, [])


if __name__ == "__main__":
    unittest.main()
