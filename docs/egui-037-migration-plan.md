# FilmCraft — egui 0.37 migration plan

Current reconciliation (2026-10-25): the local tree incorporates main
`a504f3c5adc4cd60d9c1120916cddb50c9b24b86`. Managed CI
`egui037-filmcraft-oracle8-full-ci-20261025-0871` passed formatting, strict lint,
workspace tests and doctests, assets, layers and the 49-crate WASM check.
It recorded 3,756 passing test executions, zero failures and 45 ignored across
164 summaries. Required external oracles used the retained checksum-pinned
FFmpeg 8.1.3 toolchain and fresh generated fixtures. The source patch matched
the completed run before this prose update. Native/browser GUI, native IME,
platform runtime, final packages and paired performance remain open. Human
review and human merge are required.

The following landing record qualifies the earlier candidate; its test counts
and captured main are historical.

Historical landing status, 2026-10-25: the migration integrates main `bfc23840b603e69c782302c6ed04d4a9f6ec26a8` and uses egui `6d518fc799681571963842f95b55a6b68431b15a`, winit `a517b3846ee8457d84ee4975110bfda6797c44dc` and craft-ui `25da1314d443171b4adeaad2cd9454b273f35c6f`. Cargo.lock SHA-256 is `8ddb2d92ec359928491b47c084501bbecaa9ff708d678156dab99d1cbd24642e`; the independent media/document-path pins remain selected in the lockfile. Release workflows use craft-fonts `b076417b5880bd4154bd22f00695b1d28c6bea83`.

Fresh static review verified preservation of the captured main and the final dependency graph. The final managed CI run `egui037-filmcraft-oracle8-full-ci-20261025-0570` passed formatting, strict lint, workspace tests and doctests, layers, assets and the 49-crate WASM check, including the web app. It recorded 3,733 passing test executions, zero failures and 45 ignored across 164 summaries. Earlier passing counts below qualify their historical snapshots only. The icon-font attribution URL now identifies the selected egui source; font bytes are unchanged. Visible GUI, browser/platform runtime, native IME, final packages and paired performance remain separate acceptance gates. Human review and human merge are required.

External oracle checks ran with `FILMCRAFT_REQUIRE_ORACLES=1`, checksum-pinned FFmpeg 8.1.3 (`n8.1.3-24-g92611900bb`) and fresh generated fixtures. An earlier run using FFmpeg 7.1.5 failed ten codec/oracle/workflow tests. All ten passed unchanged with the new tools in managed job0567, before the full suite passed in0570; no decoder, tolerance or fixture-source changes were made to clear them. The newer tool provides the required APV, half-float, SVT-AV1 and Kvazaar capabilities. It remains a private external oracle, not an app dependency or distributed asset.

The original planning and earlier qualification record follows. Its pins, hashes, paths and passing counts are historical.


Status: local implementation revision after the independent adversarial reviews of
`plan-reviews/filmcraft-v1.md` and `filmcraft-v2.md`. The owned checkout now contains the
0.37 migration work; commits, pushes, publications, and final immutable pins/lock remain
gated. This document is the implementation contract, not publication acceptance. Every
statement about runtime behavior below is source inspection at the recorded revisions unless
it is explicitly labelled a proposed test or a proposed command.

This revision resolves the review findings inline, including the dependency DAG. Section 17
is a disposition index only; it does not override any section.

## 1. Scope and exact baseline

| Item | Value |
|---|---|
| Repository | `storytold/filmcraft` |
| Assigned checkout | `/home/codex/work/CraftApps/egui037-migration-20261022/filmcraft-implementation` |
| Latest qualified tree | HEAD `4ce159e1cda036bbc972d6d27d125626ff714b98` (2026-10-25; merge of migration `df5f86ae` and main `bfc23840`), with main `a504f3c5adc4cd60d9c1120916cddb50c9b24b86` merged locally but uncommitted; `Cargo.toml` SHA-256 `d544c4368d222e79e34be0c681b3b34f435109903b4f85f1ccc4ff27e35b8efb`; `Cargo.lock` SHA-256 `8ddb2d92ec359928491b47c084501bbecaa9ff708d678156dab99d1cbd24642e` |
| Planning-snapshot branch HEAD (historical) | `fa550ce2d07bdfee6b1d7b6835e8abc8c6913d00` (2026-10-24; migration work is uncommitted on this base) |
| Planning baseline (historical) | `abf44fda9015f4b7a96dc338f6042499bf8e9d9f` (2026-10-22, "Reject non-hex characters in colour hex parsing (#2600)") |
| `Cargo.toml` SHA-256 (planning snapshot) | `b41244b83b0aff71ab6e122826d20a49a2d59c35e87da57c8dfd228a962ec2cf` |
| `Cargo.lock` SHA-256 (planning snapshot) | `67528fa8486c963dadbaf04e487ab5a8af59ac8f98dac97e1d51f40b953b5bb5` |
| Workspace | resolver 3, edition 2024, Rust 1.95, 50 crates plus 3 apps and `xtask` |
| Planned UI stack (planning snapshot) | `egui`, `eframe`, `egui-wgpu`, `egui-winit`, `egui_glow`, `egui_kittest`, `emath`, `epaint`, `epaint_default_fonts` at `0.37.0`; `winit 0.30.13`; `wgpu 30.0.1`; `accesskit 0.24.1`; `arboard 3.6.1` |
| Shared UI dependency | Planning manifest (historical): `craft-ui` at `5c874d51eeb3de70e735c8ce87a59a7dedd47675`, `craft-media` at `f0ad6c1e1d297908915f4be315b0fcfc9be7b5d4`, `craft-docpath` at `82a24b006eefa036b43c09a67fb9a3fbd17ffe63`. Reconciled PR #199 base: `3832c47ef68a0e6214f2c1e85f3526b6adcb686e`; final immutable pins and lock remain G-PINS/G-CL2F/G-CRAFT-LIBS gates. |
| Target release | `egui 0.37.0` = upstream commit `841a328ba2dd8f6996beb7189ebb7fc00e668a2a` |
| Research candidate | `aiongg/egui` `craft/0.37` = `a4f9a17f64a6bae61350fb85804c9df0d9865916`; `aiongg/winit` `craft/0.30.13-lang` = `8b35388bd719e8cc86a95971ba0bc026c9a8f058` |
| Production fork pins | **Unknown and deliberately not invented.** They become valid only after reviewed, published immutable `storytold/egui` and `storytold/winit` commits exist. See §3.2 and §15 gate G-PINS. |
| Evidence directory | `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022` |
| Historical diagnostics | `/home/codex/work/ssd2/task-evidence/egui037-audit-20261022/{fork,stock}/filmcraft.errors` — older checkout (FilmCraft manifest 0.10.0), clues only, never acceptance |

The existing `App::logic` / `App::ui` split and root-`Ui` architecture are **not** introduced
by 0.37 and must not be rewritten as if they were. This plan changes where frame side effects
run and how input is handled across layout passes; it does not change the command, undo,
project or persistence model. Do not add a FilmCraft-specific `egui` compatibility crate. Use
the new APIs directly, and add a helper to `craft-ui` only for semantics that at least two
consumers need with the same contract.

This repository has:

- one root workspace (`Cargo.toml`) with `members = ["crates/*", "apps/*", "xtask"]`;
- one workspace lockfile (`Cargo.lock`, 708 packages);
- no `[patch]` section, no `vendor/` directory, no source replacement and no local crate copy;
  `.cargo/config.toml` contains only the `xtask` alias;
- exactly four manifests that mention `egui`: the workspace, `crates/ui-egui/Cargo.toml`,
  `apps/filmcraft/Cargo.toml` and `apps/filmcraft-web/Cargo.toml`;
- a native entry point (`apps/filmcraft/src/main.rs`), a wasm entry point
  (`apps/filmcraft-web/src/lib.rs`), a CLI app and a web app;
- no `egui_plot`, `egui_extras` or `egui_dock`; all other crate manifests scanned for `egui`,
  `eframe`, `winit`, `rfd` and `egui_*` contain none;
- `crates/ui-egui/Cargo.toml` has the only relevant target cfgs: `rustix` for Unix
  thread/process clocks and `cpu-time` for Windows;
- `craft-libs` vendors `krilla`/`hayro` for PDF work, unrelated to the windowing stack; this
  plan does not touch them.

### 1.1 Reverse dependency: EffectCraft

EffectCraft (`9581b53c3f8945683252af0eb5fac79e0e43aed4` in the owned checkout) pins fifteen
`filmcraft-*` crates to `2c5ba7a72619935f0481f99d59409e0d1844b234` in `effectcraft/Cargo.toml`.
That revision is not an ancestor of FilmCraft baseline `abf44fda`; it is reachable from
`origin/effectcraft/webm-alpha`. None of those crates (`media`, `codecs`, `frame`, `color`,
`time`, `project`, `interchange`, `geom`, the encoders/decoders and the muxer) contains
`egui`, and EffectCraft does not depend on `filmcraft-ui-egui`.

Decision: EffectCraft needs no change for this migration. Retain its separate branch pin.
Re-audit only if (a) the pin moves to FilmCraft main or another revision that changes a
shared public type, or (b) EffectCraft starts consuming `filmcraft-ui-egui` (directly or
through another crate). The re-audit must check the actual Cargo identities, not just a
version string. This is the required explicit cross-repository check; it is not a silent
deferral of FilmCraft's own compatibility work.

## 2. Current architecture and lifecycle

### 2.1 Entry points

| Target | Entry point / symbol | Notes |
|---|---|---|
| Native desktop | `apps/filmcraft/src/main.rs:80` `main`, `:139` `eframe::run_native`, `:168` `native_options`, `:190` `app_creator` | wgpu only; features `default_fonts`, `wgpu`, `accesskit`, `wayland`, `x11`; no `system_fonts`, no `links` |
| macOS native | `apps/filmcraft/Cargo.toml` target deps `muda`, `objc2-*`, `winit`; `main.rs:430` `agent_event_loop`; `native_menu.rs` | custom titlebar and menu; `--control` must not activate the app |
| Linux native | `apps/filmcraft/Cargo.toml` `zbus`; `main.rs` `run_dialog_off_thread` | X11 and Wayland at runtime; XDG portal dialogs |
| Windows native | `apps/filmcraft/Cargo.toml` `winresource`; `graphics.rs` DX12/default/software ladder | DX12 FXC shader compilation is relevant to startup measurement |
| Browser | `apps/filmcraft-web/src/lib.rs:125` `start`, `:156` `WebRunner::start`, `:92` `impl eframe::App for WebApp` | wasm32 only; WebGPU with WebGL2 fallback; WebCodecs; OPFS |
| Headless UI | `crates/ui-egui/tests/scripted.rs` and the other `tests/*_ui.rs` `Harness::builder().build_eframe(...)` | `egui_kittest` drives the same `FilmcraftApp` |

### 2.2 The app object

`FilmcraftApp` is in `crates/ui-egui/src/lib.rs`. The UI code is already split:

- `crates/ui-egui/src/lib.rs:2133` `impl eframe::App for FilmcraftApp::logic`;
- `crates/ui-egui/src/lib.rs:2193` `impl eframe::App for FilmcraftApp::ui`;
- `crates/ui-egui/src/lib.rs:1807` private `FilmcraftApp::frame(&mut self, ui: &mut egui::Ui)`,
  called from `App::ui` and wrapped in `catch_unwind`;
- `cargo xtask` only builds/tests; FilmCraft does not implement a second `update` path;
- the main workspace is custom-drawn: `App::ui` receives the root `Ui`, then calls
  `header::show`, `dock_area`, `panels::*` and dialog functions. It does not use
  `egui::TopBottomPanel`, `SidePanel` or `CentralPanel`;
- floating panels are `egui::Area`s managed by `crates/ui-egui/src/dock.rs` and `dock_ui.rs`,
  not native viewports. There is no `show_viewport_deferred` and no child-window dock;
- dialogs use `egui::Window::new(...).show(ctx, ...)`, valid only inside a pass.

### 2.3 Where side effects live (exhaustive inventory)

These are the behaviours that must survive hidden logic-only calls, forced hidden paint and
multi-pass layout without duplication. All clock-driven entries use a monotonic host clock
after migration; no hidden work may read `ctx.input(|i| i.time)`.

