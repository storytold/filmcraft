# Intel VPL 2.15 ABI comparison

This first-party C oracle checks public API declarations only. It is not linked
into FilmCraft, never calls MFX functions and requires no VPL import library.
The Rust product remains pure Rust and does not require an SDK at build time.

`headers.json` pins eight public headers and the Intel MIT license at revision
`c45b5d786bf7cdabbe49ff1bab78693ad78feb78`. Acquire each `files[].source` into its
matching `files[].path` inside ignored build storage. Verify both `bytes` and
`sha256` before compiling. Do not substitute another SDK checkout or fetch any
implementation source. Keep `MFX_DISPATCHER_EXPOSED_PREFIX` and
`ONEVPL_EXPERIMENTAL` undefined.

In an actual Windows x64 MSVC developer shell, with these verified headers under
`include/` in the current ignored directory, compile the oracle by absolute path:

```powershell
cl.exe /nologo /TC /std:c17 /W4 /WX /I include /DM0_SIGNATURES_ONLY /c <oracle-source> /Fosignatures.obj
cl.exe /nologo /TC /std:c17 /W4 /WX /I include <oracle-source> /Felayout.exe /Folayout.obj
.\layout.exe > layout.json
```

The signature object intentionally refers to ten native symbols; never link or
run it. The separate layout program excludes those symbol references and links
only the C runtime. Record compiler architecture/version, source/header hashes,
commands, exits and complete output. Compare the resulting sizes, alignments,
offsets and constants with `intel_vpl::abi_tests`; a compile warning or difference
is a failed comparison, not permission to disable warnings.

Run the focused Rust tests with `cargo test --release --locked -p
filmcraft-platform --lib intel_vpl::abi_tests`. Declaration tests and the oracle
establish no driver loading, codec availability, native ownership or hardware
performance. Those require the later isolated adapter/session implementation.
