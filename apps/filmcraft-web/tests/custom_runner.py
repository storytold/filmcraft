"""Build and exercise WebApp::new as an external wasm consumer inside disposable output."""
import functools
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

root = Path(__file__).resolve().parents[3]
output = os.environ.get("BUILD_OUTPUT_DIR")
target_directory = os.environ.get("CARGO_TARGET_DIR")
if not output or not target_directory:
    raise SystemExit("BUILD_OUTPUT_DIR and CARGO_TARGET_DIR must point to disposable build output; use a runner that removes it after this test.")
work = Path(output) / "custom-runner"
(work / "src").mkdir(parents=True, exist_ok=True)
shutil.copyfile(root / "apps/filmcraft-web/tests/fixtures/custom_runner.rs", work / "src/lib.rs")
manifest = '[package]\nname="custom-runner"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[lib]\ncrate-type=["cdylib"]\n[dependencies]\n'
for name, path in [("filmcraft-web", "apps/filmcraft-web"), ("filmcraft-engine", "crates/engine"), ("filmcraft-ui-egui", "crates/ui-egui")]:
    manifest += name + '={path="' + (root / path).as_posix() + '"}\n'
manifest += 'eframe={version="0.37",default-features=false,features=["default_fonts","wgpu"]}\nwasm-bindgen="=0.2.129"\nwasm-bindgen-futures="0.4"\nweb-sys={version="0.3",features=["Window","Document","HtmlCanvasElement"]}\n'
patch = (root / "Cargo.toml").read_text().split("[patch.crates-io]", 1)[1].split("\n[", 1)[0]
manifest += "[patch.crates-io]\n" + patch
(work / "Cargo.toml").write_text(manifest)
shutil.copyfile(root / "Cargo.lock", work / "Cargo.lock")
target = Path(target_directory)
environment = os.environ.copy()
if sys.platform == "darwin":
    # Protected Python launchers may strip dyld settings passed by the build runner.
    sysroot = subprocess.check_output([environment.get("RUSTC", "rustc"), "--print", "sysroot"], text=True).strip()
    libraries = str(Path(sysroot) / "lib")
    previous = environment.get("DYLD_LIBRARY_PATH")
    environment["DYLD_LIBRARY_PATH"] = libraries + (os.pathsep + previous if previous else "")
subprocess.run(["cargo", "build", "--manifest-path", str(work / "Cargo.toml"), "--target", "wasm32-unknown-unknown"], check=True, cwd=root, env=environment)
dist = work / "dist"
dist.mkdir(exist_ok=True)
subprocess.run(["wasm-bindgen", "--target", "web", "--no-typescript", "--out-dir", str(dist), str(target / "wasm32-unknown-unknown/debug/custom_runner.wasm")], check=True, cwd=root, env=environment)
(dist / "index.html").write_text(
    '<!doctype html><canvas id="filmcraft_canvas" style="width:1260px;height:806px"></canvas>'
    '<script type="module">import init,{start_custom} from "./custom_runner.js";'
    'window.filmcraftLoad={};await init();window.startCustomRunner=async()=>{try{await start_custom();'
    'window.filmcraftLoad.readyMs=performance.now()}catch(error){window.filmcraftLoad.error=String(error);throw error}};'
    'window.customModuleReady=true;</script>'
)

class Quiet(SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

server = ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Quiet, directory=str(dist)))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    chrome = os.environ.get("CHROME_BIN", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" if sys.platform == "darwin" else "google-chrome")
    subprocess.run(["node", str(root / "apps/filmcraft-web/tests/recovery_startup.mjs"), f"http://127.0.0.1:{server.server_port}/", chrome, "custom"], check=True, cwd=root, env=environment)
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=10)
