# ADR 0002: optional vendor SDK decoders in a helper process

- **Status:** proposed (for the owner decision asked for in #383; not in effect until accepted)
- **Issues:** #383 (vendor SDK bridges for camera RAW), #1111 and #1415 (Nikon and RED raw);
  sibling proposal #1954 (Blackmagic RAW through a runtime loaded at run time)
- **First user:** the `red-sdk` feature of `crates/platform` ([r3d-ne.md](../r3d-ne.md))

## Context

FilmCraft's rules are pure Rust in the product, clean-room codecs from public specifications,
permissive dependencies, and `unsafe` only in `crates/platform` for OS media APIs
([ADR 0001](0001-platform-ffi.md), [AGENTS.md](../../AGENTS.md) §0 item 3, §2). Some camera RAW
formats have no public specification: R3D and R3D NE (RED, Nikon ZR), BRAW (#290), ProRes RAW
(#345). Their vendors' SDKs are the only decoders, and decoding them otherwise would mean reverse
engineering, which the project does not do. Under the current rules those formats cannot be opened
at all; editors who shoot them transcode first.

#1954 proposes one shape for this: load a vendor's runtime library at run time, in the editor's
process, through FFI in `crates/platform`. That fits a vendor that ships a runtime with a stable,
COM-style C ABI and headers under a permissive licence. RED's SDK is different: a C++ API, a static
library that has to be linked at build time, runtime libraries beside it, and an agreement that
keeps its headers and libraries out of anyone else's distribution. Linking it into the editor would
mean C++ FFI into the app process, and the SDK's threads, memory and crashes inside it.

## Decision (proposed)

An optional decoder built on a vendor SDK is acceptable when all of the following hold. Anything
that does not meet them needs a decision of its own.

1. **Off by default.** It is a Cargo feature named after the SDK (`red-sdk`), never in any crate's
   default features, never enabled by CI, `cargo xtask ci`, the release packaging or any official
   artefact. Without it none of the backend's code is compiled.
2. **Built only against an SDK the user supplies.** An environment variable names the user's own
   extraction of the SDK (`RED_R3D_SDK_DIR`); without it the feature build stops with a message
   saying so, and on an unsupported target with a message naming the supported ones. Nothing is
   downloaded, and no SDK file (headers, libraries, documentation, sample code, LUTs) and no camera
   footage is ever committed or bundled.
3. **The vendor's code runs in a separate helper process.** The adapter is an original program
   compiled against the SDK by the crate's build script (in the SDK's own language: one C++ source
   file for RED's, the only part of the repository that is not Rust, and compiled only with the
   feature). The editor never links or loads the vendor's libraries. It starts the helper, talks
   to it over a versioned, framed protocol that both sides check, and treats every reply as hostile
   input under the never-crash rules: bounded sizes and counts, deadlines, a watchdog. A crash, hang
   or refusal in the helper becomes a media error for the frames concerned and a helper started
   again, never a crash or a frozen window.
4. **`unsafe` stays under ADR 0001.** The backend lives in `crates/platform` (layer L5). Its only
   `unsafe` may be one module that maps the helper's decoded frames read-only (shared memory, through
   the official `windows` bindings or `libc`), with ADR 0001's containment: `#[allow(unsafe_code)]`
   on that module alone, a `// SAFETY:` comment on every block, a safe API, no callbacks into Rust,
   and no Rust invariant that depends on the other process keeping to the protocol. Everything else,
   the protocol included, is safe Rust.
5. **The default build is unchanged.** `cargo fmt`, `cargo clippy --workspace --all-targets -D
   warnings`, the workspace tests, `cargo xtask layers`, `assets` and `wasm` pass and behave the same
   without the feature; no default-build test needs the SDK; the layering is unchanged. A file the
   feature decodes is still recognised without it and reported as unsupported with a message naming
   the feature, without reading the file into memory. Tests that need the SDK compile only with the
   feature, and tests that need footage are `#[ignore]` behind an environment variable naming the
   user's own clip.
6. **What the editor builds on stays generic.** Model changes the backend needs (a development
   settings model, a frame format, a playback hint) are written for any camera RAW source, tested
   without the SDK, and useful to a public-spec decoder such as DNG or CinemaDNG.
7. **Evidence comes with the change.** CI cannot build the feature (it has no SDK), so a pull request
   that touches such a backend carries its author's runs of the feature's clippy, unit tests and
   real-clip tests on every platform it supports, and the backend's document records measurements
   by hardware class.
8. **The project ships no SDK-enabled build.** Publishing the adapter's source is reviewed against
   the vendor's agreement in the backend's document. Distributing a build with the SDK linked in
   is a separate decision, which would have to meet the vendor's conditions (for RED: an end-user
   licence for the helper and RED's libraries, a private install directory, notices kept, no use of
   RED's marks). Until such a decision, users build the feature locally against their own SDK.

## Alternatives considered

- **Public-spec formats only.** Camera RAW without a specification stays out of FilmCraft; such
  backends live in forks. The simplest rule, and the default if this record is rejected.
- **A runtime loaded in the editor's process** (#1954). Suits a vendor runtime with a C ABI and
  permissively licensed headers, and avoids a process hop. For a C++ SDK linked at build time it
  would put C++ FFI and the vendor's threads and crashes in the editor. The two shapes can coexist;
  the owners may prefer one pattern for every vendor SDK, and this record would then follow it.
- **An out-of-tree plug-in protocol.** The cleanest separation, but it asks the project to design and
  keep a stable decoder-plug-in interface before there is a second user of it.

## Consequences

- People who shoot R3D NE can build FilmCraft that edits it natively, with the SDK they downloaded;
  everyone else is unaffected.
- The repository gains one non-Rust source file and a build script path behind the feature, and the
  platform crate one more `unsafe` module. Changes to them need FFI-level review.
- CI cannot verify the feature. Reviews rest on the author's runs, and a regression in it can go
  unnoticed until someone with the SDK builds it.
- Users of the feature build from source; the project's releases do not change.