| Effect | Current location | Required migration decision |
|---|---|---|
| Theme/font installation and language fallback | `lib.rs:2133` `logic` (`theme::install`, `i18n::install_japanese_font`, `chinese_font_available`, `korean_font_available`) | Keep in logic; once-per-process state, not per pass; never call `Context::root_ui` |
| Control channel, deferred requests, input waiters | `lib.rs:1672` `drain_control`, retry deadlines `:1682`, `now - last_ui_time > 0.25` raise heuristic `:1679` | Logic; consumed `try_recv` queue; monotonic `now`; `ui.*` requests that need composition send `ViewportCommand::RequestPaintWhileHidden` (see §4.5) |
| Screenshot settle / issue / collect | `lib.rs:1702` settle timestamp, `:1714` `issue_screenshots`, `:1744` `collect_screenshots`; `queued_screenshots` `*frames += 1` (`lib.rs:1717-1720`), `monitor_inexact` `mem::take` (`:2182`), `timeline_still` increment (`:2163`) | Logic; monotonic deadline; `ScreenshotCallback` plus the CL-2 completion slot. Settle counters advance once per rendered frame/pass identity, not per logic call; a queued-but-not-yet-issued hidden request repeats `RequestPaintWhileHidden` until issued, after which the screenshot command itself forces the paint. |
| FPS, frame time, close interception | `lib.rs:2133` `logic` (`ctx.input(i.time)`, `visible()`, `close_requested()`) | Logic; monotonic host clock; viewport metadata is fresh in `run_logic`, input time is not |
| Frame worker pump | `lib.rs:2193` `ui` calls `self.frames.pump(...)` | Logic/background tick; do not rely on paint |
| Persistence polling | `lib.rs:1825` `frame` calls `session.poll_persistence()` | Logic; once per state transition, not per pass |
| Engine event drain | `lib.rs:1826` `session.drain_events()` | Logic; drain the one-shot queue; an event is consumed exactly once regardless of pass count |
| Toast timestamp | `lib.rs:1840` `Event::Toast` stamps `ctx.input(i.time)` while `drain_events` moves to logic | Stamp with the monotonic host clock; repeated logic calls must not re-time or duplicate the toast |
| Media refresh and pool sync | `lib.rs:1822` `sync_pool`, `:1825` `tick_media_refresh` (`:1777`) | Logic; monotonic deadlines and `request_repaint_after`; consumed scan state |
| Preferences application | `lib.rs:717` `apply_prefs`, called from `frame` | Logic or known UI pass 0; idempotent; font/visual side effects once |
| Drop import | `lib.rs:1473` `handle_drops` | `App::ui` pass 0 only; uses `RawInput` events; never `run_logic` |
| Native menu / command inbox | `lib.rs:1846` command inbox and `menus::invoke`; `:1860` `sync_native_menu` | Command inbox in logic (consumed channel); native menu sync in UI pass 0; `menus::invoke` must not run twice per frame |
| Keyboard shortcuts and focus/IME | `lib.rs:1546` `handle_shortcuts` | `App::ui` pass 0 only; `consume_key` remains the anti-duplication mechanism; fork E19 Latin-layout handling |
| Playback/source playback/scrub audio | `lib.rs:1128` `advance_playback`, `advance_source_playback`, `scrub_audio` | Logic; monotonic host clock; set playhead from anchor + elapsed, never accumulate per call |
| Undo press transaction | `lib.rs:2000` `begin_press` | `App::ui` pass 0 before UI; one press transaction per user gesture |
| Escape drag rollback | `lib.rs:2011` `cancel_value_drag` | Latch Escape and the press mark in UI pass 0; apply rollback exactly once after the final UI pass (§4.4) |
| TTS selection following | `lib.rs:1865` `panels::tts::follow_selection` | Logic or UI pass 0; idempotent selection identity |
| Job watch / auto-play after render | `lib.rs:1916` `watch_jobs` | Logic; consumed job results; status-bar drawing remains in UI |
| Auto-save dirty poll | `lib.rs:1818` `session.is_dirty()` and `request_repaint_after` | Logic; monotonic deadlines; worker stays alive while hidden |
| Dynamic trim / loop playback (`trim.tick`) | `lib.rs:1817` `panels::trim_monitor::advance`; `panels/trim_monitor.rs:39-48` executes `session.execute("trim.tick", {"clock": ctx.input(i.time)})` on every frame | Move to logic; monotonic host clock; compute an idempotent target time (same target called twice is a no-op), not one tick per pass; keep `request_repaint` |
| Metadata selection follow | `lib.rs:1814` `panels::metadata::follow_selection` | UI pass 0 or idempotent logic state; follows selection identity |
| Auto/undo frame bookkeeping | `lib.rs:1813` `self.auto.begin_frame()` | UI pass 0 once; no second call in a discarded pass |
| Timeline view sync | `lib.rs:1844` `self.sync_timeline_view()` | UI pass 0; idempotent |
| Native menu sync | `lib.rs:1860` `self.sync_native_menu()` | UI pass 0; idempotent |
| `last_ui_time` update | `lib.rs:2213` `self.last_ui_time = ctx.input(i.time)` | UI pass 0; monotonic host clock; drives the control raise heuristic and screenshot settle heuristics |
| Input waiters drain | `lib.rs:2193` `App::ui` tail and synthetic queue | UI pass; consumed waiter vector; respond once |
| Synthetic input / control input | `raw_input_hook` `lib.rs:2092`, synthetic queue | `raw_input_hook` runs before every `run_ui` frame and before `update_logic_only` ticks. Keep one unapplied synthetic gesture step in app state across repeated hidden logic ticks until a full UI pass consumes it; never collapse a press into its release or inject the same step twice. |

Additional deadline/side-effect calls found by the review and covered by this table:
`drain_control`'s `now + 3.0` retry deadline, its `now - last_ui_time > 0.25` raise decision,
`issue_screenshots`'s `settle` and timeout, `tick_media_refresh`'s persisted `last` timestamp,
the auto-save `request_repaint_after(1s)`, TTS `request_repaint_after(100ms)`, the synthetic
input repaint, and the `monitor_inexact`/`timeline_still` settle counters. None may use stale
`ctx.input(i.time)` after the lifecycle split; the toast stamp and settle counters have explicit
rows above.

### 2.4 Actual multi-pass exposure

- FilmCraft does not call `Context::request_discard` (verified by grep).
- egui 0.37 components used by FilmCraft can request it: `Grid` on first show
  (`crates/egui/src/grid.rs`, "new Grid" path), `Aligned` on size change
  (`containers/aligned.rs`), and `Resize` on a sizing pass (`containers/resize.rs`). FilmCraft
  uses `Grid` in many dialogs, `ResizeHandle` through `craft_ui::layout`, and `Aligned` only if
  a dependency introduces it.
- Both native `crates/eframe/src/native/epi_integration.rs::update` and web
  `crates/eframe/src/web/app_runner.rs` call `app.logic` **and** `app.ui` inside the
  `Context::run_ui` closure. Both call logic once per layout pass, not once per frame.
  When the root window/tab is hidden and no paint is requested, native
  `update_logic_only` and the web runner call `Context::run_logic` with no UI pass.
- `run_ui` may run a second pass after a discard. `RawInput::take()` moves `events` out, so
  pass 1+ receives an empty event list; `InputState` recomputes pointer clicks and
  `key_pressed` only from `new.events`. A click or key seen in pass 0 is therefore not
  re-derived in pass 1.
- After a normally completed `run_ui`, `current_pass_index()` is **zero**, including after
  multiple passes: `end_pass` takes the output. Do not claim or test a stale nonzero index.
  A pass-index first-pass guard is valid only for genuinely once-per-layout-frame UI state
  under this pinned integration, and needs a regression proving hidden calls still run. It
  must not suppress fresh asynchronous results, control commands or widget actions merely
  because a later pass has a nonzero index.
- `InputState::viewport().visible() == Some(false)` does **not** prove that the current
  callback has no fresh input. A pending screenshot or `RequestPaintWhileHidden` can force a
  real UI pass while minimized/occluded, and that pass can carry fresh input. Do not infer a
  "logic-only" phase from visibility.

## 3. Dependency and pin strategy

### 3.1 Shared `craft-libs` baseline — corrected facts

- The reconciled shared-library base is the merged PR #199 squash
  `3832c47ef68a0e6214f2c1e85f3526b6adcb686e` on `storytold/craft-libs`. The implementation
  checkouts use that base; `49190884278f16c9a9f5823d7698a8ffb5deb062` and its verification
  below are historical planning evidence.
- GitHub `repos/storytold/craft-libs/branches/main` returned
  `49190884278f16c9a9f5823d7698a8ffb5deb062` at approximately 06:25 UTC on 2026-10-23. This was
  the shared-library baseline at planning time.
- The local `origin/main` ref (`b1401e3`) is stale. A stale remote-tracking ref **does not
  prove** that `4919088` is unpublished. The review's "unpublished local/audit branch" claim
  is rejected; the corrected evidence is the GitHub main verification above.
- FilmCraft's current manifest pins `craft-ui` at
  `5c874d51eeb3de70e735c8ce87a59a7dedd47675`, `craft-media` at
  `f0ad6c1e1d297908915f4be315b0fcfc9be7b5d4`, and `craft-docpath` separately at
  `82a24b006eefa036b43c09a67fb9a3fbd17ffe63`. The old `5c874d51` commit was fetched exactly
  into the owned `craft-libs` checkout for inspection without changing HEAD or any source.
  These manifest pins are not the final publication pins.
- Between `5c874d51` and `4919088`, `crates/ui` changes seven files, **607 insertions and two
  deletions**: `README`, `docking/model.rs`, `docking/surface.rs`, `docking/tests.rs`,
  `lib.rs`, new `printing.rs` and `printing/tests.rs`. `Cargo.toml` is unchanged.
  - The docking change is the protected-group fix: `MoveFloating` now requires every
    floating-group member to be movable, including the resize grip. Preserve the tests from
    `d66e345`, `fd9cf54` and `c67b1e1`; this intentional security behavior must not be
    reverted to match the old FilmCraft baseline.
  - Printing is additive and lazy. Enabling the updated crate must not start discovery at
    startup. Relevant print commits include `b700ae7`, `d27646a`, `4cae45d`, `2bbef53` and
    `4f85efb`.
- `craft-docpath` has no `egui` dependency. Keep it at `82a24b0` unless a later actual
  cross-type or feature reason requires the move; do not fold it into the `craft-ui` bump.
- PR 199 is **not** mergeable as a tree. Its head `b68de8c` and historical audit tree
  `ec8840a` are stale relative to `4919088`, omit current `craft-platform`, and revert
  security, settings, docking and printer fixes. Reuse only the API port hunks from
  `8614e4a04fae516fba16c22b31ff1008cb6be10b` and the relevant README changes from
  `1764bca`, applied onto current `4919088`. Preserve intervening fixes and provenance.
  Never merge `b68de8c` or `ec8840a` as a tree.
- The shared 0.37 port and the CL-2 capture helper will produce a later published
  `craft-libs` revision. FilmCraft's final compatibility review must compare its actual old
  API/use sites against that final published revision, not against `b68de8c` or a branch
  point.

### 3.2 One coherent production graph, not a stock-0.37 stage

Stock 0.37 is a **local comparison** and CI control only. It is not a landable production
stage. The production landing uses one coherent graph: the final reviewed immutable
`storytold/egui` family, the final reviewed immutable `storytold/winit`, and the matching
published `craft-libs` revision. The previous coherent 0.36.2 graph remains the rollback
target.

The exact production pins are **not known yet** and must not be invented. `a4f9a17f` and
`8b35388b` are aiongg research candidates and comparison sources, not approved production
pins. Gate G-PINS (§15) blocks the entire landing stage until:

1. the complete reviewed/published `storytold/egui` P8 commit is immutable: E1-E21,
   P2b bounded async W15P routing, P7 held-key pairing, P7b/E21b, P7c selected #8763/#8748
   fixes and justified performance fixes;
2. the completed reviewed/published `storytold/winit` commit contains the bounded W15P
   transfer path, the W27 owner/support lifecycle and the required W1-W27 behavior;
3. the matching published `storytold/craft-libs` commit contains CL-1 (the PR-199 API hunks
   and one coherent fork graph) and the frozen CL-2F helper;
4. the exact SHAs, feature sets, provenance and test mappings are recorded in the suite pin
   manifest and the ledger.

Until then, no `[patch]` block in FilmCraft names a fabricated or guessed revision. The
eventual block must name the same immutable revision for every egui-family package that can
resolve, plus `winit`; a patch section inside `craft-libs` does not propagate into this root.
`egui_extras` is unused. `egui_plot` does not exist in FilmCraft; do not add it because its
version numbering differs. `winit` stays exactly `0.30.13`.

The final production manifest will look like the following shape, with `REPLACE_WITH_*`
resolved only after G-PINS:

```toml
[patch.crates-io]
egui = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
eframe = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
egui-winit = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
egui-wgpu = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
egui_glow = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
egui_kittest = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
epaint = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
emath = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
ecolor = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
epaint_default_fonts = { git = "https://github.com/storytold/egui", rev = "REPLACE_WITH_APPROVED_EGUI_SHA" }
winit = { git = "https://github.com/storytold/winit", rev = "REPLACE_WITH_APPROVED_WINIT_SHA" }
```

If a stock-0.37 comparison is needed, it lives in a separate local/CI branch or harness with
its own lockfile and evidence. It is never merged to main as an intermediate production
state, and it does not change the production pin policy.

### 3.3 Capture-helper sequence (CL-2 -> adapters -> CL-2F -> consumers)

The suite capture contract is one completion slot per admitted request: the app holds it
strongly and the callback holds it weakly.

1. **CL-2** in `craft-libs` (`crates/ui/src/capture.rs`): provisional bounded completion-slot
   prototype and tests. The app owns the request ID, path, deadline, settling policy,
   capture-frame geometry, DPI/scale identity and reply schema.
2. **DesignCraft and GridCraft adapters** against that exact provisional revision. DesignCraft
   exercises multiple viewports; GridCraft exercises a single window. Their tokens, paths,
   settle delays, deadlines and replies remain app-owned.
3. **CL-2F** freezes the helper API after joint review of those adapters.
4. **FilmCraft and the other consumers** pin the published revision containing CL-1 and
   CL-2F in one atomic dependency stage, not a branch head and not a path dependency.

All fourteen capture consumers adopt this contract. The eleven apps that already declare
`craft-ui` (CADCraft, DesignCraft, EffectCraft, FilmCraft, GridCraft, LightCraft, PdfCraft,
PhotoCraft, SoundCraft, VectorCraft and WordCraft) import the helper from the published
revision that contains both CL-1 and CL-2F in the same atomic dependency stage. DesignCraft
and GridCraft are the pilot adapters against the reviewed CL-2 provisional revision before
CL-2F freezes the API. DeckCraft, FigCraft and ArtCraft Native do not declare `craft-ui`;
they use contract-compatible local adapters unless they choose to add the dependency. Any
consumer that already carries an incompatible `craft-ui` bump must reconcile it in that same
atomic dependency stage, not a separate registry-only state. The earlier "thirteen `craft-ui`
consumers" wording was based on substring matches and is superseded by the exact TOML-key
inventory.

The helper contract, which every adapter must satisfy:

- the app's live request record owns the completion slot and a strong viewport waker; the
  callback holds only a `Weak` reference to that slot, never a strong `Context` or app
  reference and never a cycle back into the owning context. `ScreenshotCallback::new` does
  not request a new frame when data arrives (`egui/src/data/screenshot_callback.rs:22-24`),
  so the live slot's waker must request the repaint after the slot lock is released;
- the request record owns one slot; correlation is by ownership, not callback order or a
  second global token map;
- admission uses the shared defaults of **four outstanding requests, 128 MiB per image and
  256 MiB aggregate reserved in-flight/completed bytes**; a stricter documented app cap is
  permitted (the shared SoundCraft example is 64 MiB/image and 192 MiB aggregate with the
  same four-request count). Check the count and aggregate budget before issue, reserve
  checked `width * height * 4` bytes, and do not evict an earlier admitted request;
- record the target-UI logical bounds and a geometry epoch at issuance and bind the returned
  pixels to that epoch. Cancel/retry on any intervening resize, DPI, zoom or surface-epoch
  change before save, **even a change back**; with an unchanged epoch derive pixel scale
  from the returned image dimensions and verify aspect/rounding. A callback carries pixels
  only, not capture metadata;
- cancel, close, drop and late callbacks release the image and send no reply/file; release
  the slot lock before waking the owning viewport. The callback has no repaint request of its
  own; the live slot's waker is the only wake path and runs after unlocking;
- the app keeps the request ID, output path, serialization, timeout, settling and reply
  schema; the callback does no PNG encoding, file I/O, command execution or long locking;
- retained image bytes return to zero after drain/shutdown; a post-hoc size check does not
  substitute for pre-issue accounting.

### 3.4 Lockfile and reachability checks

Before publication, run these against a clean checkout and a fresh `CARGO_HOME` (or with
`--locked` after fetching). These are proposed checks, not executed here:

