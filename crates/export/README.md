# FilmCraft export

The export pipeline renders sequence frames, encodes them and writes the selected container.
See [architecture](../../docs/architecture.md) for formats, settings, presets and queue behavior.

## MP4 upload compatibility (#3459)

MP4 video exports finalize with the existing ISO BMFF muxer's fast-start layout: `ftyp`, then
`moov` metadata before `mdat` media data. This lets progressive players and upload services read
the track metadata before downloading the media. The choice follows the container, so it applies
to H.264, HEVC and AV1 MP4 exports with software or hardware encoders. QuickTime MOV and
audio-only export layouts are unchanged.

The muxer shifts media in place with a bounded copy buffer and updates sample offsets; it does
not load the entire disk export into memory. Disk outputs therefore need read and write access.
Callers supplying `ExportSettings::output_file` for MP4 video must reserve a readable, writable
file. Buffered writes are flushed before reads through that same handle. In-memory download
sinks use their existing seekable byte buffer. Read, write and seek failures propagate as export
errors.

## Animated GIF transparency

Animated GIF supports the existing `ExportSettings::alpha` option (#3396). In Export mode,
choose Animated GIF and enable Video ▸ Include Alpha Channel. Engine and MCP exports use
`file.exportMedia` with `"format": "gif", "alpha": true`; queue entries and presets use the
same settings. The default remains off, flattening the picture over black.

GIF has binary transparency, not partial alpha. After rendering and scaling, RGBA8 alpha
0–127 becomes transparent and 128–255 becomes opaque. Visible pixels retain their straight
RGB color before palette quantization; transparent pixels share a cleared color. This avoids
darkening partially transparent edges, but cannot preserve smooth translucent edges or shadows.
The UI displays the threshold alongside the checkbox.

The existing `image` GIF encoder writes the animation; no codec implementation is added.
Format reference: [GIF89a, 31 July 1990, §23 Graphic Control Extension](https://www.w3.org/Graphics/GIF/spec-gif89a.txt).

`settings_tests::gif_alpha_threshold_and_disposal_roundtrip` exports synthetic moving stripes
with alpha 0, 127, 128 and 255, followed by an empty frame. Decoding verifies transparency,
color, frame timing and removal of previous-frame pixels, with alpha both enabled and disabled.
When FFmpeg is installed, its independent decode must agree on every pixel's alpha and opaque color.
`settings_tests::gif_palette_quantization_preserves_isolated_transparency_and_dark_pixels`
checks two frames with more than 256 distinct post-conversion RGBA colors, forcing palette
reduction. A single transparent pixel moves between frames, surrounded by opaque black and
near-black pixels. Decoded alpha must match exactly; opaque RGB channels allow at most 16/255
quantization error. This test uses the Rust decoder and needs no FFmpeg installation.

Run the GIF export tests with `cargo test -p filmcraft-export --lib gif_ -- --nocapture`.
Run all export unit tests with `cargo test -p filmcraft-export --lib`.

`filmcraft-ui-egui`'s `export_ui::gif_export_exposes_binary_alpha_option` uses the control
channel to select GIF, toggle alpha and inspect the explanatory label. Set
`FILMCRAFT_UI_SNAPSHOT_DIR` to render its Export-mode screenshot.

## Strict H.264 packet-copy joins (#4516)

Native CLI/API callers can opt into `ExportSettings.videoMode = "copyRequired"`.
The default is `"render"`, including old presets. This is compressed-sample copying,
not a lossless encoder setting or a decoded-frame fast path: the source AVC sample
bytes, codec configuration and per-sample composition offsets go to the MP4 writer
without calling a source decoder, compositor or video encoder.

```
filmcraft-cli --project edit.fcproj export joined.mp4 --format h264 --no-audio \
  --settings '{"videoMode":"copyRequired"}'
```

The first supported case is **video-only MP4, whole-file hard cuts** of compatible
native `avc1` files: same SPS/PPS/configuration and video parameters, timescale and
presentation origin; CFR at the sequence rate and size; limited-range BT.709,
progressive, square pixels, identity track matrix and no display crop. Whole-file
B-frame sources are supported when their constant decode grid and reordered
presentation grid cover exactly the complete clip; their composition offsets and
single initial edit are preserved. No partial-GOP smart rendering is attempted.

Gaps/overlaps, transitions, nested/generator/subclip media, proxy attachments,
interpretation overrides, retiming/reverse/frame holds, modified clip effects or
motion, output scaling/rate/color changes and graphics/caption overlays reject.
Frame-accurate trims never silently snap to a keyframe. Parameter sets changed
in-band, fragmented inputs, unsupported edit lists, non-CFR timestamps, rotation,
non-BT.709/unknown color signaling, or differing codec configurations also reject.
These restrictions are deliberately conservative, not claims that every rejected
case is inherently impossible to remux. Encoder quality/profile/bitrate controls do
not rewrite copied samples; the source configuration is retained. Explicit intra-only
or level conversion requests reject.

`ExportError::CopyIneligible { code, detail }` carries a stable reason code. CLI/engine
errors retain `copyRequired[code]: detail`; this mode **never** falls back to rendering.
Successful reports include `copied_video_frames`; ordinary rendered exports report 0.

All source descriptions, sample bounds, AVCC lengths, random-access starts, exact
sample/display timestamps, codec compatibility and edit coverage are checked before
creating output. Allocations/file counts are bounded; packets are read on demand.
The destination must be a new file (existing paths, including source aliases and
hardlinks, are not truncated). Sources must remain unchanged during export. Cancellation
or an I/O error during writing can leave an incomplete new destination for diagnosis;
remove that owned partial file before retrying. In-memory/pre-reserved-file outputs
and the stepped `Exporter` API are not supported in this initial native path; use
blocking `export` (the native engine runs it on its normal background job worker).
Audio stays a separate export/mux concern for this MVP.

Tests generate sources using Filmcraft's own encoder: bit-identical copied payloads,
exact rational 24 and 30000/1001 fps timing, intra-only and reordered B-frame files,
distinct compatible sources, own/optional FFmpeg decoded pictures, preserved input
files/hardlinks, and strict rejection/cancellation. Malformed AVC length/header
mutations run under `catch_unwind`; no media fixtures or external runtime codec
dependency are added. Run `cargo test -p filmcraft-export packet_copy`.
