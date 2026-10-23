# AV1 initial metadata hardware acceptance

These opt-in Windows tests include production Media Foundation AV1 DXVA output and injected
native-failure checks, plus a software-backed replay regression. The hardware checks fail when
a decoder is unavailable. Ordinary workspace runs leave these tests explicitly ignored.

From the repository root, generate six small inputs with external FFmpeg/ffprobe on PATH:

```powershell
./crates/platform/tests/support/generate_issue1678.ps1 -OutputDirectory target/fixtures/issue1678
$env:FILMCRAFT_ISSUE1678_FIXTURE_DIR = (Resolve-Path target/fixtures/issue1678).Path
cargo test --release --locked -p filmcraft-platform --test issue1678_generated -- --ignored --nocapture --test-threads=1
```

FFmpeg is a fixture generator only. Its libraries are neither linked nor shipped. The script
uses procedural testsrc2 footage, refuses to overwrite files, caps each file at 8 MiB and the
combined set at 25 MiB, and records hashes, versions and ffprobe metadata. Do not commit the
generated media. Use an outer process watchdog when collecting acceptance evidence.

Each actual file has 48 frames, two GOPs and 640×360 4:2:0 pictures. The donor changes only
AV1 color-description codes. The test copies only its configuration OBUs into the actual
sample entry, preserving the actual compressed samples and container color declaration:

| Signal | Actual primaries/transfer/matrix | Provisional codes | Depth |
|---|---|---|---|
| SDR | 1/1/1 | 6/6/6 | 8 |
| PQ | 9/16/9 | 1/1/1 | 10 |
| HLG | 9/18/9 | 1/1/1 | 10 |

The baseline stand-in demonstrates strict hybrid rejection before native input. The positive
path uses the production factory and checks hardware counters, absence of fallback, frame
count, timestamps, dimensions, chroma, bit depth, color and pixels against FilmCraft software.
Reset at the second GOP and MP4/GOP frame requests exercise accepted-header reinjection and
container color handling. A real `MfDecoder::fail_after` case removes the second GOP's sequence
header, forces a native failure, and checks exact software replay and a subsequent headerless
seek. The software-backed stand-in isolates the same replay boundary without claiming GPU
evidence. PQ/HLG fixtures test declared signal metadata; testsrc2 is not a
calibrated HDR scene and no mastering/content-light metadata is added.

Generated fixtures do not reproduce the reporter's AMD capture. Record adapter/driver and
AV1 Video Extension versions alongside results; keep issue 1678 open until a permitted
reporter reproduction is checked on the affected system.
