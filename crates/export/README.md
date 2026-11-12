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

## Which encoder wrote the file (#3952, #4339)

The export result says which encoder wrote the stream, and why a hardware encoder did not.
Three fields, all absent from the JSON when they are empty:

```json
"encoder":    "NVENC H.264"      // readable text: "{name} {format}"
"encoder_id": "nvenc"            // stable identifier
"declined":   "two-pass VBR"     // why the hardware encoder declined, when one did
```

`encoder` is for people and may be reworded; `encoder_id` is for programs, and its vocabulary is
fixed: `software`, `nvenc`, `videotoolbox`, `media_foundation`. `VideoEncoder::id` supplies it and
defaults to `software`, as `VideoEncoder::name` does, so an encoder that overrides neither is a
software encoder - and an encoder that overrides `name` must override `id` too, or its hardware
export would report the identifier of a software one. The Media Foundation encoder lives on
[#2488](https://github.com/jahvari/filmcraft/pull/1)'s branch and answers `media_foundation` there.

A hardware encoder that declines is not an error: the software encoder writes the export instead,
and `declined` says why the hardware one did not take it. The reason belongs to the export that
had it - it is cleared per export, and the first reason is the one reported, as with the first
failure in the registry walk. `declined` is absent when nothing was declined, including when
Hardware encoding is **Off**, where nothing was asked.

An image sequence writes stills, and so does every report that is not an export (masks, narration,
previews, proxies, scene detection): they name no encoder and leave all three fields out. `Report`
is `Deserialize`, so a result written before these fields still reads.

`encoder_name_tests` covers the whole path: a factory that declines one export only (the encoder
registry is process-wide, so it declines by path) proves the report and the status line; two
declines keep the first reason; an export with no decline after one reports none; a two-pass export
reports the encoder of its final pass; and the JSON carries the fields only when they are set.

