# FilmCraft: review, build and desktop launcher (2026-10-10)

## Outcome
FilmCraft 0.5.0 builds from this checkout on Windows 11 and runs. A **FilmCraft** shortcut is on the Desktop. I verified it by launching it from the shortcut. A control-channel screenshot of the running app is in [filmcraft-running.png](filmcraft-running.png): the Edit workspace with the demo project, Program monitor, timeline and meters.

## Review of the app
- **What it is:** a clean-room, pure-Rust non-linear editor aiming at Premiere Pro parity. It is native on macOS, Windows and Linux, with a WASM web build.
- **Size:** 43 crates (`crates/*`) plus three apps (`filmcraft`, `filmcraft-cli`, `filmcraft-web`) and `xtask`. Roughly 322k lines of Rust and about 2,200 `#[test]` functions (counted by grep, not run).
- **Layering:** L0 codecs and containers (h264, hevc, av1, prores, aac and others), then engine, render and gpu, then `ui-egui` (egui/eframe on wgpu). The `crates/platform` crate holds the OS hardware-decode bindings.
- **Windows specifics:** `apps/filmcraft/src/graphics.rs` leaves OpenGL out of the wgpu instance (an AMD driver crash workaround). Hardware decoding via Media Foundation reported available on this machine.
- **Agent surface:** a JSON-lines control server (`--control <port>`), an MCP server in the CLI, and every action is an engine command.
- **Self-reported status (ROADMAP.md):** about 87% of the feature checklist, but only about 50-60% "ready for real work". The roadmap says Windows and Linux are "built for releases but barely exercised at runtime". No plugin hosting (VST3/OpenFX). Whisper speech-to-text is off in default builds.
- **Not done:** I did not run the test suite or the quality gates (`cargo test`, clippy, `cargo xtask ci`), and I did not audit the code line by line.

## What I did
1. Found no `cargo` on PATH. A Rust install existed under `~/.cargo` (1.93.1) but was not on PATH, and the repo needs 1.95+.
2. With your approval, downloaded `rustup-init.exe` from static.rust-lang.org, checked its SHA-256 against the published `.sha256`, and installed the stable toolchain. That updated Rust to **1.99.0** (minimal profile, user-level, no admin). I did not change any other machine settings. `~/.cargo/bin` may not be on your PATH in existing shells; the launcher adds it itself.
3. Ran `cargo build --release -p filmcraft -p filmcraft-cli`. It finished in 6m59s with no errors. Outputs: `target/release/filmcraft.exe` (54 MB) and `filmcraft-cli.exe` (35 MB).
4. Launched with `--control 9876`. The window opened with the demo project, the control port listened, and a screenshot was captured through `ui.screenshot`. Quit cleanly with `app.quit`.

## Files added or changed in the repo
- `packaging/windows/launch-filmcraft.bat` (new, untracked). Starts `target\release\filmcraft.exe`. If the exe is missing, or you pass `--rebuild`, it runs the release build first. It looks for cargo on PATH, then in `%USERPROFILE%\.cargo\bin`. Other arguments (a project file, `--control 9876`) are passed through.
- `reports/` (new): this report and the screenshot.
- Outside the repo: `Desktop\FilmCraft.lnk`. It runs the launcher minimized, with `assets/app-icon/filmcraft.ico` as its icon.
- Nothing is committed and no source code was changed.

## Things to know
- The launcher runs the build in your checkout. After a `git pull`, run it with `--rebuild` (an incremental build).
- The build does not include the optional Japanese/CJK fonts from `storytold/craft-fonts`. Without them the app uses its own and system fonts. Release builds from the project include them.
- The build log is not kept in the repo. It is at `%TEMP%\filmcraft-build.log`.
- The `whisper` feature (speech-to-text) is not enabled. To add it: `cargo build --release -p filmcraft --features whisper`.
- `target/` is about several GB and is gitignored.

## Still needs attention
- Decide whether to commit the launcher and the `reports/` folder (the launcher may be useful to others on Windows).
- Run `cargo xtask ci` if you want the quality gates verified on this machine.
- Interactive use (import, edit, export, audio playback) was not exercised beyond the demo project opening, so Windows runtime behaviour is only lightly checked.

## Update: crash troubleshooting (log.txt)
You reported the app keeps crashing. I could not reproduce it: two launches ran and quit cleanly, FilmCraft's own log (`%APPDATA%\FilmCraft\Logs\`) has no crash files, and the Windows Application event log has no `filmcraft.exe` crash events. So the cause is unknown; a hard native crash (driver/GPU) would leave no log by default.

Added a log mode to the launcher:
- `packaging\windows\launch-filmcraft.bat --log` (or the new Desktop shortcut **FilmCraft (log)**) runs the app with `RUST_LOG=warn,filmcraft*=debug` and `RUST_BACKTRACE=full`, waits for it to exit, and writes `log.txt` in the repo root (overwritten each run): everything the app prints, its exit code with a legend of Windows crash codes, the last 200 lines of the app log, any crash logs from the last day, and Windows crash events for filmcraft.exe. If the exit code is non-zero it pauses so the window stays visible.
- A build triggered by `--log` writes its output to `log.txt` too.
- `/log.txt` is now in `.gitignore`.

Next step: reproduce the crash with **FilmCraft (log)**, then read `log.txt`.

## Update: MCP and the control port
The repo ships an MCP server, `filmcraft-cli mcp`, registered in `.mcp.json` in two modes: `filmcraft-headless` (`mcp --demo`, in-process, no window) and `filmcraft` (`mcp --bridge 127.0.0.1:9876`, drives the running app). Both showed "Connection closed" at the start of the session only because `filmcraft-cli.exe` was not built yet; a stdio handshake and tool listing now succeed.

Bridge mode needs the app started with the control port open. Added a Desktop shortcut, **FilmCraft (MCP)** (`launch-filmcraft.bat --control 9876`), and confirmed port 9876 listens on 127.0.0.1 after launching it. The Desktop shortcuts live in `C:\Users\denni\OneDrive\Desktop`: FilmCraft, FilmCraft (log) and FilmCraft (MCP).

## Status at end of session (2026-10-10)
- PR: https://github.com/storytold/filmcraft/pull/583 (branch `windows-launcher`, pushed to the fork `didpublishing/filmcraft`; no write access to `storytold/filmcraft`).
- Working: release build, launcher, three Desktop shortcuts (FilmCraft, FilmCraft (log), FilmCraft (MCP)), MCP stdio server.
- Open: the crash you reported has no captured log yet. Reproduce it with **FilmCraft (log)** and share `log.txt` (the one reviewed earlier was from the test run, exit code 0). Also not run: `cargo test` and `cargo xtask ci`; the MCP servers need `/mcp` reconnecting in an interactive `claude` terminal.
