# filmcraft-web

FilmCraft in the browser (wasm32): the engine and egui UI on eframe's web runner, with browser
implementations of the host services (Blob media reads, downloads, OPFS auto-save and recovery,
WebAudio output, WebCodecs decoding) and the `window.filmcraft` agent API.

Build with `cargo xtask web [--serve PORT]`. Toolchain, architecture and the API: [docs/web.md](../../docs/web.md).

`web/` holds the static files copied next to the generated `filmcraft_web.js` / `.wasm`;
`tests/smoke.mjs` is a dependency-free headless-Chrome smoke test. `tests/recovery_startup.mjs`
checks rejected snapshots, download failures and same-name imports after skipped or unreadable
recovery media. Startup reserves existing media names before accepting imports; storage errors
or more than 100,000 media-directory entries pause snapshots and imported-media persistence;
the editor remains usable for manual saves while the recovery warning asks for a reload/retry.

Custom eframe runners construct the wrapper with `WebApp::new(app)`, then set any audio or host
bridge and call `set_context` from their app creator. Custom runners do not write automatic
browser snapshots. Imported media are not persisted until standalone startup has protected their
names; the provided `start` entry point initializes browser recovery. For the external-consumer regression,
set `BUILD_OUTPUT_DIR` and `CARGO_TARGET_DIR` to disposable output and run
`python3 apps/filmcraft-web/tests/custom_runner.py` through a runner that removes that output afterward.