```sh
cargo metadata --locked --format-version 1 > /dev/null
cargo tree -i egui@0.36.2        # must report no match
cargo tree -i egui               # exactly one 0.37.0 source
cargo tree -i winit              # exactly one pinned source
cargo tree -d -i winit           # no duplicate winit
cargo tree -d -i wgpu            # one resolved wgpu source/version
cargo tree -e features -i egui | grep -E 'default_fonts|monochrome_emoji_fonts|clipboard'
```

Add a CI/`xtask` check that fails if:

- an egui-family package has a source other than the chosen immutable fork;
- `winit` has more than one source or a version other than 0.30.13;
- a published manifest contains `path = "/home/`, `path = "/Users/` or an uncommitted local
  checkout;
- a git pin changes revision without the same change in the suite pin record;
- the required `clipboard` or `monochrome_emoji_fonts` feature is absent from the selected
  native/egui feature set.

The lockfile is committed. A fresh clone with `cargo fetch --locked` is the publication
reachability test.

### 3.5 Feature policy

The current feature lists are explicit. Migrate to 0.37 with these decisions:

- workspace `egui`: `default-features = false` and features `["default_fonts",
  "monochrome_emoji_fonts"]`;
- native `apps/filmcraft`: keep `default_fonts`, `wgpu`, `accesskit`, `wayland`, `x11`, and
  **add `clipboard`**. Without `clipboard`, eframe 0.37's `egui-winit` uses the in-app
  fallback clipboard, so cross-application text copy/paste silently stops working;
- `crates/ui-egui`: keep `default_fonts`, `wgpu`, `accesskit`; the native app root is
  authoritative for OS clipboard;
- web `apps/filmcraft-web`: keep `default_fonts`, `wgpu`; the eframe `clipboard` feature is
  native (`egui-winit`) and is not added to wasm. Verify browser clipboard behavior in the
  browser workflow instead;
- do **not** add `system_fonts`; do **not** add `links` or `web_screen_reader` by accident.
  Record them as deliberate opt-outs in the PR (`links` is not used; `web_screen_reader` is
  a separate speech/accessibility decision, not part of this migration). Revisit only with
  measured evidence.

`monochrome_emoji_fonts` is not implied by `default_fonts` in 0.37. It must be explicit;
otherwise the bundled `NotoEmoji-Regular`/`emoji-icon-font` faces disappear and symbols such
as `✔ ✓ ✖ ⚡ ⚠ ⚙ ♪ ☑` can become tofu or switch to a platform face.

These feature decisions apply to production roots. They do **not** flow through
`egui_kittest`: its normal `egui` dependency is `default-features = false`, and only its own
dev-dependencies enable fonts, which do not propagate. Test roots must enable the features
explicitly or install controlled fonts (§11.1).

## 4. Lifecycle and root-Ui design

### 4.1 Rule: logic is idempotent background; UI is composition; input is pass-0 only

Introduce private `FilmcraftApp::run_logic_frame(&mut self, ctx: &egui::Context)` and
`FilmcraftApp::run_ui_frame(&mut self, ui: &mut egui::Ui)`:

- `App::logic` runs `run_logic_frame` on **every** call. Under this pinned eframe/egui
  integration, native and web call `App::logic` once per `run_ui` layout pass, and also call
  it through `run_logic` for hidden ticks. Therefore `run_logic_frame` must be safe to call
  more than once per frame:
  - background queues are consumed (`try_recv`, `drain`, `Option::take`), so a second call
    sees no work;
  - playback/trim/autosave/deadline work compares a monotonic host time and computes an
    idempotent target; calling it twice at the same instant does not advance twice;
  - engine events are drained once and processed as values, not replayed;
  - no logic path reads UI input events, `consume_key`, `Response`, or stale input time.
- `App::ui` runs only `run_ui_frame`. Fresh-input handling (drops, shortcuts, `begin_press`,
  Escape, IME/focus, synthetic queue, screenshot request dispatch) runs in **pass 0** of a
  UI pass, where events are fresh. Pass 1+ has no events and must not re-derive them.
- `raw_input_hook` runs before each UI frame **and** before each `update_logic_only` tick
  (eframe `prepare_raw_input`). It must therefore advance at most one synthetic gesture step
  per call and leave that step in app state until a full UI pass consumes it; repeated hidden
  logic ticks must not drain a whole press/release sequence or inject it twice.
- `run_logic_frame` is wrapped in `catch_unwind` with a latch: an unwinding logic pass sets
  `ui_error`, stops playback and does not re-run the same panicking code every tick or
  re-log it every pass. `run_ui_frame` keeps its catch around `frame(ui)` and the graphics
  notice.
- `on_exit` stays in `FilmcraftApp` and calls `session.shutdown()` exactly once.

No `ui_pass`/`hidden_tick` predicate based on `viewport.visible()` is used. Visibility is
used only for presentation policy (for example whether a hidden window should be raised),
never to infer that the current pass has no input or that background work is complete.

### 4.2 Hidden work and monotonic time

`Context::run_logic` leaves `InputState` events and time at the previous pass. Therefore:

- all background code uses a monotonic host clock (`web_time::Instant` on all targets, or a
  small `Clock` wrapper around it); no hidden work uses `ctx.input(|i| i.time)`;
- background code never reads `i.raw.events`, never calls `consume_key`, and never polls
  `Response`/`read_response`;
- input-dependent work is in `App::ui` pass 0, not in logic and not inferred from a
  visibility flag;
- `Window::show`, `Area::show`, and any other widget construction run only inside a UI pass;
  they are never called from `run_logic_frame`;
- control `ui.*` requests and gesture stages that need composition while hidden use
  `ViewportCommand::RequestPaintWhileHidden` (see §4.5). A screenshot request already causes
  the needed paint.

A regression test exercises visible single/multipass, repeated hidden logic ticks, a forced
hidden UI paint, restore with buffered events, a background completion between passes, and
one gesture yielding one command/undo.

### 4.3 Do not use `Context::root_ui` recursively

