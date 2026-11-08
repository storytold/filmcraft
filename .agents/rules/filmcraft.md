<!-- craft-house-rules:start core source=dde09ea31e2e457f5b4653a6e76f7cdf55944bdf sha256=540e6cea75d12d0710c3dca89658328e81e508ecef102446ca5bb95b7c0b92bb -->
## House rules

These apply in every Craft repository. A later rule may be stricter, not weaker. The source is `standards/house-rules-core.md` in `storytold/craft-contrib`. Propose changes there, not in a copy.

1. **A person is accountable.** Whoever runs an agent answers for what it opens and for its review comments.
2. **Check, then claim.** Before writing code, read the issue comments, search pull requests for `#N` and the keywords, and check recent commits on `main`. Comment on the issue first. Release a claim you will not finish.
3. **Verify, don't guess.** Open a definition before using it. Confirm the cause in the code, even when someone names a location.
4. **Keep fixes narrow and tested.** Fix what the issue describes, where the bug is. Add a test that fails without the fix.
5. **Review the diff before opening a pull request.** Identifiers exist as used, the test fails without the fix, nothing unrelated changed, and the description is true.
6. **Look for a better pull request.** If another fixes the same thing, close yours when it is better or equal, and say why.
7. **Report only what ran.** Say what you tested and what you could not run. Without a local build or test, say so and rely on CI before marking ready.
8. **`main` changes through a pull request.** Do not push to it. Branch protection enforces this. There is no exception.
9. **Stop and ask only about** licensing outside the allowlist, publishing, releases, a new remote, secrets, spending money, destructive git, and anything else hard to undo. Otherwise keep working. `plan/` is a gitignored notebook, not rules. Without it, do not ask for it and do not stop.
10. **Keep secrets and personal details out** of the repository, logs and requests.
11. **CI stays shared.** Workflows call `storytold/craft-common-actions`. Do not change CI in a feature or fix pull request. If CI cannot cover a change, open an issue there.
12. **Guards say why.** A check that looks removable says what it prevents. Read that before changing it.
13. **Instructions must be readable by everyone.** No notes about one machine, and no links to files a reader cannot open. `craftrules` is retired. Do not link to it.
14. **Keep pull requests moving.** Concurrent pull requests are fine; unanswered actionable feedback is not. Respond in 7 days, a reminder at 14, closure eligible at 30, from when an actionable request waits on the author. Suggestions and unconfirmed bot findings do not start the clock. A fix, explanation or agreed plan resets it; unrelated activity does not. Waiting on a reviewer or maintainer pauses it and does not count against the author. A maintainer decides closure; it is never automatic. Reuse search results, do not poll, and stop at a rate limit.
15. **No agent attribution.** A pull request does not say an agent wrote it. A repository rule may not require that.
<!-- craft-house-rules:end -->
<!-- craft-house-rules:start rust source=dde09ea31e2e457f5b4653a6e76f7cdf55944bdf sha256=387e0ccb4b49e567f60951d7d6fff7305054c89f43ad812b9993dc7c38528013 -->
## Rust Craft repositories

These apply to native Craft apps and to shared Rust libraries. Select them by repository type. A repository that merely contains some Rust does not take this block. Rules later in a repository's file may be stricter, but not weaker. The source is `standards/house-rules-rust.md` in [storytold/craft-contrib](https://github.com/storytold/craft-contrib).

