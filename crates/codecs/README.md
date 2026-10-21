# filmcraft-codecs

The L2 container and decoder hub. MP4/MOV, Matroska/WebM, MXF, MPEG TS/PS, AVI,
Ogg and standalone audio sources expose the same `MediaSource` interface. Video
uses GOP-aware seeking; audio packets are decoded and cached per stream.

## Metadata

Sources report container, timing, video format, colour and audio stream metadata
through `MediaInfo`. The engine exposes this through `mediaBrowser.probe` and
Media File Properties. Unknown metadata should stay unknown rather than be
inferred from a decoder's defaults. Colour information comes from container
metadata and codec headers; explicit Interpret Footage settings take precedence
in the renderer.

## Verification

Run `cargo test --release -p filmcraft-codecs` and the workspace's
`cargo xtask ci` gates. The integration tests use ffmpeg only as an external
fixture generator and oracle, with generated media under `target/fixtures/`.
Module tests also exercise synthetic headers and malformed input. No media
fixtures are committed to this crate.

AC-3 and E-AC-3 MP4 audio uses `dac3`/`dec3` (ETSI TS 102 366 Annex F)
for the primary program's layout rather than the mandated stereo sample-entry
field. Missing or invalid descriptors fall back to bounded primary syncframe
headers. Dependent E-AC-3 substreams remain outside the decoder's output.
`ac3_mp4_layout` forces the generic count to two, checks all six decoded channels
against FFmpeg, and checks random access.
