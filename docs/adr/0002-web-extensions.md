# ADR 0002: experimental desktop web extensions

- **Status:** proposed; runtime and API acceptance require maintainer review
- **Scope:** optional workflow UI extensions, separate from video/audio effect processing
- **Related:** [G11](../gaps.md), [ADR 0001](0001-platform-ffi.md)

## Context

FilmCraft's internal commands serve several frontends, but they do not provide an installable
third-party interface inside the desktop editor. A workflow service may need its own buttons,
progress display, lists and interactions rather than a host-generated parameter inspector.
Embedding a particular service's panel into the main application does not provide that extension
model.

## Proposed design

1. The desktop application has an optional, default-off `web-extensions` feature. The first runtime
   targets macOS only. Engine, renderer and project persistence do not depend on a webview.
   Native runtime dependencies stay in the desktop application, above the headless engine boundary.
2. The user installs a `.fcplugin` archive through an Extensions manager, reviews its declared
   permissions, and explicitly opens it. Packages are stored in a dedicated application data area.
   No project directory or current working directory is scanned to execute plugins automatically.
   An installation receipt records the package digest and enabled state. The manager checks that
   digest when loading and opening a package; updates require a new installation review. The digest
   is an integrity check, not publisher authentication. Limits are 32 installed packages and four
   running windows.
3. A package contains a versioned manifest and its own HTML, CSS and JavaScript. FilmCraft opens a
   managed, resizable window using an OS webview. The extension owns the window contents. No external
   browser, separately installed companion app or user-started server is needed.
4. Window execution uses a helper subprocess of the same executable. The editor owns its lifetime
   and communicates through a dedicated bridge. There is no native dynamic-library plugin loading
   and no browser access to the general control server.
5. A versioned SDK exposes `invoke`, events and host theme variables. The first API is only
   `host.info`, `document.snapshot`, `playhead.seek` and `markers.add`. Each privileged method checks
   the installed package's permission at the host boundary. An extension cannot grant itself
   privileges by changing its JavaScript.
6. Mutations name the target document and sequence. Marker creation also carries the observed
   revision, goes through the existing engine command path and participates in undo. Context data
   does not expose local media paths. Integer frames and rational frame rates preserve exact time.
7. Network access is an explicit permission with declared HTTPS origins. Local resources use a
   dedicated extension origin; Content Security Policy restricts connections and executable content.
   CORS remains enforced. No native fetch proxy is introduced to bypass it.
8. Install, open, disable and remove are visible application actions. Package validation, errors and
   runtime failure are reported without discarding the current project. The package and protocol
   parsers use bounded inputs and reject unsupported values.

## Runtime and licence decision

The repository's product rule is pure Rust. Its only explicit workspace `unsafe` exception is
`crates/platform` for OS media APIs, and ADR 0001 specifically excludes other OS integration from
that exception. This proposal does not extend it. A safe Rust webview/window wrapper still depends
on platform runtimes; that dependency is an architectural choice to review, not proof that the
existing media exception covers it.

The proposed Rust wrappers must carry licences allowed by AGENTS.md, and the dependency/lockfile
change must include their transitive dependencies. This runtime uses the macOS system WebKit
framework and does not redistribute it.

Windows and Linux are unsupported in this first experiment. Their extension backends require
separate implementation, dependency-policy review and validation. Native extension dependencies
are enabled only for the macOS desktop application. The browser/WASM editor continues to build
without the desktop runtime.

## Boundaries and limitations

- The helper process separates extension-window work from the editor's UI process. It is not an OS
  sandbox, a formal memory quota, or a claim that malicious content cannot exhaust system resources.
  Host message bounds and permissions must be enforced independently of the JavaScript SDK.
- Archives and manifests are untrusted input. The decoder rejects unsafe paths and links and bounds
  file counts and expanded sizes. Assets stay in an in-memory map; ZIP entries are not extracted to
  the filesystem. Installation writes the original archive plus its receipt and must avoid partial
  installations. An installation action must not be replaced by ambient discovery of executable files.
- SDK calls have bounded request size/concurrency and a timeout. A timed-out edit is ambiguous to
  the caller and is not automatically retried. Host callbacks must avoid blocking the editor on a
  helper response.
- There is no arbitrary engine command dispatch, native executable hook, filesystem bridge or
  credential vault. The normal automation interface is not an extension API.
- The window is floating in v0. Docking, marketplace discovery, signatures, automatic updates,
  media export/import jobs, streaming and effect processing are separate proposals.
- `apiVersion: 1` is experimental. A published compatibility policy, migrations and supported host
  version range are still required before declaring a stable ecosystem.
- The manager registers automation IDs and exposes host commands. The existing egui `ui.elements`
  interface does not inspect arbitrary HTML controls inside an extension window. Extension-content
  automation remains an open design question under the repository's agent-drivable UI rule.

## Alternatives

An internal service-specific panel is simpler for one integration but requires editor source
changes for each UI. A shared generated form constrains workflows that need custom interfaces.
An external browser or companion application does not satisfy the in-editor entry point. A
native-library ABI increases the crash and compatibility surface. A WASM effect runtime addresses
per-frame processing, but does not by itself supply an author-owned workflow UI. These approaches
can coexist later; none is implicitly selected for this UI experiment.

## Acceptance evidence

Before this proposal can be considered ready to merge, record:

- Maintainer acceptance of the runtime, dependency policy, process boundary and experimental API.
- Green repository quality gates, including default builds and the WASM editor, plus checks of the
  enabled desktop feature and its target-specific dependencies.
- Tests for malformed packages, bounded archive decoding and messages, permission denial, stale context
  rejection and undoable changes through the bridge.
- Actual desktop runs of the two neutral examples, including install, open, resize, theme change,
  close/reopen, disable/remove and runtime failure. Report untested platforms explicitly.

The [extension guide](../extensions.md) documents the package and API contract. No roadmap item is
marked complete by this proposed ADR.