1. **Never crash.** Non-test code does not panic and has no `unsafe`, except where the repository file names an exception. Return errors, treat input as hostile, and bound recursion and allocations. New production crate roots copy the `deny` attribute from an existing crate: `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::unimplemented`, `clippy::todo`, `clippy::unreachable`. Every crash fix comes with a test that panicked before the fix.
2. **Clean room.** Learn from observed behaviour and public specifications. Do not copy another product's code, assets, strings or artwork. Check a dependency's licence before adding it.
3. **Assets and fonts.** Every asset is original or suitably licensed, and is recorded in `ATTRIBUTION.md` in the same change. Font files live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not in the app repository.
4. **Rust only for native Craft apps.** No handwritten JavaScript or TypeScript, and no Tauri, Electron or webview shell, in those apps. Shared tooling follows its documented technical scope and maintainer-approved exceptions. Craft PR Review's existing Tauri and TypeScript UI is one such exception. It does not relax this rule for native apps. Preserve any existing web build.
5. **Time goes through `chrono`.** Do not hand-roll calendar conversion. For dates beyond what `chrono` can represent, follow [standards/time.md](https://github.com/storytold/craft-contrib/blob/main/standards/time.md).
6. **Contributor credits.** Add or edit only your own person's entry in `contributors/people.toml`, and only after asking them. Never guess a name or hand-edit `contributors.json`.

## Licence allowlist

Dependencies and assets use SPDX identifiers. An expression is allowed when every identifier is in this list. A repository may name a shorter list. It may not add an identifier here by editing its own copy of these rules.

- `ISC`
- `MIT`
- `BSD-2-Clause`
- `BSD-3-Clause`
- `Apache-2.0`
- `OFL-1.1`
- `Ubuntu-font-1.0`
- `Bitstream-Vera`
- `CC0-1.0`
- `CC-BY-4.0`
- `CC-BY-SA-4.0`
- `LicenseRef-Public-Domain`

`LicenseRef-Storyteller-Trademark` is not in this list. It covers Storyteller brand marks only, where a repository already says so.
<!-- craft-house-rules:end -->

# FilmCraft — rules for agents and contributors

These rules apply to every human and AI contributor. `CLAUDE.md` holds the working instructions; this
file holds the rules that must never be broken. When the two disagree, this file wins.

## 0. Never crash

People trust FilmCraft with hours of work. A malformed file, a bad command or MCP parameter, a corrupt
preset or project, an odd keystroke or a full disk must give an error the user (or agent) can act on,
never a crash and never lost work. **This rule outranks feature work:** don't ship a feature by adding a
panic path, and fix a crash before building on top of it.

1. **Fail with `Result<T, E>`.** Return the crate's error type and propagate with `?`; add context
   (`map_err`, an error variant) instead of discarding it. In the UI, report the error (status bar, error
   dialog) and carry on.
2. **No panicking shortcuts outside tests:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`
   or `unimplemented!`. Use `?`, `ok_or(..)?`, `let Some(x) = .. else { return Err(..) }`, `if let`, or a
   fallback that is truly correct (never one that silently corrupts a project). An unfinished feature
   returns an "unsupported" error. The only exception is a call that is provably infallible from local
   code alone (a literal that always parses): a single-item `#[allow(clippy::expect_used)]` with the reason.
3. **No `unsafe`** (`unsafe_code = "forbid"` for the workspace). The single exception is
   `crates/platform`, which needs `unsafe` to call the operating system's media APIs (hardware
   video decoding through VideoToolbox, Media Foundation / Direct3D 11, VA-API and NVDEC; hardware encoding) and does nothing else. It
   uses `unsafe_code = "deny"` with `#[allow(unsafe_code)]` only on its FFI modules, a
   `// SAFETY:` comment on every `unsafe` block, a safe `Result`-returning public API, and the
   pure-Rust decoder as the tested fallback ([ADR 0001](docs/adr/0001-platform-ffi.md)).
4. **Every input-derived number is hostile.** Media files, project and interchange files, presets, fonts,
   CLI / MCP / control-channel parameters and UI state are untrusted. Index and slice with `get()` (or
   validate bounds once, up front, for a hot loop); slice strings only at char boundaries; use
   `checked_*` / `saturating_*` for lengths, offsets and counts; never divide by a value that can be zero
   (frame rates, timescales, sample rates, sizes); don't cast negative or NaN values to integers; don't
   call `clamp` with bounds that can cross; cap allocations sized by input.
5. **Bound recursion and loops.** Projects can be cyclic (nested sequences) or deeply nested: walk them
   with seen-sets or depth limits.
6. **Don't cascade.** Lock poisoning is not fatal: `lock().unwrap_or_else(PoisonError::into_inner)`.
   Every thread or job (export, previews, proxies, decoding workers…) runs under `catch_unwind` and
   reports a failure as an error: a dead worker leaves monitors blank or jobs "running" forever.
7. **Last-resort guard.** The panic hook (`crates/ui-egui/src/crash.rs`) logs every panic, and the UI pass
   runs under `catch_unwind`, so an escaped panic becomes an error window and the session (and unsaved
   project) survives. It is a safety net, not a substitute for rules 1–6. Keep `panic = "unwind"`.
