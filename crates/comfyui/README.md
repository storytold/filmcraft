# filmcraft-comfyui

The ComfyUI bridge (L2): run any ComfyUI workflow on a ComfyUI server and bring its outputs back
as bytes. User-facing behaviour and the `comfyui.*` engine commands: [docs/comfyui.md](../../docs/comfyui.md).

- `workflow`: API-format workflows (ComfyUI ▸ Workflow ▸ Export (API)): validation (editor files
  are refused with a hint), the literal inputs of every node with their kind, seed inputs, and
  applying overrides; links between nodes are never overridden.
- `Recipe` / `Binding`: what a ComfyUI clip stores in the project (workflow, overrides by node +
  input as a value or a local file to upload, output nodes). A recipe never names a server: a
  project file can be shared, so the server is the user's setting (a `server` key is ignored).
- `Workflow::key`: a workflow's identity (its node ids and classes, not their values), for
  settings kept per workflow such as the inputs exposed in the ComfyUI window.
- `protocol`: the server's JSON: `/prompt` (and its refusals with node errors), `/history`
  (outputs of every kind, execution errors), `/queue`, `/view`, `/upload/image`.
- `client`: the `Transport` trait (blocking GET / POST, and a streaming download) and the
  `Client` that uploads input files, queues, polls with progress and stop, and streams the wanted
  media outputs into a `Sink` (the engine writes them to disk; `MemorySink` keeps them in memory).
- `http` (feature `http`): the transport over HTTP(S) with ureq and pure-Rust TLS (rustls +
  RustCrypto, the OS certificate verifier), as the speech-model downloader uses. Output files are
  streamed, never read into memory whole.

Everything a server sends is capped: `MAX_FILE` (2 GiB per output file), `MAX_FILES` (64 files
per run), `MAX_RUN_BYTES` (8 GiB per run), `MAX_OUTPUTS` (1,024 outputs read), `MAX_TEXT`
(64 KiB per text output), JSON answers at 64 MiB; uploads at `MAX_UPLOAD` (1 GiB).
- `fake`: `FakeComfy`, an in-process server for tests and headless sessions.

Nothing here touches the file system or knows about projects. Which local files may be uploaded
(only those the user chose in the session, or the project's own media) is the engine's rule. The feature is off by default and
never enabled for wasm (`cargo xtask wasm` checks the crate without it).

The protocol follows ComfyUI's public HTTP routes as seen from a client; no ComfyUI code is used.
`tests/http_roundtrip.rs` runs the HTTP transport against a minimal look-alike server on a local
socket.
