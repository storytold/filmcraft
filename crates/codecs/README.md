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

AV1 pixel-format labels use `av1C` depth, monochrome and chroma fields, following
the AOM AV1 ISO Media File Format Binding v1.3.0, section 2.3.4. Matroska
AVC/HEVC/AV1 labels use their parsed codec configuration, and AVC includes its
profile. Missing or inconsistent AV1 configuration keeps the format unknown;
VP9 metadata retains its existing behavior. `tests/video_metadata.rs` covers
generated AV1 MP4 (8/10-bit), AVC MKV and HEVC MKV (10-bit) files, including
opening and decoding their first frame. Module tests cover 12-bit, monochrome,
and invalid configurations.

## Verification

Run `cargo test --release -p filmcraft-codecs` and the workspace's
`cargo xtask ci` gates. The integration tests use ffmpeg only as an external
fixture generator and oracle, with generated media under `target/fixtures/`.
Module tests also exercise synthetic headers and malformed input. No media
fixtures are committed to this crate.