8. **Prove it.** Every crash fix comes with a small synthetic regression test that panicked before the
   fix. Parsers and decoders get mutation-fuzz tests (truncation, bit flips, corrupt sizes) run under
   `catch_unwind`; new commands get hostile-parameter tests. Fuzzer findings become tests.
9. **Enforced:** clean crates carry `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic,
   clippy::unimplemented, clippy::todo, clippy::unreachable)]` (moving to `[workspace.lints.clippy]` once
   every crate is clean). `clippy.toml` allows them in tests: a failing test should fail loudly.

## 1. Assets: no Adobe artwork, every asset licensed and attributed

This rule is absolute. Breaking it is the most serious mistake a contributor can make on this project.

**What counts as an asset:** any image, icon, cursor, logo, illustration, screenshot, font, colour LUT,
preset, template, sound, music, video, 3D model, or other non-code media. This covers files in the
repository, bytes embedded in code (`include_bytes!`, base64, data URIs), and data hard-coded to
reproduce an image (for example, point lists traced from someone else's icon).

1. **Never use Adobe iconography, images or other Adobe assets**, in any form:
   - no icons, cursors, logos, splash screens, UI artwork or screenshots from any Adobe product;
   - no Adobe fonts, LUTs, presets, templates, sound effects, stock media or sample projects;
   - no traced, redrawn, recoloured or "inspired-by" copies of Adobe icons. Our icons may follow generic
     conventions (a play triangle, a razor blade, a stopwatch), but each must be drawn from scratch
     without reference to Adobe's artwork.
2. **Every asset must be open**: open-source licensed (MIT, Apache-2.0, BSD, ISC, zlib, SIL OFL…),
   public domain / CC0, or Creative Commons (CC BY or CC BY-SA; no NC/ND licences), **or** created by a
   contributor who owns it and licenses it to the project under the project licence (MIT OR Apache-2.0).
   If the licence is unknown or unclear, the asset does not go in.
3. **Every asset needs an attribution sidecar.** Next to each asset file `X`, add `X.attribution` with:
   ```text
   asset:        <file name>
   title:        <what it is>
   author:       <creator / copyright holder>
   source:       <URL, or "original work" for contributor-made assets>
   license:      <SPDX id or licence name>
   license-file: <path to the licence text, if the licence requires shipping it>
   added:        <YYYY-MM-DD> by <contributor>
   notes:        <how it was made or modified; for screenshots, what is shown>
   ```
   Also add one line for the asset to [`ATTRIBUTION.md`](ATTRIBUTION.md). `cargo xtask assets` (part of
   `cargo xtask ci`) fails if any asset lacks a sidecar or index entry.
4. **Screenshots** in the repo may show only FilmCraft (or other open projects), with media we generated
   or media that is itself openly licensed. Never commit screenshots of Adobe products.
5. **Local reference material stays local.** Premiere reference screenshots and notes live only in
   `plan/premiere/` (gitignored). They must never be committed, bundled, embedded, traced or shipped.
6. **Code-drawn assets** (icons in `crates/ui-egui/src/icons.rs`, procedural demo footage in
   `crates/media`, procedural looks in `crates/render`) are original work under the project licence
   and are listed in `ATTRIBUTION.md`.
7. **When in doubt, leave it out** and draw or generate it yourself.
8. **The one exception: first-party ArtCraft brand marks.** The ArtCraft name and logos in `docs/brand/`
   are trademarks of the ArtCraft Team, not open source, usable only unmodified and only in the context of
   FilmCraft under `docs/brand/LICENSE-brand.txt`; forks and modified versions must remove them. They still
   need a sidecar and an `ATTRIBUTION.md` row (licence `LicenseRef-ArtCraft-Trademark`). No other
   non-open asset is allowed, and this exception never covers third-party marks (Adobe, Discord, GitHub
   and other logos stay out; draw a generic icon instead).
9. **Fonts live in [storytold/craft-fonts](https://github.com/storytold/craft-fonts), never in this
   repo.** Don't commit new font files (the small Latin UI fonts already in `assets/fonts/` stay). A
   font FilmCraft needs is added to craft-fonts, which the app reads through the optional build input
   `CRAFT_FONTS_DIR=<absolute path of a craft-fonts checkout>` (`crates/text/build.rs` embeds the fonts in its
   `fonts/manifest.txt` as `filmcraft_text::fonts::CRAFT_FONTS`; unset, it is empty and the app uses its
   bundled and system fonts). Never add craft-fonts to a `Cargo.toml`. Code using `CRAFT_FONTS` must work
   when it is empty.

## 2. Clean-room code

- Never read, disassemble or copy anything inside Adobe application bundles; file names and listings only.
  Observe behaviour by using the app; never capture its Home screen, recent projects, account info or
  file browsers.
- Never copy GPL/LGPL/AGPL code (FFmpeg, x264, x265, MLT, Kdenlive, Shotcut, Olive, LAME…).
- Implement codecs and formats from public specifications (ITU-T/ISO/IEC standards, IETF RFCs, the VP9
  and AV1 bitstream specs, SMPTE documents, published container specs). Do not read the source of
  reference or third-party decoders/encoders while implementing a format, even permissively licensed
  ones (libvpx, libopus, libaom, dav1d, openh264…); spec text and conformance vectors only. Record the
  spec edition used in the crate README.
- ffmpeg/ffprobe may be used only as external test oracles and fixture generators. They are never
  linked, bundled or shipped.
- Dependencies must use permissive licences: MIT/Apache-2.0/BSD/ISC/Zlib/Unicode/CC0/BSL-1.0, or
  MPL-2.0 used unmodified.

## 3. Engineering rules

### 3.1 Never crash

See [§0](#0-never-crash). It outranks every other engineering rule.

### 3.2 The rest

**Invariant — move identity uniqueness:** A move plan lists each clip identity at most once.
The core move operation rejects repeated ids before mutation or id allocation; placing one identity
on multiple tracks makes selection and later identity-based editing ambiguous. Copies receive new ids
and retain each entry's requested destination. Destination track creation and the edit commit together.
In an idle session, a rejected duplicate move retains the project and its undo and redo history.
Selection-derived edit plans keep each identity once in first-occurrence order before expanding
linked partners; explicit move requests with repeated identities are still rejected.

- **Layering** ([`docs/architecture.md`](docs/architecture.md) §1, enforced by `cargo xtask layers`): nothing below L5 depends on egui/eframe/winit/rfd/cpal. L0 codec/container crates depend only on `filmcraft-bitstream`.
- **Exact time:** all time is `filmcraft_time::Tick` (254 016 000 000/s). Never use f64 seconds for edit math.
- **Everything is a command** (`crates/engine`): id, label, menu path, shortcut, params, enabled(), run(). UI, CLI, control channel and MCP all dispatch by id.
- **Everything is agent-drivable:** every interactive widget registers an automation id; UI state is serde so the control channel can read and set it.
- **Fonts** are the optional build input `CRAFT_FONTS_DIR` (`crates/text/build.rs`). Unset, the build must still pass. Never add craft-fonts to a `Cargo.toml`.
- OS media APIs go through Rust bindings in `crates/platform` (the one crate allowed `unsafe`; `docs/adr/0001-platform-ffi.md`), optional, behind traits, with our decoders as the fallback.
- **Time goes through `chrono`** (workspace dependency). Never hand-roll UTC/calendar conversion (days-to-date algorithms, leap-year math); a crate owns only its output format string. The one exception is range extension: past the years chrono can represent, shift the date by whole 400-year Gregorian cycles (146,097 days, after which the calendar repeats), let chrono compute the month and day, and add the cycles back to the year (splitting seconds into hours, minutes and seconds is plain arithmetic).
- **Quality gates** before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, layers, assets, wasm).
- New workspace crates include `[lints] workspace = true` in `Cargo.toml`. Production crate roots declare the crate-level lint attributes before modules; crate-level `//!` documentation may precede them.
- **Commits:** one task id per commit (`M2.4: CABAC residual decoding`). Only green states.
- `cargo run -p filmcraft -- --control 9876` opens the desktop app (`docs/control-protocol.md`). For UI work, look at `ui.screenshot`. Generate fixtures with ffmpeg into `target/fixtures/` and do not commit media (`docs/testing.md`).
- `plan/` is an optional gitignored notebook (`plan/premiere/` may hold local screenshots). Do not ask for it and do not stop when it is absent. Public work lists are [`ROADMAP.md`](ROADMAP.md), [`docs/gaps.md`](docs/gaps.md) and [`docs/agents.md`](docs/agents.md).

## Contributor credits (About window)

- About ▸ Contributors/Models are compiled into the binary from `contributors/contributors.json`
  (commit stats; never hand-edit) and `contributors/people.toml` (names people chose for
  themselves). See `docs/contributors.md`.
- **Agents working for a contributor:** when you prepare a PR, check whether your human's GitHub
  username has a `[people.<username>]` entry in `contributors/people.toml`. If not, ask them once
  whether they want to be credited by more than their username: a real name, a display name, and/or
  their public GitHub profile name (`sync_github_name = true`). If yes, add **only their own** entry
  (copy the template at the top of the file)
  and include it in their PR, committed as them. If no, change nothing: they are credited as
  `@username` anyway.
- Never add, edit, guess or copy anyone else's entry or name (not from git config, commit authors or
  GitHub profiles). Never hand-edit `contributors.json`.

## CI workflows
- Workflows in `.github/workflows/` call the shared workflows in [`storytold/craft-common-actions`](https://github.com/storytold/craft-common-actions) at `@main`, with their inputs and secrets. No other jobs: if no shared workflow does what's needed, it doesn't go here.
- Don't add or change CI in a feature or fix PR. If the existing workflows can't build or test a change, link an existing craft-common-actions issue or PR, or open a new issue, or a PR only for a small fix that is safe for every app and does not increase CI time.

---

# User Rules & Additions

## 1. CodeRabbit & PR Bot Hygiene (Zero-Tolerance)
- **100% Docstring Coverage (Mandatory)**: Every function, method, private helper, and `#[test]` function touched by the PR diff MUST have a clear Rustdoc `///` comment directly above it. CodeRabbit enforces an 80.00% threshold on all touched functions with cyclomatic complexity >= 2 (including private helpers and tests); we enforce **100.00%** across all touched functions so coverage never drops below 100%.
- **Mandatory Pre-Commit Docstring Audit**: Before creating or amending any PR commit, run this verification script against `upstream/main` and require `0` undocumented functions:
  ```bash
  python3 -c '
  import subprocess, re, sys
  base = subprocess.check_output(["git", "merge-base", "upstream/main", "HEAD"]).decode().strip()
  files = subprocess.check_output(["git", "diff", "--name-only", base, "HEAD"]).decode().splitlines()
  missing = []
  for f in [x for x in files if x.endswith(".rs")]:
      lines = open(f).read().splitlines()
      diff = subprocess.check_output(["git", "diff", "-U0", base, "HEAD", "--", f]).decode()
      touched = set()
      for d in diff.splitlines():
          m = re.match(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", d)
          if m:
              s, c = int(m.group(1)), int(m.group(2)) if m.group(2) is not None else 1
              touched.update(range(s, s + max(c, 1)))
      for i, line in enumerate(lines):
          m = re.match(r"^\s*(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?(?:const\s+)?fn\s+(\w+)", line)
          if m:
              has_doc, j = False, i - 1
              while j >= 0 and (lines[j].strip().startswith("#[") or lines[j].strip().startswith("///") or lines[j].strip().startswith("//")):
                  if lines[j].strip().startswith("///"): has_doc = True
                  j -= 1
              depth, started, end = 0, False, i + 1
              for k in range(i, len(lines)):
                  depth += lines[k].count("{") - lines[k].count("}")
                  if "{" in lines[k]: started = True
                  if started and depth <= 0: end = k + 1; break
              if any(l in touched for l in range(i + 1, end + 1)) and not has_doc:
                  missing.append(f"{f}:{i+1}: fn {m.group(1)}")
  if missing:
      print("ERROR: Missing /// docstrings on touched functions:\n" + "\n".join(missing))
      sys.exit(1)
  print("OK: 100% docstring coverage on all touched functions.")
  '
  ```
- **No Standalone Contributor Commits**: Never commit changes to `contributors/people.toml`, `CONTRIBUTORS.md`, or metadata files in isolation. Always squash contributor additions directly into the feature or fix commit.
- **No AI Slop / Trivial Assertions**: Never write trivial test assertions (`assert!(!px.is_empty())` on fixed-size buffers, or `let _ = run(...)` on hostile inputs). Always assert pairwise distinct outputs for enum/preset variants and `all(|v| v.is_finite())` on hostile inputs.
- **Pre-Flight Clippy & Formatter Gate**: Before pushing any commit or branch, run `cargo fmt --all -- --check` and `cargo clippy -p <crate> --all-targets -- -D warnings`. Ensure 0 warnings.
- **No Panicking or Overflowing Code (Rule 0)**:
  - Absolutely no `.unwrap()`, `.expect(...)`, `panic!()`, `todo!()`, or `unreachable!()` in non-test code (including examples). Return `Result<T, E>` and propagate with `?`.
  - Use `saturating_add`, `saturating_sub`, and `checked_*` when computing pixel/raster coordinates (`x0`, `y0`, `w`, `h`) from user/effect positions so extreme coordinates never overflow `i32` in debug/overflow-checked builds.

## 2. FilmCraft Git & Branch Workflow
- **Dual-Track Workflow (PRs vs Daily-Driver)**:
  - **PR Track**: Every PR branch (`feat/<name>`, `fix/<name>`) MUST branch directly off `upstream/main` (`git checkout -B feat/<name> upstream/main`). Never stack unrelated PR branches on top of release branches or other unmerged feature branches. Maintain 1 clean atomic commit per PR.
  - **Daily-Driver Track (`local/main`)**: Persistent local integration branch `local/main` merges `upstream/main` + all active feature branches so local editor (`~/.local/bin/filmcraft`) runs with all features intact.
  - **Sync Automation**: Run `./scripts/sync-local.sh [feature-branch]` to sync `upstream/main` into `local/main`, merge feature additions, and recompile.
- **Keep Remote Fork Synced**: Keep `origin/main` fast-forwarded to `upstream/main` (`git push origin upstream/main:refs/heads/main`) and push PR branches to `origin` (`git push --force-with-lease origin feat/<name>`). Never push feature branches directly to `upstream`.

## 3. Motion Graphics Templates (.fcgt), Fonts & Effect Standards
- **Bundled Default Fonts Only**: Default font choices in effects and built-in templates MUST use a bundled font guaranteed to exist in every build (`"Inter"`, `"JetBrains Mono"`, or `"Noto Serif"`). Optional fonts (e.g. `"Stalinist One"`) may be offered as secondary choices only when fallback behavior is explicit.
- **Discrete Choice Controls (`ControlKind::Choice`)**: Never bind a `ParamValue::Choice` layer parameter to a `ControlKind::Slider`. Always use `ControlKind::Choice` so the UI presents a combo box and `graphics.template.set` receives an unsigned integer choice index (`0..N`).
- **Animated Template Progression & No Conflicting Sliders**:
  - Built-in templates describing an animation (e.g. burning wipe, slide-in) MUST include keyframes across the template duration so the animation plays out of the box.
  - Never expose a static `TemplateControl` slider on a keyframed parameter (such as `burn_progress` or `opacity` when keyframed) — editing it fights the keyframe curve.
- **TemplateControl 1:1 Layer Target**: `TemplateControl` binds strictly to a single `u64` layer UID (`layer: u64`). Never expose styling controls as "global" when they only alter 1 layer in multi-layer presets.
- **Accurate Layer Visibility Labels**: Name toggle controls after the exact layer they target (e.g. `"Show Row 4 Plate"` vs `"Show Row 4"`).
- **Cross-Platform Directory Resolution**: In examples or generators writing `.fcgt` files, never hardcode `HOME` / Linux paths. Mirror FilmCraft's standard data directory resolver (`FILMCRAFT_DATA_DIR`, `APPDATA`, macOS `Library/Application Support`, `XDG_DATA_HOME`, fallback `./`) and accept CLI args / env overrides.
- **PR Description Parity**: PR descriptions and summaries must strictly match actual template and effect features.

## 4. Feature Completion & Local Binary Deployment
- **Mandatory Release Compilation from `local/main`**: Once a new feature is implemented, tested, and quality gates pass (`cargo test` / `cargo xtask ci`), deploy it to the local binary by syncing into `local/main`:
  ```bash
  ./scripts/sync-local.sh <feature-branch-name>
  ```
  This merges the feature into `local/main`, builds in release mode, installs to `~/.local/bin/filmcraft`, and verifies execution while keeping PR feature branches clean.
- **Deployment Verification**: Verify binary installed and runs (`~/.local/bin/filmcraft --help` or exit 0) before reporting task complete.