`Context::root_ui` panics if called while `App::ui`/`Context::run_ui` already holds the root
`Ui` (upstream #8535). FilmCraft does not need it. Keep passing the existing `&mut egui::Ui`
to `header::show`, `dock_area`, `panels::*` and dialogs. Where a helper takes
`&egui::Context` only to add widgets, change it to take `&mut egui::Ui` (or return an
action/shape), not a `Context` bridge.

### 4.4 Multi-pass actions: no clearing on a no-event pass

Immediate mode plus `request_discard` means a UI handler can run in a pass whose output is
discarded. FilmCraft's project mutations must run once, and must not be lost.

Design:

- Add a frame-scoped `pending_ui_actions` queue and an `input_generation` counter in
  `crates/ui-egui/src/lib.rs`. Each action carries a stable `ActionKey` (command, target
  identity, payload digest) and the generation in which it was queued.
- Input-driven handlers queue only in UI pass 0. Pass 1+ receives no events, so it cannot
  re-derive a click or key.
- **Never clear the queue at the start of pass 1+.** There is no reliable way to predict a
  future discard from inside pass 0; `ctx.will_discard()` describes the current pass's
  output, not the next pass.
- Dispatch policy:
  - input-driven actions execute at the end of pass 0 (the pass that saw the events); the
    empty-event pass cannot recreate them, so this is exactly once;
  - actions that must observe final layout (dialog OK/Apply/close, dock reflow, actions that
    close a panel) stay queued across discarded passes and are drained at the end of the
    first pass whose `ctx.will_discard()` is false;
  - a duplicate enqueue in a later pass coalesces by `ActionKey`; it is never used as a
    reason to drop the original;
  - if a frame ends with deferred actions still pending (for example an abnormal maximum-pass
    exit or a caught panic), do not silently drop them: keep them, surface a visible status
    error, and dispatch once at the next safe pass. A test forces this path.
- `cancel_value_drag` (Escape drag rollback) is also pass-0-aware. In pass 0, if
  `pointer.any_down() && key_pressed(Escape)`, latch `escape_cancel` together with the undo
  mark. Apply the rollback once after the final UI pass (or after pass 0 when no discard
  occurred), then clear the latch. The final pass has no key event, so it must use the latch,
  not re-read `ctx.input`. The latch is scoped to the active press transaction; it does not
  cancel unrelated queued actions.
- `begin_press` runs before UI in pass 0; `cancel_value_drag` sees the corresponding mark and
  runs once.

Tests: click + forced discard = exactly one command/undo step; click on a `Grid` first-show
frame; key shortcut + forced discard; Escape drag + forced discard; two queued actions with
the same key coalesce; an abnormal pending queue is surfaced and dispatched exactly once.

This replaces the reviewed design that cleared the queue on `pass_index > 0`; that design
would have lost the user's click on every discarded frame.

### 4.5 Screenshot callback, hidden paint and CL-2

0.37 removes `egui::UserData` and `Event::Screenshot`; `ViewportCommand::Screenshot` takes a
`ScreenshotCallback` (`crates/egui/src/data/screenshot_callback.rs`).

Replace `lib.rs:1714 issue_screenshots` and `lib.rs:1744 collect_screenshots` with:

- one CL-2 completion slot per request, owned by the request record; the live slot holds the
  strong viewport waker (`Context` clone plus `ViewportId`, or `request_repaint` callback),
  and the callback holds only a `Weak` slot;
- `ViewportCommand::Screenshot(ScreenshotCallback::new(move |image| { /* complete the weak slot once, unlock, then wake */ }))`;
- `ScreenshotCallback::new` does not request a frame when the data arrives. Completion must
  unlock the slot and then call the stored waker; the callback has no repaint path of its own
  and does not hold a strong `Context` or app reference;
- `collect_screenshots` drains the result in logic and calls `control::save_screenshot(ctx,
  &image, path, crop)` as today; after timeout/close/cancel, release the slot and ignore a
  late image;
- token/correlation still required: two queued screenshots may complete out of order; use
  ownership plus the request ID, not callback order;
- admission and memory limits from §3.3: four outstanding requests, 128 MiB per image, 256
  MiB aggregate (a stricter documented cap is allowed), checked `width * height * 4`
  reservation before issue, and revalidation/rejection after any resize/DPI/zoom epoch
  change;
- the issuance record binds the target-UI logical bounds and geometry epoch to the returned
  pixels; a change back to the old geometry does not restore validity.

Hidden-window semantics are the 0.37 behavior, not the old assumption:

- `ViewportCommand::Screenshot` maps to `ActionRequested::Screenshot`/`PaintWhileHidden` and
  makes the integration run a UI pass even when hidden; the callback is then completed from
  that forced paint. A hidden pending screenshot is therefore expected to deliver an image.
- A request that is still queued and not yet issued — settling frames or a hidden logic tick
  — has no screenshot command to force its paint. Logic must send
  `ViewportCommand::RequestPaintWhileHidden` while such a request is pending so the UI pass
  that advances settling can run; once issued, the screenshot command itself forces paint.
- `ViewportCommand::RequestPaintWhileHidden` is also the supported way to force a UI pass for
  other stages that need composition/gesture handling while hidden (for example control
  `ui.*` requests that need fresh layout or settling without a screenshot).
- The screenshot timeout (`SCREENSHOT_TIMEOUT_S`) is a renderer/readback failure guard, not a
  "hidden never presents" path. A test must not encode "hidden returns timeout" as normal
  behavior. Test both branches: hidden + pending screenshot → image delivered, including the
  queued-but-not-yet-issued settling case; hidden + no capture request → no capture (and,
  where a request is admitted but the renderer cannot complete, the documented timeout
  error).
- `raw_input_hook` (`lib.rs:2092`) no longer preserves `Event::Screenshot`; it clears input
  events as it already clears hovered/dropped files. Update
  `crates/ui-egui/tests/ui_control_input.rs:153`: assert close/viewport metadata survive and
  no screenshot event is expected; add a native/VM check that the callback still delivers.
- `raise_for_control` (`lib.rs:1665-1673`) remains for occluded macOS raising without focus;
  pair it with `RequestPaintWhileHidden` where a UI pass is required, rather than assuming a
  hidden window never runs UI.

### 4.6 Playback, audio, autosave, jobs, trim and deadlines

- `advance_playback` (`lib.rs:1128`) sets the playhead from `anchor_tick + elapsed`; use a
  monotonic host clock and do not accumulate dt. Repeated calls at the same time are
  idempotent.
- `playback_mix` remains the live project snapshot; audio callbacks are outside egui. Logic
  keeps polling `audio.played_frames()` while hidden.
- `FilmcraftApp::frames.pump` moves from `App::ui` to logic. The web runner's `WebApp::logic`
  also pumps jobs/audio; its per-frame work must be idempotent because web calls logic per
  pass.
- Auto-save: `Session::poll_persistence` and `session.is_dirty` move to logic; the recovery
  journal worker must not depend on painted frames.
- `watch_jobs` splits: `poll_jobs` in logic handles completion and `play_after_render`; the
  status-bar `job_status` remains in UI.
- `trim_monitor::advance` (`lib.rs:1817`; `panels/trim_monitor.rs:39-48`) moves to logic and
  uses a monotonic clock. `trim.tick` receives an idempotent target time/step, so a second
  logic call for the same pass does not advance edits twice; it still requests repaint.
- Metadata follow, `auto.begin_frame`, `sync_timeline_view` and `sync_native_menu` run once
  in UI pass 0 (or are made idempotent if moved to logic).
- Screenshot settling still waits for `monitor_inexact` and `timeline_still`, both updated in
  logic/UI with the monotonic clock.
- Add a CI grep that rejects direct `session.execute`, `menus::invoke` and `trim.tick`
  execution from UI-only modules outside the queue/dispatch module and an explicit
  allow-list. Until that check is green, the one-action guarantee is not claimed.

## 5. Concrete API migration catalogue

These are the source-grounded incompatibilities. Line numbers are baseline line numbers.

### 5.1 `FontData` is a shared blob

0.37 `epaint` has `pub type Blob = Arc<dyn AsRef<[u8]> + Send + Sync>` and
`FontData::from_blob(Blob, index)` (`crates/epaint/src/text/font_data.rs`). Replace literal
`FontData { font: Cow::... }` in:

| File:symbol | Current code | Migration |
|---|---|---|
| `crates/ui-egui/src/cjk.rs:53` `find_system_font` | `FontData { font: Cow::Owned(face.data()?), index: face.info.index, ... }` | `FontData::from_blob(Arc::new(face.data()?), face.info.index)` |
| `crates/ui-egui/src/i18n/mod.rs:292` `system_japanese_font` | `FontData { font: Cow::Borrowed(bytes), index: face.info.index, ... }` | `FontData::from_static(bytes)` (bytes are already leaked and shared) |
| `crates/ui-egui/src/rtl.rs:92` `system_fonts` | `FontData { font: Cow::Owned(data), index: face.info.index, ... }` | `FontData::from_blob(Arc::new(data), face.info.index)` |

Keep `Arc<FontData>` in `font_data` maps; `FontData` is cheap to clone now. Add a regression
that a TTC face index is preserved (non-zero `info.index`), because the literal construction
was the only place that carried it.

### 5.2 `MouseWheel` gained `source`

0.37 `Event::MouseWheel` has a required `source: MouseWheelSource`. Add
`source: MouseWheelSource::Wheel` for the control-channel synthetic wheel in
`crates/ui-egui/src/control.rs:370`, and to the two tests that construct it:
`crates/ui-egui/tests/monitor_geometry.rs:176` and `crates/ui-egui/tests/panels_ui.rs:249`.
Use `Wheel` rather than `Unknown` so ScrollArea behavior and kinetic-scroll policies match a
real mouse wheel. Add a test that the timeline wheel path (`panels/timeline.rs:2221`) still
steps one tab per notch and does not start kinetic scrolling in headless tests.

### 5.3 `WidgetType` -> `Role`, and one role per control

`WidgetType` is a deprecated alias of `accesskit::Role`; `SelectableLabel` no longer exists
and `TextEdit` is `TextInput`. Replace the ten sites:

| File:line | Current role | New role |
|---|---|---|
| `crates/ui-egui/src/dock.rs:673` | `WidgetType::Button` | `Role::Button` |
| `crates/ui-egui/src/dock.rs:725` | `WidgetType::SelectableLabel` | `Role::Tab` with `selected(active)`; the code already overrides the node role to `Tab` |
| `crates/ui-egui/src/dock.rs:765`, `:779` | `Button` | `Button` |
| `crates/ui-egui/src/dock_ui.rs:326` | `Button` (floating-panel resize grip) | `Role::Button` with the accessible name "Resize panel". Do **not** expose `Splitter` here; a floating-window grip is not a pane divider. Dock split dividers use `Role::Splitter`; update `crates/ui-egui/tests/shared_docking_ui.rs:201` to the chosen role in the same PR |
| `crates/ui-egui/src/panels/graphics.rs:1028` | `TextEdit` | `Role::TextInput` with a name |
| `crates/ui-egui/src/panels/tools.rs:27` | `SelectableLabel` | `Role::Button` with `selected`; a radio-group model is a later accessibility change |
| `crates/ui-egui/src/panels/monitor.rs:1013` | `Button` | `Button` |
| `crates/ui-egui/src/panels/source_drag.rs:46` | `Button` | `Button` |
| `crates/ui-egui/src/theme_favorites.rs:4` | `Button` | `Button` (favorite toggle; `CheckBox` is a later semantic change) |

The previous `Button` vs `Splitter` contradiction is resolved: floating grip = `Button`;
dock pane divider = `Splitter`. The test is updated once, not left ambiguous.

### 5.4 `ScrollArea::State::offset` is private

0.37 provides `clamped_offset()` and `unclamped_offset()`; the old `offset` field is private.
Replace `crates/ui-egui/src/panels/media_browser.rs:576` and
`crates/ui-egui/src/panels/project_views.rs:516` (`out.state.offset.x`) with
`clamped_offset().x` for header alignment. Add a test that scrolls the project list
horizontally to the end and asserts the header’s first column x matches the last visible
row’s column x within one point.

### 5.5 `Context::run_ui` callback is `&mut Ui`

`crates/ui-egui/src/lib.rs:2411` currently writes:

```rust
let mut out = ctx.run_ui(raw, |ctx| secondary = ctx.input(|i| i.pointer.secondary_clicked()));
```

Change to:

```rust
let mut out = ctx.run_ui(raw, |ui| secondary = ui.ctx().input(|i| i.pointer.secondary_clicked()));
```

The other `run_ui` call sites already use `|ui|`. Do not call `Context::root_ui` inside these
closures.

### 5.6 `Id::new` and `Ui::id`

0.37 deprecates `Id::new` and `Ui::id` (`Id::unique` / `Ui::scope_id`). FilmCraft has 403
`Id::new` occurrences across `crates/ui-egui/src` and tests. Under `clippy -- -D warnings`,
deprecation is a hard failure.

- `egui::Id::new(x)` -> `egui::Id::unique(x)` for every root id; same hash, same identity.
- `Id::new(parent).with(child)` -> `Id::unique(parent).with(child)`.
- `UiBuilder::id(x)` -> `UiBuilder::scope_id(x)`; FilmCraft uses `id_salt` and has no
  `UiBuilder::id` (verified), but `craft-ui` PR #199 performs its own conversion.
- `Ui::id()` -> `Ui::scope_id()`; FilmCraft has no `ui.id()` call (verified).
- Keep `id_salt` for child UIs; do not replace every `id_salt` with a global id.

Add a grep-based CI check that no `Id::new(` remains in FilmCraft or its tests.

### 5.7 `Event::PasteImage`

0.37 emits `Event::PasteImage(Arc<ColorImage>)` when the clipboard has no text. Update
`crates/ui-egui/src/lib.rs:2299` `clipboard_events_as_keys` to map `PasteImage` with
`command` to `Key::V`, with `shift` to `Key::Insert`, and plain to `Key::Paste`, exactly as
`Event::Paste`. Add tests alongside `ctrl_c_x_v_become_key_presses`.

The native app’s `clipboard_media` hook (arboard) still supplies files/images to the Paste
command. On wasm, either keep the existing “FilmCraft clips only” path or extend
`ClipboardMedia` with the `PasteImage` bytes; that is a separate feature decision. Do not
double-read the clipboard if the event already carries an image.

### 5.8 `Event::Screenshot` / `UserData`

Covered in §4.5. Remove `egui::UserData::new` at `lib.rs:1721` and the `Event::Screenshot`
matches at `lib.rs:1753` and `lib.rs:2100`.

### 5.9 Other 0.37 changes that are compile-compatible but visible

- `default_fonts` off by default: explicit feature; `monochrome_emoji_fonts` explicit (§3.5,
  §6.1).
- `clipboard` opt-in in eframe/egui-winit: native feature added (§3.5).
- `Id` deprecations (§5.6).
- `Panel` constructors take `impl AsIdSalt`: FilmCraft does not use `egui::Panel`, but
  `craft-ui` and any new panel code must use the new form.
- `UiBuilder::scope_id`: see PR #199.
- `ComboBox` is built from atoms and takes `IntoAtoms`; existing `selected_text`/`show_ui`
  calls remain source-compatible, but styling/measurement changes must be reviewed.
- `WidgetType::TextEdit` renamed to `Role::TextInput`; `TextEdit` itself uses
  `Role::TextInput`.
- `Context::root_ui`/`run_pass`: not adopted; no recursive root UI.
- `ViewportCommand::Screenshot` callback and `RequestPaintWhileHidden`: adopted as above.
- `egui::Event::PasteImage`, `MouseWheelSource`, PRIMARY selection: adopted as above.
- kittest 0.37 hard-fails input widgets without accessible names (#8589/#8611). The first
  green compatibility stage must name all input widgets, including all 56 `TextEdit`
  construction sites; do not disable strict naming globally.

## 6. Fonts, glyph coverage, language and RTL

### 6.1 Font installation and bundled coverage

`crates/ui-egui/src/theme.rs:420` `install` builds `FontDefinitions`, inserts Inter/JetBrains
Mono, then appends Japanese craft-fonts (`add_craft_fonts`), Chinese (`cjk::install`) and RTL
(`rtl::install`). Keep that order.

The explicit `default_fonts` feature preserves egui’s built-in fallback, but in 0.37 the
monochrome emoji faces moved behind `monochrome_emoji_fonts`. Enable it explicitly. FilmCraft
starts from `FontDefinitions::default()` and uses `✔ ✓ ✖ ⚡ ⚠ ⚙ ♪ ☑` (20 occurrences across
10 files); with `system_fonts` off, those symbols can otherwise become tofu or switch to a
platform face. Add a glyph-coverage assertion for exactly these characters plus a screenshot
diff.

### 6.1.1 Deliberate visible `▸` → `>` substitution

The integrated tree replaces the user-visible chevron U+25B8 (BLACK RIGHT-POINTING SMALL
TRIANGLE) with ASCII `>` in 38 lines across 18 files: status, toast and error hints that
point at menu paths (`Settings ▸ Audio Hardware`, `File ▸ Link Media…`, `Export ▸ Hardware
encoding`, `Project Settings ▸ Scratch Disks`, `Text to Speech ▸ Download natural voices`),
the matching i18n source keys and translation rows, and the mixer post-fader insert prefix
(`▸ ` → `> `). One `# View ▸ Reset Interface Zoom` header comment in each language file was
updated for consistency; Rust doc comments were left unchanged.

This is deliberate tofu avoidance. FilmCraft installs Inter at the front of the proportional
family and `FontDefinitions::default()` then falls back to Ubuntu-Light, egui-icons and the
monochrome emoji faces; none of those contains U+25B8 (verified from each bundled font's
`cmap`), and `system_fonts` stays disabled, so proportional menu-path text could show a
missing-glyph box. JetBrains Mono and Hack do cover U+25B8, but the substitution is global so
the same hint reads identically in every family. ASCII `>` is used because every bundled face
has it. This is a source-level decision; no visible GUI comparison has been recorded yet, so
it remains part of the visible-GUI acceptance gate below.

`FontData` construction is §5.1. Keep the process-wide caches (`OnceLock`) and the single
system scan; do not call `filmcraft_text::fonts::scan_system()` in a UI pass.

`eframe` 0.37 adds an optional `system_fonts` feature (fontique provider, #8512) and a
`system_font_fallback` option. **Do not enable it in the migration.** It changes startup cost
and the fallback face chosen for a missing glyph; the current explicit CJK/RTL fallback is
part of FilmCraft’s visual contract. Record it as an opt-in follow-up with measured startup
and glyph coverage, especially for color emoji; do not accept clamp-only as a substitute for
texture wrap/filter semantics.

Pinned test fixtures are the exact set from the egui plan §10.6, fetched into the task’s
evidence fixture directory with their licence files. **Do not copy font binaries into this
repository.** The fixture set is:

| Fixture | Source | SHA-256 | License |
|---|---|---|---|
| Arabic | craft-fonts `2481a8a324b986a45954e90045ed1f8f2efaaf3c` `fonts/noto-sans-arabic/NotoSansArabic.ttf` | `63111b5b2e074dd48cc67692e0a2726d86ee94c1c37fe8598257b7b4e87e869e` | OFL-1.1, adjacent `OFL.txt` |
| Thai | same craft-fonts `fonts/noto-sans-thai/NotoSansThai.ttf` | `5a1c559bb539583c8a1fd99d1c5b9491e5e14478c9cd2bd0970d5c3096cc9ef8` | OFL-1.1 |
| CJK SC | same craft-fonts `fonts/noto-sans-cjk-sc/NotoSansCJKsc-Regular.otf` | `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b` | OFL-1.1 |
| Japanese | same craft-fonts `fonts/biz-ud-pgothic/BIZUDPGothic-Regular.ttf` | `258d7156c165f2ff774b6efee637c22c3b950de0d8a10e501137061bc8085d01` | OFL-1.1 |
| Hebrew | google/fonts `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5` `ofl/notosanshebrew/NotoSansHebrew[wdth,wght].ttf` | `7ef36a2c3593758cdb622e1bdef4f84523e92fbc3ccc667438dd80ff54c2de88` | OFL-1.1; fix axes weight 400/width 100 |
| Color emoji | googlefonts/noto-emoji `22e564626297b4df0a40570ad81d6c05cc7c38bd` `fonts/NotoColorEmoji.ttf` | `39ee3c587e10e89669b9ff32703261d10d5f9c4dd5ad147b6b5a1c5200591817` | OFL-1.1 |
| Bundled Latin/emoji | candidate `epaint_default_fonts` `Ubuntu-Light.ttf` | `80307b8da7649aa4ee4d484b232140e3ce1ec0ca093073d3c53c8f5a5ced7a70` | Ubuntu Font License, adjacent `UFL.txt` |
| Bundled mono emoji | candidate `epaint_default_fonts` `NotoEmoji-Regular.ttf` | `415dc6290378574135b64c808dc640c1df7531973290c4970c51fdeb849cb0c5` | OFL-1.1 |

The fixture bytes and licence hashes enter the evidence manifest. A controlled comparison
uses a fixed common set; a separate default-policy visual comparison records each
baseline/candidate policy. System-provider tests are a nondeterministic integration row with
OS/font inventory recorded, not pixel-golden input.

### 6.2 UI language and AccessKit language

FilmCraft’s `i18n::Language` is a private list (`crates/ui-egui/src/i18n/mod.rs`,
`Language::ALL`; 12 languages, no Arabic/Hebrew UI). Add `Language::bcp47()` returning proper
tags (`en`, `ja`, `es`, `pt-BR`, `uk`, `zh-CN`, `de`, `ru`, `fr`, `id`, `it`, `ko`). Call
`ctx.set_language(Some(tag))` when the language changes and `frame.set_ui_language(Some(tag))`
in `App::ui` (fork E18) so the AccessKit root and the web hidden IME input use the same tag.
In kittest, `Frame::set_ui_language` is a no-op; the `ctx.set_language` call is the tested
path.

Do not enable `Style::right_to_left` (fork E9) yet: FilmCraft offers no RTL interface
language, and mirroring the custom-drawn workspace without a full RTL audit would be worse
than leaving it LTR. Document RTL UI as a future feature gated on real translated layouts.

### 6.3 Document RTL and the fork’s bidi work

`crates/ui-egui/src/rtl.rs` currently compensates for 0.36’s lack of bidi: `line_sections`
splits RTL runs into words, reverses them, mirrors brackets and gives each word its own
`LayoutSection`. The fork (E5/E12) performs UAX #9 shaping and visual ordering itself. Keeping
both is a double-reversal bug.

In P1, in the same stage that pins the fork, change `rtl::Lines`:

- keep `has_rtl` and the `rtl` alignment flag;
- build one `LayoutJob` with `direction: TextDirection::Auto`,
  `halign: if rtl { Align::RIGHT } else { Align::LEFT }`, `halign_min_is_start: true`, one
  section, and the requested wrap width;
- delete `line_sections`, `mirror`, `in_word`, `line_ranges` and their unit tests once the
  fork is pinned; replace them with tests on `Row::rtl_paragraph`, `Row::visual_order`, glyph
  order and right alignment;
- keep `paint`/`paint_line` API so `timeline_captions.rs:152` and `text.rs:239` do not change
  contracts.

The fork's E5/E12 path and the removal of the manual reversal must ship in the same P1
commit set; there is no intermediate state with both active. A negative regression first
demonstrates the double ordering (Arabic caption with the fork produces reversed words), then
P1 removes it. Test `"كلمة أخرى"`, `"قال FilmCraft 0.6 جيد"`, Arabic-Indic digits, brackets
and an LTR paragraph containing an RTL phrase; assert the leftmost drawn glyph of the first
visual word, not just “some Arabic glyph exists”.

`TextEdit` keeps logical order; fork E13/E16 add grapheme/bidi-safe caret movement. Test
caption text editing (logical caret, Delete/Backspace, Home/End) and selection rendering.

## 7. Input, IME, clipboard and PRIMARY

### 7.1 Keyboard

`handle_shortcuts` (`lib.rs:1546`) already uses `i.consume_key` and a panel/app-wide
specificity sort. Fork E19 adds `RawInput::keyboard_layout_is_latin` and
`latin_layout_chars`; `handle_shortcuts` must use the Latin-layout character for letter
shortcuts when the active layout is not Latin, so `Ctrl+Z` still undoes on a
Russian/Arabic/Hebrew layout and an unmodified `Z` does not become a tool shortcut. P7
held-key pairing keeps a key's emitted egui identity for its release when the layout or
modifiers change, clears it on focus loss/IME/destruction, and does not redefine winit's
live `logical_key`. Add a kittest that constructs `RawInput` with
`keyboard_layout_is_latin: Some(false)` and
`latin_layout_chars: vec![(Key::Q, 'q'), ...]`, sends a physical key, and asserts the command
fires only for a Latin-letter key. Do not hard-code a platform.

Input handling remains in `App::ui` pass 0, where the event list is fresh; pass 1+ must not
re-run it.

### 7.2 IME and outgoing surrounding text (E21b/P7b)

Fork E6 (request_focus does not interrupt composition), E8 (TextEdit glyph clipping), E21
(`ImeEvent::DeleteSurroundingBytes`) and winit W18-W27 cover Windows/X11/Wayland/macOS IME.
The candidate has a confirmed gap: winit W27 exposes
`WindowExtWayland::set_ime_surrounding_text`, but egui-winit never calls it, and
`egui::IMEOutput` carries no logical text. E21 alone carries incoming deletion/commit only.

This migration therefore requires **E21b/P7b** before acceptance. The authoritative
contract is the complete E21b section of `egui/docs/egui-037-migration-plan.md` plus
`winit-review-triage.md`; these bullets are FilmCraft's adoption summary, not a weaker
restatement:

- `IMEOutput` carries one atomic descriptor: owner/context-generation identity plus the
  surrounding-text state and payload. The owner generation is present **even when the state
  is `Unavailable`**; an available empty document is distinct from unavailable support and
  is sent as an explicit empty payload, not omitted.
- add a bounded transient payload of at most **4000 UTF-8 bytes**, extracted at valid UTF-8
  boundaries **without allocating a full document copy**, with indices remapped after
  excluding preedit. If the selection cannot fit, emit explicit `Unavailable` rather than
  presenting a truncated selection as complete. Keep `Clone`/`Eq`, update `Copy`-dependent
  callers, and keep transient text out of Debug, serialization and persistence.
- built-in `TextEdit` produces committed logical text after applying deletion/commit/cursor
  updates, excluding preedit, with UTF-8 byte cursor/anchor indices; password fields emit no
  text. Normal-to-password, no-IME, focus-loss/reset, owner-change and unavailable
  transitions clear old backend text; each payload belongs to the current IME-owning
  widget/viewport.
- W27 gains an explicit owner/support transition: clear its cache and commit a
  disable/enable boundary before publishing a new owner or support state. Merely changing
  purpose or sending empty text does not clear protocol state. Available-empty maps to
  `set_surrounding_text("", 0, 0)` plus commit; unavailable omits `set_surrounding_text` and
  uses the committed clear boundary; when support returns, enable plus the full payload
  precedes commit. Cancel the old preedit without inserting it into the new owner.
- egui-winit appends the latest completed pass's IME descriptor, owner and payload together,
  including an authoritative `None`; a previous payload must not survive a later
  non-text/password/`None` pass. The same finalized output and payload are appended
  together across passes.
- on a current-epoch text-input-v3 `done` with a mismatched serial, apply the incoming edits
  **once** and defer outgoing state until a matching serial. Obsolete owner/focus epochs
  invalidate their pending batch; a current-generation serial mismatch is not a reason to
  drop valid edits.
- the egui-winit Wayland adapter calls W27 with the transient payload on text, selection or
  IME-owner change, per window and owner; macOS NSRange conversion remains a separate W26
  path, not a claim of a new macOS API.

FilmCraft’s own custom `IMEOutput` producer is at
`crates/ui-egui/src/panels/graphics.rs:933-940`. It must supply the logical model text, the
preedit, and UTF-8 byte cursor/anchor ranges from its own scalar/grapheme coordinates; it must
not pass display-bidi text or a `None` placeholder. Its integration test must reject a
split-UTF-8 range and verify the model range maps to the emitted byte range. Built-in
`TextEdit` consumers in FilmCraft (caption, notes, search, metadata) inherit the producer and
need tests rather than custom adapters; the same inheritance applies to LightCraft’s built-in
consumers. The suite inventory of custom producers is WordCraft `canvas.rs` (two),
VectorCraft `canvas.rs`, FigCraft `shortcuts.rs`, EffectCraft `viewer_text.rs`, FilmCraft
`graphics.rs`, DesignCraft `canvas.rs`, DeckCraft `canvas.rs` and PhotoCraft `type_tool.rs`;
each needs its own adapter and test.

Required tests: outgoing text changes immediately after delete+commit and deletion-only
batches; scalar/grapheme vs byte indices; supplementary emoji/combining marks/RTL logical
order; preedit exclusion and caret movement; long selection/window limits; same-window field
switch; two windows; focus loss/reset/late serial; initial empty, available-empty → non-empty
and unavailable/password → available transitions; multi-pass final-output selection where
pass 1 is text and pass 2 is password, non-text or `None`. Native Wayland Vietnamese/
predictive IME verifies the protocol trace and editor content/undo. Headless tests cannot
prove native IME focus/composition.

#### Film graphics IME ownership lifecycle

Incoming `ImeEvent` carries no owner or epoch, so the custom Program-monitor editor keeps a
local causal lifecycle: `Active(owner)`, `TerminalPending(owner)` and `Stale`. A non-empty
preedit records `Active`; an empty preedit clears the visible composition but records
`TerminalPending`, because its commit/delete is a separate event. A target change or editor
close turns either state into `Stale`, so that pending terminal event cannot edit the
replacement target. A rejected stale commit is itself the terminal boundary and consumes the
tombstone immediately, allowing a preedit later in the same raw batch to start a new
composition. A stale empty preedit or delete is only tentatively terminal: the tombstone is
kept through the batch so a paired old delete+commit is rejected, and is consumed at batch end
when no commit follows. A lone stale non-empty preedit keeps the tombstone across batches, so a
preedit/commit pair split across batches cannot retarget either. This uses observed event order
only, never frame/time heuristics. Regressions cover an empty preedit followed by a target
switch before its commit, and a coalesced stale commit followed by a new preedit in the same raw
batch. An entirely unobserved platform reset remains indistinguishable from a new composition;
the first ambiguous composition after it may be dropped rather than claiming provenance.

Platform rows:

- Windows: Japanese and Korean IME in a caption/notes/search `TextEdit`; caret follows
  composition; a key the IME consumes is not also a tool shortcut; commit adds exactly one
  undo step.
- X11: Vietnamese Telex; one input context per window; no duplicate letters.
- Wayland: text-input-v3 surrounding text and delete-surrounding; no double commit; outgoing
  bytes observed after commit/delete.
- macOS: press-and-hold accents, dictation, ideographic space, first Korean key; real
  hardware required.
- Web: named keys are not typed as text; `lang` on the hidden input; IME commit in a text
  field.

### 7.3 Clipboard and PRIMARY

0.37 adds PRIMARY selection (#8428). The production graph must integrate **both** E4 and
W15P:

- W15P adds a winit-owned PRIMARY path on the existing Wayland connection/event queue and
  per-seat lifecycle, using SCTK 0.19.2’s existing `PrimarySelectionManagerState`, device,
  source and offer support. PRIMARY is a distinct protocol object; do not bind a second
  regular `wl_data_device`.
- Egui 0.37 `get_primary`/`set_primary` routes through that path. E4 preserves password
  exclusion and does not reclaim ownership for an unchanged selection.
- FilmCraft also uses `arboard` directly for files/images (`apps/filmcraft/src/main.rs`
  `clipboard_media`). The arboard/smithay PRIMARY path is a measured fallback only if the
  winit path cannot meet a proven requirement; it is not the planned end state and it is not
  accepted merely by documenting a GNOME limitation.

Required native matrix, on X11 and Wayland, under wlroots (Hyprland/Sway), KWin and Mutter
with supported capabilities recorded:

- keyboard- and pointer-triggered selection plus middle-click paste at the clicked position.
  A PRIMARY request uses a fresh serial from the event that triggered it; the protocol
  explicitly allows pointer-driven selection and `set_selection` takes the triggering
  event's serial. Do not impose a keyboard-only serial rule;
- serial freshness, owner/seat changes, empty/absent offers, self-paste;
- password selection never offered;
- unchanged selection does not reclaim ownership;
- bounded asynchronous transfer: total deadline **2 s**, **16 MiB** cap, at most **4 read
  workers** and **2 writer threads** with queue depth **8**. Copy/PRIMARY ownership claims
  stay synchronous on the fresh triggering input turn (validate serial/seat, reserve bytes,
  create the source, call `set_selection`); only pipe serving and potentially blocking reads
  are asynchronous;
- a caller-owned live completion handle records the target window, input generation and an
  owner-specific waker, wakes only that viewport, and is held weakly by workers. Canceled,
  stale or superseded results are discarded and release their resources; no arbitrary
  first-seat fallback;
- slow, trickling, oversized, canceled and failed transfers release/complete exactly once;
- text copy/cut/paste and Paste/Paste Insert/Paste Attributes;
- file list and image paste through arboard, not duplicated by egui’s image paste;
- Windows `Ctrl+Insert`, `Shift+Insert`, `Shift+Delete` still map through
  `clipboard_events_as_keys`;
- `Event::PasteImage` maps to `Key::V` and does not produce two Paste commands.

## 8. Panels, geometry, docking, viewports and GPU resources

- `dock_ui.rs:190` `dock_area` calls `dock::layout_with_actions` for a manual rect tree, then
  `panel_body` (`:144`) creates `ui.new_child(... id_salt(...))` and sets clip rects. 0.37’s
  `Panel` API is not used; keep the manual layout, but verify that `Ui::new_child`/`scope_id`
  and the improved panel clip rect (#8562) do not change body rects.
- Floating groups use `egui::Area` with `Order::Middle`; layer order is read through
  `ctx.memory(|m| m.layer_ids())` (`dock_ui.rs:173` `floating_order`). Check the 0.37 layer
  iteration API and z-order after the bump; `ui_dock_resize.rs` and `shared_docking_ui.rs`
  cover focus, resize, detach, close/reopen and tab reorder.
- `dock.rs` draws custom tabs and registers automation ids. Keep stable ids
  (`panel.tab.*`, `timeline.tab.*`, `panel.menu.*`, `panel.float.*.resize`) and use
  `Role::Tab`/`Role::Button` semantics. The `Tabs` tab strip is not an `egui::TabBar`; do not
  replace it.
- `ViewportCommand` usage remains root-only: `Close`, `InnerSize`, `Focus`, `CancelClose`,
  `Maximized`, `Fullscreen`, `StartDrag`, `CursorPosition`, `Screenshot`, and the new
  `RequestPaintWhileHidden` for hidden composition stages. `ui.resize` and `ui.focus`
  control methods keep their behavior; `control_input_only` must still preserve close and
  viewport lifecycle metadata.
- GPU: `FilmcraftApp::set_wgpu` receives `CreationContext::wgpu_render_state`;
  `TextureHandle` and `GpuState`/`uploadedBytes` remain. `egui-wgpu` fork E2/E3 add surface
  fitting and error scopes; they are relevant to large/odd windows. Do not change the
  FilmCraft `wgpu 30.0.1` pin or the GPU compositor in the migration PR. The candidate’s own
  lock resolves `wgpu 30.0.0`; FilmCraft’s lock is authoritative. Record the actual resolved
  version and prove there is one source; do not claim the candidate lock’s version.
  `perf.stats` `gpu.uploadedBytes`, transitions, `submitMs` and frame-cache counters are the
  upload/GPU observables.

## 9. Commands, undo, save and reopen

The public contracts are:

- every interactive action is a command registered in `crates/engine`; UI, CLI, MCP and the
  control channel use the same ids;
- `menus::invoke` dispatches UI-only commands and engine commands;
- drag gestures are one undo step via `begin_press` and `cancel_value_drag`;
- `file.save`, `file.saveAs`, `file.open`, `file.closeProject`, `file.revert` and recovery are
  unchanged;
- UI state (tool, workspace, dock layout, zoom, scroll, monitor settings) is serde in
  `crates/ui-egui/src/state.rs`; project state is not UI state.

Migration rules:

- `begin_press` runs in UI pass 0 before any UI; `cancel_value_drag` runs once after the
  final UI pass using the pass-0 Escape latch (§4.4).
- Input-driven UI mutations dispatch once in pass 0; actions that must see final layout are
  queued and drained on the first non-discarded pass. Engine commands from control/MCP/CLI
  remain one-shot channel work in logic.
- A forced two-pass click adds exactly one undo step and one project revision. A forced
  two-pass click on a first-show `Grid` does the same. A forced discard during a drag with
  Escape cancels once.
- Save/reopen preserves an edit, an effect, a caption, and the dock/tab layout. Existing
  `timeline_tabs_ui.rs:258` and `scripted.rs:548` patterns are the starting point; add an
  effect/trim/save/reopen assertion and compare `sequence.inspect` and `project.inspect`.

## 10. Fork and vendor patch provenance

FilmCraft has no vendor directory. The suite forks carry behavior that FilmCraft either uses
or must consciously not use. Keep the pins; do not delete unrelated fixes. The tables below
name the aiongg research candidate for provenance only; production pins are G-PINS.

### 10.1 `aiongg/egui` `craft/0.37` (`a4f9a17f`, research)

| Patch | Decision in this plan | Validation |
|---|---|---|
| E2+E3 egui-wgpu surface fit/error scopes | Keep; use for large windows and odd surfaces | Windows/Linux resize/VM; `ui.resize` to a large size; no `Wgpu` error |
| E4 Wayland clipboard/PRIMARY | Keep; must be paired with W15P; the filmcraft#1115 problem is exactly this | Hyprland/KWin/Mutter copy/paste, PRIMARY matrix, file/image paste |
| E5, E8, E12, E13, E14, E15, E16, E17, E20 | Keep; use them; remove FilmCraft’s manual bidi | Arabic/Hebrew captions, editing, alignment, screenshots |
| E6, E21 IME | Keep; E21 alone is incoming only; E21b is mandatory | native IME rows plus outgoing-state tests |
| E7, E19 non-Latin shortcuts and P7 held-key pairing | Keep; adapt `handle_shortcuts` to Latin-layout chars; retain the emitted egui key identity for a held physical key across layout/modifier changes and clear it on focus loss/IME/destruction | kittest + native layouts, repeat/unidentified keys |
| E9 RTL UI mirroring | Keep in fork but do not enable `Style::right_to_left` | no RTL UI language yet |
| E10 web key/lang fixes | Keep; test browser IME and named keys | `smoke.mjs` + manual browser |
| E11 vello_cpu multithreading | Not exercised by FilmCraft; no local feature change | none |
| E1 viewport hooks | Not used by FilmCraft; keep the fork patch | no FilmCraft test |
| E18 `Frame::set_ui_language` | Use with `ctx.set_language` | language switch + AccessKit root |
| E15 `LineBreaker`/`LayoutJob::language` | Optional; use only if document line breaking needs it | caption wrapping tests |
| E21b/P7b outgoing IME surrounding text (P7b) | **Required new prerequisite**: the complete shared owner/support/serial contract, 4000-byte bounded payload without a full-document copy, TextEdit producer, Film graphics adapter and Wayland delivery | §7.2 tests; native Wayland trace |
| P7c selected #8763/#8748 fixes | P7c owns the selected, tested #8763 selection-action and #8748 hidden parent/child fixes. #8764 remains an unmerged proposal at `6eb460a2a6d537d312a80579cb801895509deaed`, not a release fix | selection/pointer/keyboard and hidden-parent regressions |

There is no E22. Do not invent fork APIs. A local workaround for #8763 needs a specific
reason and removal condition.

### 10.2 `aiongg/winit` `craft/0.30.13-lang` (`8b35388b`, research)

Keep the whole pinned branch because E4/E19/E21/E21b require W15/W15P/W25/W26/W27,
including the bounded W15P transfer path and the W27 owner/support lifecycle. For FilmCraft,
exercise at least:

- W1 mixed-DPI window rect; W3/W4 Windows IME/hidden maximized; W5/W20/W21 X11 IME;
  W6-W10 macOS IME; W11 X11 DPI; W12 macOS titlebar drag; W13 Wayland scroll phase;
  W15 Wayland drops/clipboard; W15P PRIMARY; W18/W19 Windows IME; W22-W27 language/IME.
- W15P is required with E4 as described in §7.3, including per-seat lifecycle, password
  exclusion, unchanged-owner behavior and slow/canceled transfer tests.
- W14 macOS cancelable termination is opt-in; FilmCraft does not call that platform extension,
  and its own close interception remains.
- Do not enable W16 pinch/rotation or tablet input; no `craft-tablet` dependency exists.

If the suite later replaces these forks with upstream releases containing the fixes, drop the
corresponding patch rows only after the same tests pass; do not remove a patch just because a
release number moved.

### 10.3 `craft-libs` port

Port only the selected PR-199 API hunks (`8614e4a`) and README changes (`1764bca`) onto the
current published `4919088` tree. Preserve newer security/settings/docking/printing fixes,
including the protected-group docking tests. Never merge `b68de8c` or `ec8840a` as trees. The
CL-2/CL-2F helper is a separate reviewed addition, not part of the stale PR tree.

## 11. Tests and observable assertions

### 11.1 Required repository gates (managed, proposed)

Every new host build/test and owned-VM command must use
`maintenance/task.py run/status/finish` with its own task, job ID, source and evidence. The
commands below are proposals for the parent to submit; they were not executed during this
planning pass. Capacity must be inspected first; the suite snapshot had less than one default
64 GiB build reservation available after existing reservations.

```sh
python3 /home/codex/work/maintenance/task.py run \
  --job-id filmcraft-egui037-ci-01 --kind build \
  --task filmcraft-egui037-migration \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/filmcraft-implementation \
  --evidence /home/codex/work/ssd2/task-evidence/egui037-migration-20261022/filmcraft \
  -- cargo xtask ci
python3 /home/codex/work/maintenance/task.py status filmcraft-egui037-ci-01 --tail-bytes 8192
python3 /home/codex/work/maintenance/task.py finish filmcraft-egui037-ci-01 --artifact-manifest /home/codex/work/ssd2/task-evidence/egui037-migration-20261022/filmcraft/artifacts.json
# after inspecting the plan and preserving evidence:
python3 /home/codex/work/maintenance/task.py finish filmcraft-egui037-ci-01 \
  --artifact-manifest /home/codex/work/ssd2/task-evidence/egui037-migration-20261022/filmcraft/artifacts.json \
  --apply --owner-confirmed --checks-complete --evidence-preserved
python3 /home/codex/work/maintenance/task.py status filmcraft-egui037-ci-01 --tail-bytes 8192
```

Every job uses one unique resource task/fresh job ID/`--source`/`--evidence`; poll `status`
until both the command and the finish outcome are known, and never treat `ok: true` alone as
success.

`cargo xtask ci` already runs fmt, clippy, tests, layers, assets and wasm; keep it as the
authoritative gate. Also run, as separate managed jobs where a selected feature is not in
default CI:

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, `cargo xtask layers`, `cargo xtask assets`, `cargo xtask wasm`;
- with and without `CRAFT_FONTS_DIR` (pinned craft-fonts `abb83316d96aa59c1cf64784289e378fe9fa5695`);
- `FILMCRAFT_REQUIRE_ORACLES=1` when ffmpeg is available;
- `cargo metadata --locked`, the graph checks from §3.4, and the native `clipboard` +
  `monochrome_emoji_fonts` feature assertions.

Strict kittest naming and glyph behavior is part of the first green compatibility stage.
Do not add a global `allow`/disable for missing names or missing glyphs. If a name/glyph is
missing, fix the widget or fixture.

**Font features do not propagate through kittest.** `egui_kittest`'s normal `[dependencies]
egui` uses `default-features = false`; only kittest's own `[dev-dependencies]` enable
`default_fonts` and `monochrome_emoji_fonts`, and a dependency's dev-dependencies do not
propagate into a consumer. Every kittest/FilmCraft test root that needs the bundled faces must
enable those egui features explicitly or install controlled fonts, and each actual graph must
be inspected. The production feature policy in §3.5 does not reach the harness by itself.

Windows uses a uniquely owned clone and the VM run/status/finish workflow, with the clone’s
`Build-DesignCraft.ps1` equivalent for FilmCraft or the repository’s documented build helper.
The baseline VM is not adopted or reused across tasks.

### 11.2 UI, lifecycle and workflow tests

Existing anchors to extend or keep:

- `crates/ui-egui/tests/scripted.rs` `Driver::demo`, `frames`, `call`, `ok`, `exec`; add
  multi-pass and save/reopen cases.
- `crates/ui-egui/tests/ui_control_input.rs:153` screenshot/control isolation.
- `crates/ui-egui/tests/ui_dock_resize.rs`, `shared_docking_ui.rs`, `timeline_tabs_ui.rs`.
- `crates/ui-egui/tests/interface_zoom_ui.rs`, `settings_ui.rs`, `panels_ui.rs`,
  `monitor_geometry.rs`, `timeline_trim_ui.rs`, `clipboard_paste_ui.rs`, `project_panel_ui.rs`.
- `crates/ui-egui/tests/perf_ui.rs` monitor lag.
- `crates/ui-egui/tests/robustness_ui.rs` hostile UI state.

New/extended assertions:

1. **Lifecycle:** visible single/multipass; logic called per pass but background effects are
   idempotent; hidden `run_logic` ticks advance playback/autosave/workers/jobs without reading
   input; forced hidden paint with a screenshot delivers an image; buffered input survives a
   hidden tick and is handled once on the next visible pass; repeated hidden logic ticks keep
   one synthetic gesture step pending and do not collapse press/release; a forced two-pass
   click (including a first-show `Grid`) adds exactly one undo step; Escape drag cancels once.
2. **Root UI/geometry:** at 1600×980 and 1920×1080, `ui.elements` rects for
   `panel.Timeline`, `panel.Program`, `panel.Project`, `tools.Razor`, dock split handles and
   floating grips match the baseline within 1 point; panel bodies never overlap the
   header/status bar; tab overflow still reaches every tab.
3. **Screenshot callback:** two queued `ui.screenshot` requests with different panel crops
   return matching images/paths; a hidden screenshot delivers both when already issued and
   when still settling (queued-but-not-issued), using `RequestPaintWhileHidden` in the latter
   case; the weak callback wakes the live slot's waker after unlocking and never wakes itself;
   a callback after timeout/close is ignored; the completion slot is dropped exactly once; any
   resize/DPI/zoom change, including a change back, either retries within the deadline or
   errors, never pairs old pixels with new geometry. Native VM test; headless test of the
   correlation helper.
4. **Scroll header sync:** after horizontal scroll, media-browser/project-list headers align
   with rows (≤1 point); after rubber-band overshoot, the header uses clamped offset.
5. **Accessibility:** for the demo workspace assert `Role::Tab`/selected on panel tabs,
   `Role::Button`/selected on tool buttons, `Role::Button` "Resize panel" on the floating grip,
   `Role::Splitter` on dock split dividers, and `Role::TextInput` with a non-empty accessible
   name on caption/metadata/search fields. Use `Harness::get_by_label`,
   `accesskit_node().role()`, `label()`, `toggled()`. All 56 `TextEdit` sites must be named;
   add a negative control that removes a name and fails the strict kittest gate.
6. **Fonts:** with `CRAFT_FONTS_DIR` and without, assert Japanese/Chinese/Korean/Arabic/Hebrew
   sample text produces no missing-glyph UV and preserves the expected family/weight; assert
   `✔ ✓ ✖ ⚡ ⚠ ⚙ ♪ ☑` coverage; assert a non-zero TTC face index survives `FontData`
   construction. Fixtures and licences are pinned in §6.1.
7. **RTL:** Arabic/Hebrew caption visual order, right alignment, bracket mirroring, digits and
   a mixed LTR paragraph; editing caret/Home/End/Delete; compare glyph order, not just “text
   present”.
8. **IME/clipboard:** outgoing surrounding text after commit/deletion, preedit exclusion,
   grapheme/byte boundaries, password clearing, same-window switch, two windows, late serial,
   owner generation present when unavailable, available-empty vs unavailable and a
   pass-1-text/pass-2-`None` final pass; synthetic `PasteImage` maps to Paste once; native
   PRIMARY (keyboard and pointer serials, bounded transfer) and text/file/image paste matrix
   from §7.3.
9. **Save/reopen:** edit + effect + caption + tab layout, `file.saveAs`, `file.open`, equal
   `sequence.inspect`/`project.inspect` and reopened tab order; undo/redo count exactly.
10. **Mutation/negative probes:** a deliberately broken test-only flag that repeats a queued
    action must fail the one-undo-step assertion; a screenshot with a wrong token/slot must not
    be delivered to the wrong request; a scroll offset beyond content must clamp; a font face
    with the wrong index must produce a distinct glyph or missing glyph and fail the coverage
    test; removing an accessible name must fail strict kittest.

### 11.3 Visual comparisons and tolerances

Do not add committed screenshots unless they are needed; if added, they are original
FilmCraft captures and need `.attribution` sidecars and `ATTRIBUTION.md` rows
(`cargo xtask assets`). Prefer writing review PNGs to the durable evidence directory.

For before/after review:

- workspace chrome is custom-painted: mask only volatile monitor/video regions; use
  PSNR ≥ 45 dB, max channel difference ≤ 12, p99 ≤ 2 for unchanged regions, and exact layout
  rects within 1 point. These thresholds are provisional until the baseline report records
  the noise floor of an unchanged build; record the reference measurement that sets them.
- egui-drawn dialogs may move/resize because 0.37 changes button spacing and corner radius
  (#8617, #8445): mask the dialog region or use p99 ≤ 8/max ≤ 32, and require human visual
  review; do not auto-bless a changed golden.
- Arabic/Hebrew captions get both a pixel check and a glyph-order assertion; pixel-only
  comparison would hide double reversal.
- A changed golden is never accepted merely to make CI pass; re-run and review.

### 11.4 Native and browser workflows

Native Linux (proposed, through the managed adapter; no raw `cargo run` outside task.py):

```sh
python3 /home/codex/work/maintenance/task.py run \
  --job-id filmcraft-egui037-linux-ui-01 --kind build \
  --task filmcraft-egui037-migration \
  --source /home/codex/work/CraftApps/egui037-migration-20261022/filmcraft-implementation \
  --evidence /home/codex/work/ssd2/task-evidence/egui037-migration-20261022/filmcraft \
  -- cargo run --release -p filmcraft -- --control 9876
python3 /home/codex/work/maintenance/task.py status filmcraft-egui037-linux-ui-01 --tail-bytes 8192
# after the UI workflow, evidence capture and review:
python3 /home/codex/work/maintenance/task.py finish filmcraft-egui037-linux-ui-01 \
  --apply --owner-confirmed --checks-complete --evidence-preserved
python3 /home/codex/work/maintenance/task.py status filmcraft-egui037-linux-ui-01 --tail-bytes 8192
```

Drive `ui.inspect`, `ui.screenshot`, `ui.resize`, dock actions, clipboard/PRIMARY, and review
every PNG. Repeat under X11 and Wayland (and Hyprland for E4/W15P). Native Windows through a
uniquely owned clone: build the task checkout, run `filmcraft-ui-egui` UI tests, launch with
`--control`, capture screenshots, exercise IME and DX12/software graphics modes. macOS: real
hardware for Cmd shortcuts, titlebar drag, Japanese/Korean IME and header drag; if no Mac is
available, record macOS as unresolved, not passed.

Browser (proposed; `cargo xtask web --serve` is a long-running server, so all steps run in
one managed `task.py run` job with a trap that stops the server, then `status` and an attested
`finish` after evidence review):

```sh
# payload inside the managed job, with exact --task/--source/--evidence and a fresh job ID:
cargo xtask web --serve 8765 &
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null' EXIT
ffmpeg -f lavfi -i testsrc2=size=640x360:rate=30 -f lavfi -i sine -t 3 -c:v libx264 -pix_fmt yuv420p -c:a aac target/web/dist/web-test.mp4
node apps/filmcraft-web/tests/smoke.mjs --url http://127.0.0.1:8765/ --media web-test.mp4 --out target/web/smoke
```

Run WebGPU and `?webgl`; inspect `report.json` and steps `01-demo` … `05-modes`; check
`filmcraft.info()` for backend/compositor. A wasm compile is not runtime evidence; headless
browser tests do not prove native focus/IME/file pickers.

### 11.5 Environment provisioning gates

No current runtime acceptance exists. The following are gates, not passes:

| Environment | Current evidence | Required before claiming a pass |
|---|---|---|
| Linux X11 | No inherited `DISPLAY`; no Xvfb/window manager observed | Provision an isolated display/window manager, record versions, qualify native focus/clipboard/PRIMARY |
| Linux Wayland | No inherited `WAYLAND_DISPLAY`; no compositor/input-method observed | Provision Weston/Sway/Hyprland/KWin/Mutter and an IME, record capabilities; run text-input and PRIMARY matrix |
| Linux GPU | Parent inspection found i915 PCI `8086:3E96` and readable/writable `/dev/dri/renderD128`; worker sandbox may hide it; **do not claim the server has no GPU** | Managed renderer/device probe must identify the actual driver/backend; access alone is not GPU acceptance |
| Windows | Sealed baseline uses Microsoft Basic Render Driver; a full clone may not fit current capacity | Allocate a unique full clone or a baseline-backed overlay with an adequate reservation; run visible edited/saved/reopened workflows, IME, DX12/software |
| Browser | Wasm compile is not runtime evidence | Pin browser and adapter; run interactive WebGPU and WebGL2 fixtures; record raw/compressed wasm sizes and backend identity |
| macOS / physical Windows audio/video | No qualified environment in this task | Real hardware access and owned evidence; software VM results do not qualify these rows |

These rows remain explicit rollout blockers where unavailable. Do not report a GPU absent,
a current runtime pass, or a platform gate as satisfied from source inspection alone.

## 12. Performance: baseline, paired runs, thresholds

### 12.1 Baseline before production edits

Record before changing production code, on the exact baseline SHA and lockfile, as managed
`task.py run` jobs (fresh job ID, exact `--source`/`--evidence`, `status`, then attested
`finish --apply`). Submit each payload as
`task.py run --job-id <unique> --kind build --task filmcraft-egui037-migration --source
<checkout> --evidence <evidence> -- <payload>`; the commands below are payloads, not a
raw-command exception:

- `cargo xtask bench` with `--sections timeline,playback,scrub`, `--label
  filmcraft-egui036-baseline`; GPU and `--cpu` runs. The timeline harness currently defaults
  to 120 frames per view, 8 warmups (the `--repeat` value is a **run count**, not a sample
  count) and emits only p50/p95, so extend `crates/ui-egui/examples/bench/sections.rs` to
  accept the frame/warmup counts and emit p99 with exact sample counts **before** using it as
  acceptance evidence. Run at least 10 warmups, 30 timed iterations per steady configuration
  and at least 1,000 actual captured frames per view for any p99;
- `cargo xtask bench-playback` for `demo`, `stack3`, `stack3-blend`, `h264-1080`, `h264-2160`,
  `after-preview`, `seek-storm`, `--res full,half`, GPU and `--cpu`, with 10 warmups / 30
  timed iterations per configuration where the harness supports them; p99 requires at least
  1,000 frames;
- native idle: no playback, no input, 60 s; record repaint count, process CPU, RSS;
- startup: first present and `Renderer::new` timing on Windows DX12 and Linux Vulkan, from
  20 fresh processes;
- `perf.stats` before/after a fixed operation script (open demo, razor, undo, apply effect,
  play 5 s, screenshot);
- wasm raw and compressed sizes for the exact feature set.

Record: exact SHA, `Cargo.lock` hash, feature set, `CRAFT_FONTS_DIR` revision, fixture names
and hashes, OS/driver, GPU adapter/backend, DPI/scale factor, viewport size, profile (release),
thread count, theme, warmups and repetitions, raw samples, median/p95/p99, peak RSS, idle
repaint/CPU, operation latency, `gpu.uploadedBytes`, frame-cache entries, shapes/vertices, and
load average. For allocation counts use `heaptrack` or `valgrind --tool=massif` on the Linux
bench process; record peak/total allocations separately from product metrics. GPU uploads are
reported by `perf.stats` (`gpu.uploadedBytes`) and frame-cache counters.

### 12.2 Paired candidate runs

Run the same commands against the candidate on the same machine and environment, alternating
baseline/candidate where possible. Separate software-renderer (`--cpu`, llvmpipe/WARP) from
real-GPU results; do not mix rows. Never benchmark concurrently with builds or another noisy
workload and claim a small improvement. Preserve raw samples and the exact source/lock
identity for both sides.

Where the operation is equivalent, include a **stock 0.37 control** alongside 0.36 and the
selected fork. Fork-only app code need not compile unchanged on stock; use the same operation
in a pinned minimal adapter/harness and explain remaining semantic differences. Record all
three source/lock identities; unavailable equivalence is an attribution limitation, not proof
that an upstream regression is unavoidable.

### 12.3 Predefined thresholds and noise policy

These are the shared suite minimums; FilmCraft may tighten them, never silently loosen them.
The stock-0.37 equivalent control and the first baseline report are required before any
regression claim; thresholds and goldens are never auto-widened or auto-blessed to make a run
pass.

- **Sampling:** at least 10 warmups and 30 timed iterations for steady operations; at least 20
  fresh processes for startup; at least **1,000 actual captured frames** per configuration for
  any frame-distribution p99. A p99 from 20/30/50 samples is not a tail estimate. Report the
  exact frame count and sample count in the report schema.
- **Timeline UI** (headless kittest, 1000 clips/20 tracks, 1920×1080): median update/tessellation
  ≤ +5%, p95 ≤ +10%, p99 reported and reviewed. Vertices/shapes must not grow without
  explanation.
- **Playback CPU** (`bench-playback` CPU ms/frame, worker/source columns): ≤ +3% median;
  shown/dropped may vary with load and is not a gate alone.
- **Peak RSS:** FilmCraft gate is ≤ +5% for the same scenario after subtracting the measured
  noise floor; investigate any repeatable increase above the floor. Report absolute MB.
- **Idle:** no extra repaint requests and no extra CPU when nothing is playing; a hidden window
  keeps auto-save/playback semantics without busy-looping.
- **Startup:** report first-present and `Renderer::new`; on Windows DX12 a +20 ms median or
  more is a regression to investigate against #8736; do not silently accept it.
- **Wasm:** record raw and compressed sizes; a size regression is reviewed, not hidden by a
  compile-only check.

If a threshold is exceeded, re-run alternating; if still exceeded, profile (`perf`, `tracy`,
`heaptrack`/massif) and report. Never auto-accept a changed golden or call compile time UI
performance.

### 12.4 Separate workload rows

Do not merge these into one number:

- **#8731 text tessellation:** plain-text timeline/fixture workload (1000 clips/20 tracks,
  1920×1080), compare unchanged 0.36, stock 0.37 and the fork; measure tessellation time and
  text-heavy frame time separately. If confirmed, evaluate a row-level fast path and cached
  color-glyph metadata; test mixed/color rows. Do not bless a regression.
- **#8736 DX12 FXC shader compile:** Windows VM, measure `Renderer::new`/pipeline creation and
  first present separately from steady frames, with predictable filtering disabled. Compare an
  override-constant design with semantically equivalent wrap arithmetic only if needed;
  clamp-only is not a valid Repeat/MirroredRepeat fix.
- **#8512 system-font startup:** measure discovery/startup separately. The migration keeps
  `system_fonts` off, so this is a controlled opt-in experiment, not a default change; preserve
  glyph coverage and record provider inventory.
- **Wasm/browser:** raw and compressed wasm size plus WebGPU/WebGL2 runtime frame timings from
  the pinned browser environment; compile time is not UI performance.

## 13. Implementation PR stages and dependency DAG

No production stock-0.37 stage exists. The first production stage is the coherent fork graph.

| Stage | Contents | Depends on | Rollback |
|---|---|---|---|
| P0 | This plan only; no code | — | delete document |
| **P1** | **Atomic compatibility landing on the final fork graph.** Manifests/lockfile: final approved `storytold/egui` P8 + `storytold/winit` pins, final `storytold/craft-libs` revision (CL-1 + CL-2F), explicit `default_fonts`, `monochrome_emoji_fonts`, native `clipboard`, no `system_fonts`/`links`. API migration: `FontData`, `Role`/names, `ScrollArea`, `MouseWheelSource`, `run_ui` signature, `Id::unique`, screenshot callback + CL-2 helper, `PasteImage`, E21b custom IME adapter, bounded E4/W15P clipboard/PRIMARY routing, lifecycle split/multi-pass queue, strict kittest names/glyphs, and **removal of the manual `rtl::Lines` reversal and its unit tests**. Tests from §11. | P0; **G-PINS**, **G-CL2F**, published/reachable pins; capacity for gates | revert to the coherent 0.36.2 graph (`Cargo.toml`/`Cargo.lock` and source); keep old craft-libs `5c874d51` and `craft-docpath 82a24b0` |
| P2 | Language and the full native input matrix: Latin-layout shortcuts, `ctx.set_language`/`set_ui_language`, and the full W15P/E4 PRIMARY and E21b native rows against the P1 integration. Fork bidi is already active in P1; P2 does not introduce a second bidi state. | P1 (fork already pinned) | revert P2; restore prior language/input behavior without changing the P1 dependency graph or re-introducing manual reversal |
| P3 | Accessibility semantics beyond first-green naming: radio-group/tool semantics, richer role/value/label assertions, AccessKit language screenshots. | P1 | revert P3 |
| P4 | Evidence hardening: visual/geometry/screenshot regression harness, mutation probes, performance baseline/candidate and stock control, wasm sizes, environment provisioning. | P1–P3 | revert P4; keep earlier behavior |
| P5 | Documentation/rollout notes and optional measured experiments (`system_fonts`, color emoji, macOS). | P4 | revert P5 |

Publication is not part of P0. Each future push/new PR requires fresh direct human approval
after the parent shows the exact contents and validation. Do not include agent/model/tool
signatures in PR text or commits.

### Rollback set

Keep the old coherent set recoverable:

- `egui/eframe/egui_kittest = 0.36.2`;
- `craft-ui` rev `5c874d51eeb3de70e735c8ce87a59a7dedd47675`;
- `craft-docpath` rev `82a24b006eefa036b43c09a67fb9a3fbd17ffe63`;
- registry `winit 0.30.13`, `wgpu 30.0.1`.

A rollback is `git revert` of the stage commits plus restoring the baseline
`Cargo.toml`/`Cargo.lock` (or `git checkout abf44fda -- Cargo.toml Cargo.lock` after reverting
source changes). Do not keep a machine-local path patch as a fallback. A rollback rehearsal
uses the preserved baseline executable/source and a separate managed allocation.

## 14. CI, compatibility and publication checks

- `.github/workflows/ci.yml` runs `cargo xtask ci` with pinned `CRAFT_FONTS_REF`; add the pin
  checks from §3.4 and a `cargo metadata --locked` step.
- Keep `freebsd.yml`’s skip-tests policy and `windows-arm64.yml` as-is; do not weaken them.
- `cargo xtask wasm` must still check `filmcraft-ui-egui` and `filmcraft-web`; `egui_kittest`
  is a dev dependency and remains native-only.
- The web crate’s `wasm-bindgen` exact pin (`=0.2.129`) must match the installed CLI. The
  candidate’s own lock uses `wasm-bindgen 0.2.126` and `web-sys 0.3.103`; FilmCraft currently
  pins `web-sys 0.3.106` and `wasm-bindgen 0.2.129`. The `0.2`/`0.3` ranges are compatible,
  but the final FilmCraft lock is authoritative: record the resolved versions and run
  `wasm-bindgen --version` before `cargo xtask web`.
- `rfd 0.17`, `arboard 3.6.1`, `wgpu 30.0.1`, `accesskit 0.25`/`0.24` transitive and `winit
  0.30.13` compatibility were checked from manifests; a fresh `cargo metadata --locked` and a
  clean lockfile resolution are still required.
- The candidate’s own lock resolves `wgpu 30.0.0`; FilmCraft pins 30.0.1. Record the actual
  resolved candidate/FilmCraft version in the evidence and ensure one source; do not claim the
  candidate lock is FilmCraft’s.
- New 0.37 dependencies include `wl-clipboard-rs`, `tree_magic_mini`, `petgraph`, `nom`,
  `send_wrapper`, `unicode-properties`, `vello_cpu 0.2`, `glifo 0.3`, `kittest 0.5` and updated
  `accesskit*`/`phf`. Review their licences (all expected MIT/Apache-2.0/BSD/ISC); no GPL/LGPL
  may enter.
- Do not commit local screenshots as assets without sidecars/index rows. Keep review PNGs
  under the task evidence directory.
- No agent/model/tool attribution in PR titles, descriptions, commits or comments. Contributor
  credit stays username-only (`@skylartaylor`); do not add a name entry.

## 15. Blocking unknowns and acceptance criteria

These are gates, not future nice-to-haves. A failed or unavailable gate is an explicit blocker,
not a passing result.

1. **G-PINS — production fork pins.** The reviewed, published, immutable `storytold/egui`
   commit (E1-E21 + E21b/P7b + selected #8763/#8748 fixes) and `storytold/winit` commit
   (W15P + W27 + required W1-W27 behavior) are not known at planning time. Do not invent a SHA,
   do not use an aiongg branch as a production pin, and do not land a stock-0.37 intermediate.
   The exact SHAs, feature sets and provenance must be recorded before P1.
2. **G-CL2F — shared capture helper.** `craft-libs` must publish the CL-2 helper, DesignCraft
   and GridCraft adapters must be reviewed against it, and CL-2F must freeze the API before
   FilmCraft pins the revision. A branch head or path dependency is not acceptable.
3. **G-CRAFT-LIBS — shared baseline and port.** The final published `craft-libs` revision must
   build on the merged PR #199 base `3832c47ef68a0e6214f2c1e85f3526b6adcb686e` with the
   protected-group docking fix and lazy printing preserved; the PR-199 hunks
   (`8614e4a`/README `1764bca`) are historical provenance. `5c874d51` is the old FilmCraft
   manifest pin; `4919088` is historical GitHub main. `craft-docpath` stays at `82a24b0`
   unless a concrete cross-type reason appears.
4. **G-IME — native IME and E21b.** E21b/P7b must exist in the pinned fork with the complete
   shared contract: owner generation present even when `Unavailable`, available-empty distinct
   and explicitly sent, committed W27 disable/enable transitions with cache clear,
   authoritative completed-pass `None`, current-epoch serial-mismatch edits applied once with
   outgoing state deferred, and a 4000-byte payload extracted without a full-document copy.
   The FilmCraft graphics adapter must supply logical model/preedit/UTF-8 bytes, and Windows
   Japanese/Korean, X11 Vietnamese, Wayland text-input-v3 and macOS rows must run on real
   environments. No headless test is a substitute. macOS without hardware is explicitly
   unresolved.
5. **G-PRIMARY — W15P + E4.** The winit-owned PRIMARY path must be integrated, with native
   per-seat keyboard- **and pointer-triggered** selection using a fresh triggering serial,
   owner/seat changes, empty/absent offers, password exclusion, unchanged-owner non-reclaim,
   self-paste, and slow/trickling/oversized/canceled transfers under wlroots, KWin and Mutter.
   The transfer path is bounded (2 s total deadline, 16 MiB, 4 read workers, 2 writers with
   queue depth 8), claims ownership synchronously on the fresh input turn, serves
   asynchronously, and discards stale/canceled completions. The arboard/smithay path is a
   measured fallback only.
6. **G-SEL — P7c #8763/#8748.** Reproduce selection loss with a caption/metadata field and a
   custom toolbar action; P7c owns the selected, tested #8763 and #8748 fixes. #8764 is an
   unmerged proposal, not a release fix. A local workaround needs a reason and removal
   condition.
7. **G-PERF — performance and stock control.** Baseline and candidate runs must meet §12
   sampling and thresholds, with separate #8731 text, #8736 DX12, #8512 font and wasm rows, and
   a stock-0.37 control where equivalent. Compile duration is not UI performance.
8. **G-ENV — real environments.** Linux X11/Wayland compositors and IME, a browser with a
   pinned adapter, a unique Windows clone, and macOS hardware must be provisioned. The i915
   device’s presence is availability evidence only; the worker sandbox may hide it. No current
   runtime pass is claimed.
9. **G-ACCESS — accessibility names and glyphs.** All 56 `TextEdit` sites and other input
   widgets have names; strict kittest naming/glyph behavior is green without a global disable;
   monochrome emoji coverage is asserted.
10. **G-LIFECYCLE — multi-pass audit.** The input-action design, Escape latch, monotonic
    deadlines, one-step synthetic gesture retention across hidden logic ticks, idempotent
    toast/settle counters and grep allow-list are implemented and tested; until then the
    one-action and hidden-work guarantees are not met.
11. **G-SCREENSHOT — capture matrix.** The callback is delivered for a visible capture and
    for a hidden capture, including queued-but-not-yet-issued settling; timeout/close/
    late-callback behavior is correct. The CL-2 slot holds the strong waker, the callback is
    weak and does not wake itself, and admission bounds four outstanding requests, 128 MiB per
    image and 256 MiB aggregate with checked `width * height * 4` before issue and after any
    resize/DPI/zoom epoch change (including a change back).
12. **G-RTL — bidi.** P1 must land the fork's E5/E12 together with the simplified
    `rtl::Lines`, and validate visual order/alignment/editing. Do not ship both the manual
    reorder and fork bidi, and do not leave the removal to P2.

## 16. Limitations of this planning pass

- No build, test, benchmark, screenshot or app launch was run. All behavior statements are
  source review and upstream documentation, not fresh validation. No command in §11 was
  executed.
- The historical compiler diagnostics are from an earlier checkout and only identify likely
  sites; current line numbers and new errors must be discovered by implementation gates.
- The production fork pins and the final `craft-libs` revision are unknown until the fork and
  shared helper are published; no SHA is invented.
- The i915 device was observed on the host but no display session is inherited and the worker
  sandbox may hide it. Windows baseline is software-rendered. macOS, browser runtime and real
  Wayland compositor behavior remain unproven until the environment gates run.
- The exact stock-0.37 control harness may need small adapters for fork-only operations; such
  differences are recorded as attribution limitations, not ignored.

## 17. Review finding dispositions

This table is an index; the body sections above are authoritative.

| Finding | Disposition | Where resolved |
|---|---|---|
| F1 deferred-action clearing loses clicks | **Accepted.** No clear on pass 1+; input actions dispatch once in the pass with events; deferred actions persist and drain on the first non-discarded pass; duplicates coalesce; no silent drop. | §4.4, §9, §11.2 |
| F2 Escape rollback cannot fire on a multi-pass frame | **Accepted.** Latch Escape and the undo mark in pass 0; apply once after the final pass. | §2.3, §4.4, §9 |
| F3 hidden window never presents | **Accepted.** The screenshot command already requests paint while hidden; `RequestPaintWhileHidden` also covers queued-but-not-issued settling and other composition stages; timeout is not the normal hidden path; no visibility-based phase inference. | §4.2, §4.5, §11.2, §11.5 |
| F4 OS clipboard feature lost | **Accepted.** Add native `clipboard`; cross-application paste tests on X11/Wayland/Windows; web checked separately. | §3.5, §7.3, §11.1 |
| F5 stock 0.37 landable stage | **Accepted.** Stock is a local/CI comparison only; one coherent fork landing; final pins gated. | §3.2, §13 |
| F6 `default_fonts` not monochrome emoji | **Accepted.** Explicit `monochrome_emoji_fonts`, exact glyph set, screenshot diff. | §3.5, §6.1, §11.2 |
| F7 E21b/P7b IME gap | **Accepted.** Full owner/support/serial contract, 4000-byte no-full-copy payload, TextEdit producer, Film graphics adapter, tests; native rows required. | §7.2, §10.1, §11.2, §15 G-IME |
| F8 PRIMARY downgraded | **Accepted.** W15P + E4 required; keyboard/pointer serials, bounded transfer, native per-seat/password/unchanged-owner/slow-cancel matrix; arboard fallback measured only. | §7.3, §10.1-10.2, §15 G-PRIMARY |
| F9 effects catalogue incomplete | **Accepted.** Enumerates `trim.tick`, native menus, metadata/auto/sync sites, all monotonic deadline calls; trim idempotent; grep guard. | §2.3, §4.1, §4.6 |
| F10 performance protocol weak | **Accepted.** 10 warmups, 30 steady, 20 startup, ≥1,000 actual frames for p99; shared 5%/10%; stock control; separate #8731/#8736/#8512 rows; wasm sizes. | §12 |
| F11 unmanaged commands | **Accepted.** All validation uses `task.py run/status/finish` with task/source/evidence; proposals only. | §11.1, §11.4 |
| F12 EffectCraft reverse dependency absent | **Accepted.** Exact `2c5ba7a` pin, UI-free proof, separate branch retained, re-audit triggers. | §1.1 |
| F13 4919088 unpublished / PR 199 premise | **Premise rejected.** GitHub main verified `4919088`; stale `origin/main` `b1401e3` does not prove otherwise. Old `5c874d51` fetched; seven-file +607/−2 diff audited; printing and protected-group docking preserved; `craft-docpath` stays separate; PR 199 only hunks `8614e4a`/README `1764bca`. The actionable audit requirements are accepted. | §3.1, §10.3, §15 |
| F14 accessibility role contradiction | **Accepted.** Floating grip = `Button` "Resize panel"; dock split divider = `Splitter`; test updated once. | §5.3, §11.2 |
| F15 minor accuracy/robustness | **Accepted.** Catch-unwind latch; no `Window::show` in logic; wgpu/candidate versions stated; thresholds require a noise reference; strict kittest names are an ordering constraint. | §4.1, §4.2, §11.1, §11.3, §12.1, §14 |

### 17.1 Fresh v2 review findings

| Finding | Disposition | Where resolved |
|---|---|---|
| v2-1 P1 lands manual bidi reversal with fork bidi | **Accepted.** P1 now pins the fork, removes `rtl::Lines` reversal and its tests, and validates visual order; P2 no longer ships a second bidi state. | §6.3, §13, §15 G-RTL |
| v2-2 E21b contract incomplete | **Accepted.** Full owner/context identity, available-empty vs unavailable, committed W27 transitions, authoritative `None`, serial-mismatch-once/deferred-outgoing, no full-document copy. | §7.2, §10.1, §15 G-IME |
| v2-3 keyboard-only PRIMARY and no bounded transfer | **Accepted.** Keyboard and pointer triggering serials; 2 s/16 MiB/4 readers/2 writers/queue 8; synchronous claim, async serving, owner-specific waker, stale discard. | §7.3, §15 G-PRIMARY |
| v2-4 no screenshot wakeup/collection path | **Accepted.** Strong waker in the live slot, weak callback, unlock before wake, callback has no repaint path of its own; queued hidden requests use `RequestPaintWhileHidden`. | §3.3, §4.5, §15 G-SCREENSHOT |
| v2-5 performance commands contradict sampling rule | **Accepted.** Harness must emit p99 and exact counts, 1,000+ frames/10 warmups/30 iterations; `--repeat` is not a sample count. | §12.1, §12.3 |
| v2-6 validation mixes managed and raw commands | **Accepted.** Bench and browser payloads run inside `task.py run/status/finish`; the Linux UI job has an attested finish and the long-running server has a stop step. | §11.1, §11.4, §12.1 |
| v2-7 effect catalogue misses Toast and settle counters | **Accepted.** Toast uses monotonic time; settle counters are per rendered frame/pass identity; synthetic hook retains one step across hidden logic ticks. | §2.3, §4.1, §4.5, §15 G-LIFECYCLE |
| v2-8 consumer count and P7c traceability | **Accepted.** Fourteen capture consumers (eleven craft-ui + three local adapters); P7c explicitly owns #8763/#8748. | §3.3, §8, §10.1, §15 G-SEL |

## 18. Primary sources reviewed

- Upstream `egui 0.37.0` changelog and release commit `841a328b`; source inspected through
  `git -C /home/codex/work/CraftApps/egui037-migration-20261022/egui show 0.37.0:…`.
- Upstream API changes: emilk/egui#8489 (screenshot callback), #8555 (`WidgetType` -> `Role`),
  #8583 (`default_fonts` off), #8534 (Id deprecations), #8535 (`root_ui`), #8428 (PRIMARY),
  #8512 (system fonts), #8631/#8748 (logic-only/overdue windows), #8577 (bidi), #8731 (text
  tessellation), #8736 (DX12 shader compile), #8763 (TextEdit click-away) and proposed #8764
  (`6eb460a2a6d537d312a80579cb801895509deaed`, open/unmerged).
- Lifecycle source audit: `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/lifecycle-source-audit.md`.
- IME gap: `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/ime-surrounding-gap.md`,
  `.../winit-review-triage.md` for the W15P/W27/E21b serial and owner/support corrections, and
  egui plan §10.6 for pinned font fixtures.
- Shared baseline audit: `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/filmcraft-shared-baseline-audit.md`.
- Suite rollout: `craft-libs/docs/egui-037-suite-rollout.md` and
  `craft-libs/docs/egui-037-migration-plan.md` (CL-2/CL-2F, §7.8, §10.6).
- Fork provenance: `aiongg/egui` `craft/0.37` `PATCHES.md` (E1-E21) and `aiongg/winit`
  `craft/0.30.13-lang` `PATCHES.md` (W1-W27), inspected read-only in
  `/home/codex/work/CraftApps/egui037-migration-20261022/{egui,winit}`.
- Shared migration: `storytold/craft-libs#199` (port hunks `8614e4a`, README `1764bca`;
  stale heads `b68de8c`/`ec8840a` are not mergeable trees), current main `4919088`, old
  FilmCraft pin `5c874d51`, language work `storytold/craft-libs#198`.
- Local evidence: `/home/codex/work/ssd2/task-evidence/craft-fork-plan-20261020/PLAN.md`,
  `/home/codex/work/ssd2/task-evidence/craft-fork-ai-lines-20261022/SURVEY.md`,
  `/home/codex/work/ssd2/task-evidence/craft-fork-egui037-20261021/RESULTS.md`,
  `/home/codex/work/ssd2/task-evidence/egui037-migration-20261022/upstream-discussions.json`,
  `upstream-8764-fresh.json`, `craft-ui-consumers-exact.json`.
- Repository rules and commands: `AGENTS.md`, `CLAUDE.md`, `docs/architecture.md`,
  `docs/testing.md`, `docs/web.md`, `docs/agents.md`, `.github/workflows/ci.yml`,
  `xtask/src/main.rs`.


## Local implementation update — 2026-10-23

The user clarified that reviewed plans remain local and implementation proceeds before publication. The earlier requirement to publish/land this plan first is superseded; pushes and PR publication still require explicit approval.

The owned checkout's current branch HEAD is `fa550ce2d07bdfee6b1d7b6835e8abc8c6913d00`; the migration work is uncommitted on that base. The 2026-10-23 note that the checkout was fast-forwarded to `c5c16939896277ab332280a6d3f1d7283d0d0c4e` and earlier baseline SHAs above are retained as historical review evidence. Use the current HEAD and its exact manifest/lock hashes (table in §1) for local integration and paired measurements; remote main is not asserted here. The original refs remain reachable.

Preserve isolated MCP screenshot temporary directories and caller-path desktop captures; rejected synthetic gestures enqueue nothing (click 0–10, drag 2–1024). Retain the new typography/contrast/clip regressions and random benchmark output directories.

The complete static delta review and exact old/new object identities are retained in the task evidence: `canonical-diffs-refresh-20261023.json` and `overnight-consumer-review-20261023.md`. This amendment claims no new build, native GUI or performance acceptance.


## Final source review repair — 2026-10-24

The reviewed cumulative source identity and two findings are preserved in the external
`final-landing/filmcraft-final-review.md/json` evidence. Same-owner application caret/selection
moves (pointer, keyboard, Select All and explicit gfxEdit offsets) and another field taking
focus now invalidate only a known active/terminal-pending graphics composition before its late
replacement can mutate the moved selection. Stable owner/generation identity is preserved;
commit-only input without an old composition and uninterrupted replacement chains stay valid.
A private one-shot interruption asks the platform to cancel the old composition and survives
discarded passes until a final observation. No public IME protocol was added.

Automation lookup now uses the current registry whenever it is populated, so a removed control
cannot resolve to its previous-pass coordinates. The pre-draw previous-registry fallback stays.
New regressions cover removed IDs, real pointer press/release plus next-batch char/byte deletion
and commit, terminal-pending composition, same-owner control/keyboard moves, direct-commit and
valid-chain positive controls. These checks and fresh independent review are pending managed
validation; the earlier18-test result predates this repair. Canonical manifests/lock are unchanged.
