# Contributing

Read [AGENTS.md](../AGENTS.md) first. Its rules on assets, clean-room code and licences are
absolute and override everything here. Then read [architecture.md](architecture.md) and the README
of the crate you will change.

## 1. Setup

| | All platforms |
|---|---|
| Rust | stable ≥ 1.95 (edition 2024; egui 0.36 requires it), via [rustup](https://rustup.rs) |
| wasm target | `rustup target add wasm32-unknown-unknown` (for `cargo xtask wasm`) |
| Components | `rustup component add rustfmt clippy` |
| ffmpeg + ffprobe (optional) | test oracle and fixture generator only; tests skip without it |

| OS | Extra |
|---|---|
| macOS | Xcode Command Line Tools. ffmpeg: `brew install ffmpeg` (includes libx264/libx265). |
| Linux | C toolchain and the system headers cpal and rfd need: ALSA (`libasound2-dev`) and GTK 3 (`libgtk-3-dev`), plus the usual X11/Wayland development packages for winit. ffmpeg from your distribution, built with libx264 and libx265. |
| Windows | MSVC build tools. The oracle tests look for ffmpeg only in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`, so most of them skip on Windows. |

The oracle tests find ffmpeg at `/opt/homebrew/bin`, `/usr/local/bin` or `/usr/bin`. H.264 and
HEVC fixtures need an ffmpeg built with libx264 and libx265. The VideoToolbox fixtures run only on
macOS.

## 2. Build and run

```sh
cargo run --release -p filmcraft                      # desktop app with the demo project
cargo run --release -p filmcraft -- --empty           # start without the demo project
cargo run --release -p filmcraft -- --control 9876    # plus the JSON-lines control server
cargo run --release -p filmcraft -- a.mp4 b.wav       # import media (or open a .fcproj)
cargo run --release -p filmcraft-cli -- commands      # list engine commands
cargo run --release -p filmcraft-cli -- render --demo --seconds 3 --out frame.png
cargo run --release -p filmcraft-cli -- mcp --demo    # headless MCP server on stdio
cargo xtask web --serve 8765                          # the web app on http://127.0.0.1:8765/ (docs/web.md)
```

| Variable | Effect |
|---|---|
| `FILMCRAFT_CONTROL_PORT` | Same as `--control <port>` |
| `FILMCRAFT_CPU_COMPOSITE=1` | Disables the GPU compositor |
| `FILMCRAFT_DATA_DIR` | Same as `--data-dir <dir>`: auto-save, crash recovery, settings and logs |
| `RUST_LOG` | Log levels for standard error and the log file (see [Logs](#logs)) |

`filmcraft --help` lists the app's options. An option it does not know, or a control port that is
not a number (from `--control` or from `FILMCRAFT_CONTROL_PORT`), is an error on stderr with exit
code 2 instead of a window.

Dev builds compile dependencies at `opt-level = 2` and workspace crates at `opt-level = 1`. For
playback and codec speed, use `--release`.

### Logs

The desktop app writes its `log` records to standard error and to `Logs/filmcraft.log` in the data
directory (`~/.local/share/filmcraft/Logs/` or `$XDG_DATA_HOME/filmcraft/Logs/` on Linux,
`~/Library/Application Support/FilmCraft/Logs/` on macOS, `%APPDATA%\FilmCraft\Logs\` on Windows,
or under `--data-dir` / `FILMCRAFT_DATA_DIR`), next to the crash logs; Help ▸ Reveal Log Files opens
the folder. A start from a desktop menu or the Dock has no terminal, so this file is what to attach
to a bug report: GPU compositor fallbacks, audio output problems, panicked frame jobs and container
warnings all land there. Each launch moves the previous log to `filmcraft.1.log` (and that one to
`filmcraft.2.log`), so the log of a run that crashed survives the next start. The file stops growing
at 16 MiB. `--help`, `--version` and command-line errors write no file.

By default FilmCraft's own crates log at `info` and everything else at `warn`. `RUST_LOG` replaces
that with env_logger-style directives, for example `RUST_LOG=debug`,
`RUST_LOG=warn,filmcraft_codecs=trace` or `RUST_LOG=info,wgpu_core=warn`; a directive ending in `*`
covers every target starting with it (`filmcraft*=debug`). The logger is
`apps/filmcraft/src/logging.rs`; the realtime audio callbacks do not log. The web app logs to the
browser console instead.

### Building with craft-fonts

Font assets live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), never in this
repo ([standard](https://github.com/storytold/craftrules/blob/main/standards/fonts.md)). It is an optional build input, not a Cargo
dependency:

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR="$PWD/../craft-fonts" cargo run --release -p filmcraft
```

Use an absolute path: `build.rs` runs in `crates/text`, so a relative `CRAFT_FONTS_DIR` would
resolve from there. `crates/text/build.rs` reads its `fonts/manifest.txt` and embeds the fonts as
`filmcraft_text::fonts::CRAFT_FONTS` (native: all of them; the web build: only BIZ UDPGothic
Regular, to keep the wasm small). The Japanese ones become the last fallback of every UI font family
(`crates/ui-egui/src/theme.rs`, BIZ UDPGothic first) and fallback faces in the text engine (Gothic
for sans text, Mincho for serif text). Without `CRAFT_FONTS_DIR`, `CRAFT_FONTS` is empty and
FilmCraft uses its bundled and system fonts. A `CRAFT_FONTS_DIR` that is not a checkout is a build
warning, or an error with `CRAFT_FONTS_REQUIRED=1` (release builds set both). Tests on these fonts'
glyphs skip when it is empty; when you touch fonts, run the gates both with and without it.

Chinese media names and track labels use the `Hans` / `Hant` craft-fonts faces after the Japanese
fallbacks in every UI font family, regardless of interface language (Japanese and Chinese share code
points, so Japanese text keeps Japanese glyph forms; the Chinese faces cover hanzi the Japanese ones lack). Without those build inputs,
native builds look up an installed Chinese face (for example Microsoft YaHei or Noto Sans CJK SC)
once and reuse it across theme changes. No system font files are bundled or copied into the app.
Web builds depend on the font faces selected by `crates/text/build.rs`; they cannot scan system fonts.
`theme::tests::chinese_names_render_in_every_theme_and_font_family` verifies Chinese glyphs in the
timeline's proportional, monospace, medium and semibold families under all three themes.

## 3. Quality gates

Every commit must pass all of these:

| Gate | Command |
|---|---|
| Format | `cargo fmt --check` (`rustfmt.toml`: `max_width = 160`) |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` |
| Tests | `cargo test --workspace` |
| Layering | `cargo xtask layers` |
| Assets | `cargo xtask assets` |
| wasm | `cargo xtask wasm` (checks every L0–L4 crate, `filmcraft-ui-egui` and `filmcraft-web` for `wasm32-unknown-unknown`) |
| All of the above | `cargo xtask ci` (runs clippy and tests with `--release`) |

Run them with `CRAFT_FONTS_DIR` set too when you change font code ([craft-fonts](#building-with-craft-fonts)).

`cargo xtask` is an alias in `.cargo/config.toml` for `cargo run -p xtask --`.

### Never crash

Production code must never crash: a crash loses someone's work, so this outranks feature work. The rules
are in [AGENTS.md](../AGENTS.md) §0. In short: return `Result<T, E>` instead of `unwrap()` / `expect()` /
`panic!` / `unreachable!` / `todo!` / `unimplemented!`; no `unsafe`; use `get()`, `checked_*` /
`saturating_*` and validated bounds on anything that came from a file, a command parameter or the UI;
bound recursion; tolerate lock poisoning; catch panics in background threads and report them as errors.
Clean crates deny the panicking lints outside tests (`clippy.toml` allows them in tests). Every crash fix
comes with a regression test; parsers, decoders and commands get fuzz / hostile-input tests run under
`catch_unwind`.

## 4. Commits

- One task per commit. The subject starts with the task id: `M3.2: trim mode engine — …`,
  `M9.9: Matroska/WebM import`. Milestones (`M0`–`M16`) are listed in
  [ROADMAP.md](../ROADMAP.md). If your change has no task id, use the area: `docs: …`,
  `README: …`, `ROADMAP: …`.
- Commit only green states (all gates pass).
- When a milestone lands, update its row in `ROADMAP.md` (status, what's done, estimates).
- AI agents end commit messages with the attribution line their environment requires.

## 5. Parallel work (several agents or worktrees)

```sh
git worktree add ../filmcraft-hevc -b hevc-work
cd ../filmcraft-hevc
export CARGO_TARGET_DIR=target/agent-hevc     # separate build dir, no lock fights
```

- One owner per crate at a time.
- The workspace includes `crates/*` and `apps/*` by glob, so one broken `Cargo.toml` breaks every
  build. Keep every manifest valid at all times, including half-finished crates.
- Test fixtures always go to `<repo>/target/fixtures/`, whatever `CARGO_TARGET_DIR` says.
- `.mcp.json` points at `target/release/filmcraft-cli`. With a custom target dir, use your own path.

## 6. How to add…

### A command

1. Add a `cmd!` (action) or `query!` (read-only) entry to `build()` in
   `crates/engine/src/commands.rs`:
   ```rust
   cmd!("sequence.addEdit", "Add Edit", ["Sequence"], Some("Cmd+K"), r#"{"time":ticks?}"#, has_seq, |s, p| {
       let t = time_p(s, p, "").unwrap_or(s.playhead());
       let tg = s.targeting().targeted;
       let n = s.edit_sequence("Add Edit", |q, ctx, _| Ok(edit::razor(q, &tg, t, ctx)))?;
       Ok(json!({"cuts": n.len()}))
   }),
   ```
   - Id: `area.camelCase`, following the menu it appears in.
   - `enabled`: reuse a predicate (`always`, `has_seq`, `has_selection`, `has_in_out`, …) or write
     one that returns `Err("human reason")`.
   - Read time with `time_p` so `time`, `frame`, `seconds` and `timecode` all work.
   - Mutate only through `s.edit(…)` or `s.edit_sequence(…)`, so the change is undoable and
     validated.
   - Put pure timeline logic in `filmcraft-edit` as a function on `&mut Sequence`, with unit or
     property tests there.
2. Add tests to `crates/engine/src/tests.rs`: execute, assert, undo, redo, and check the disabled
   case.
3. Frontend-only actions (tools, zoom, panels) go in `UI_COMMANDS` in
   `crates/ui-egui/src/menus.rs` with a branch in `menus::invoke`.
4. The command now appears in menus (from `menu`), shortcuts, `filmcraft-cli commands`,
   `engine.commands` and MCP `command_list` without further work.

### A video effect

1. Define it in `build_effects()` in `crates/project/src/effect.rs` with the `video(id, name,
   CATEGORY, params)` helper. Parameter helpers cover floats with slider ranges, choices, bools,
   angles, points, colours, curves and wheels. The Effects panel, Effect Controls and agent
   parameter docs are generated from this.
2. Implement it as a match arm in `render::effects::apply` (`crates/render/src/effects.rs`). It
   works on a linear-light premultiplied f32 `Image`. Scale pixel-size parameters by `cx.px_scale`
   so reduced-resolution playback matches full resolution. Unknown ids are a no-op.
3. Add a test in `crates/render/src/tests.rs`.
4. The GPU path needs no change: `render::plan` pre-renders layers with standard effects on the CPU.
   To run it on the GPU too, make it an `FxOp` in `crates/render/src/gpufx.rs` instead (parameter
   evaluation in `FxOp::eval`, the CPU reference in `FxOp::apply`, the id in `GPU_EFFECTS`), add
   its passes to `crates/gpu/src/fx.rs` and its math to `fx.wgsl`, and a parity case to
   `crates/gpu/src/fx_tests.rs`.

Audio effects implement `AudioEffect` in `crates/audio-dsp` and register in its `effects()`
registry. Then add an `audio(…)` definition in `effect.rs` and a `Mapping` from project parameters
to DSP parameters in `crates/render/src/audio_fx.rs`. The DSP must give the same output however the
stream is cut into blocks (there are tests for this).

### A transition

1. Define video transitions with `tr(id, name, FOLDER, badges, params)` in
   `crates/project/src/vtransition.rs` (folders follow Premiere 26: ten `Video Transitions/*`
   folders plus `Legacy/Video Transitions`). Parameters are not animatable. Never rename an id:
   projects store it. Audio transitions are `EffectKind::AudioTransition` defs in `effect.rs`.
2. Implement it in the matching module of `crates/render/src/transitions/` (`wipe`, `motion`,
   `dissolve`, `lights`, `grunge`, `special`): read params through `Tx`, build pixels with `paint`,
   shape wipes with `wipe::field_wipe` (feather/border/anti-aliasing for free) and 3D moves with
   `cards`. Audio crossfade curves go in `transitions::audio_gains`. The property tests run over
   every transition automatically; re-bless `goldens.txt` (see testing.md).
3. It can then be applied with `sequence.applyVideoTransition {"effect": "<id or name>", "params":
   {...}, "reverse": bool}` and edited with `sequence.setTransition`; `effects.list {"folder":
   "Video Transitions/Wipe", "detail": true}` lists it with its parameters.

### A codec or container crate

1. **Spec first.** Work only from the public specification (ITU-T, ISO/IEC, IETF RFCs, SMPTE,
   published container specs). Never copy GPL/LGPL code, and follow AGENTS.md §2 on reading other
   implementations. Name the spec edition in the crate README.
2. Create `crates/<name>` as an L0 crate that depends only on `filmcraft-bitstream` (plus
   `thiserror` and optional `rayon` behind a `threads` feature). Add `[lints] workspace = true`, add
   it to `[workspace.dependencies]`, and add it to `LAYERS` in `xtask/src/main.rs`. It must build
   for wasm32.
3. Test it against ffmpeg as an oracle (see [testing.md](testing.md)):
   - generate fixtures with ffmpeg on first use into `target/fixtures/<crate>/`;
   - skip with a message when ffmpeg is missing;
   - never commit media.
4. Wire it in:
   - decoders: a `VideoDecoderFactory` in `crates/codecs` (`video.rs`, default list in `lib.rs`);
   - containers: an `Opener` in `codecs::openers()`;
   - encoders: an `export::VideoEncoder` / audio encoder plus a `Format` in `crates/export`.
5. Write the README: features, API, test matrix, accuracy and speed numbers, limitations. Use
   `crates/h264/README.md` as the model.

### A panel or widget

- Panels live in `crates/ui-egui/src/panels/` and are listed in `PanelKind`
  (`crates/ui-egui/src/dock.rs`).
- Every interactive element registers an automation id each frame:
  ```rust
  app.auto.add(&format!("effects.item.{}", d.id), rect, d.name);
  ```
  Ids are stable, dot-separated and start with the panel (`timeline.track.V1.lock`, `tools.Razor`).
- UI state that should survive or be scriptable goes in `crates/ui-egui/src/state.rs` (serde), so
  `ui.inspect` and `ui.set` can reach it.
- Project changes go through `session.execute(…)`, never by mutating the project directly.
- Icons are drawn in code in `crates/ui-egui/src/icons.rs`, from scratch (AGENTS.md §1).
- Look at the result: run with `--control`, drive it, take `ui.screenshot`
  ([agents.md](agents.md#3-verifying-ui-work)).

### An asset

The rules are in [AGENTS.md §1](../AGENTS.md#1-assets-no-adobe-artwork-every-asset-licensed-and-attributed).
In short:

- No Adobe iconography, images, fonts, LUTs, presets or other Adobe assets, in any form, including
  traced or "inspired-by" copies.
- Open licences only (MIT, Apache-2.0, BSD, ISC, zlib, OFL, CC0, CC BY, CC BY-SA, or your own
  work under MIT OR Apache-2.0). No NC or ND licences. If the licence is unclear, leave the asset out.
- Screenshots show only FilmCraft (or other open projects) with media we generated or that is
  openly licensed.

- **Fonts live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not here.**
  Don't commit new font files to this repo (the Latin UI fonts already in `assets/fonts/` stay): add
  the font to craft-fonts (file, licence, manifest line, `ATTRIBUTION.md` row) and use it through
  `filmcraft_text::fonts::CRAFT_FONTS`. See [Building with craft-fonts](#building-with-craft-fonts)
  and [`craftrules/standards/fonts.md`](https://github.com/storytold/craftrules/blob/main/standards/fonts.md).

For each asset file `X`, commit a sidecar `X.attribution`. This is the real one for
`assets/fonts/Inter-Regular.ttf`:

```text
asset:        Inter-Regular.ttf
title:        Inter Regular (UI typeface)
author:       The Inter Project Authors (Rasmus Andersson et al.)
source:       https://github.com/rsms/inter
license:      OFL-1.1
license-file: assets/fonts/OFL-Inter.txt
added:        2026-09-30 by Claude (agent) for Brandon Thomas
notes:        Unmodified font file, embedded in the UI via include_bytes! (crates/ui-egui/src/theme.rs).
```

For your own work, use `source: original work` and the project licence (`MIT OR Apache-2.0`).

Then add a row to [ATTRIBUTION.md](../ATTRIBUTION.md) with the path in backticks. `cargo xtask
assets` checks every tracked file with a media extension (images, fonts, LUTs, audio, video, PDF,
project files…):

- the sidecar exists;
- `asset`, `title`, `author`, `source`, `license` and `added` are filled in;
- the licence is not NC, ND, Adobe or proprietary;
- the path appears in `ATTRIBUTION.md`;
- the path does not mention Adobe or Premiere;
- no sidecar is orphaned.
