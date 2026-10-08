# filmcraft-comfyui

The ComfyUI bridge (L2): run any ComfyUI workflow on a ComfyUI server and bring its outputs back
as bytes. User-facing behaviour and the `comfyui.*` engine commands: [docs/comfyui.md](../../docs/comfyui.md).

- `workflow`: API-format workflows (ComfyUI ▸ Workflow ▸ Export (API)): validation (editor files
  are refused with a hint), the literal inputs of every node with their kind, seed inputs, and
  applying overrides; links between nodes are never overridden.
- `Recipe` / `Binding`: what a ComfyUI clip stores in the project (workflow, overrides by node +
  input as a value or a local file to upload, output nodes, server).
- `protocol`: the server's JSON: `/prompt` (and its refusals with node errors), `/history`
  (outputs of every kind, execution errors), `/queue`, `/view`, `/upload/image`.
- `client`: the `Transport` trait (blocking GET / POST) and the `Client` that uploads input files,
  queues, polls with progress and stop, and downloads the wanted media outputs.
- `http` (feature `http`): the transport over HTTP(S) with ureq and pure-Rust TLS (rustls +
  RustCrypto, the OS certificate verifier), as the speech-model downloader uses.
- `fake`: `FakeComfy`, an in-process server for tests and headless sessions.

Nothing here touches the file system or knows about projects. The feature is off by default and
never enabled for wasm (`cargo xtask wasm` checks the crate without it).

The protocol follows ComfyUI's public HTTP routes as seen from a client; no ComfyUI code is used.
`tests/http_roundtrip.rs` runs the HTTP transport against a minimal look-alike server on a local
socket.
