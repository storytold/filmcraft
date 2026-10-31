# filmcraft-avi

A clean-room AVI demuxer, written from Microsoft's published AVI RIFF file reference and the
OpenDML AVI File Format Extensions 1.02. Layer L0: no dependencies beyond `std`, no `unsafe`, builds
for `wasm32-unknown-unknown`. No FFmpeg, libavformat or other AVI implementation was read; ffmpeg
and ffprobe are used only as external **test oracles**.

## API

```rust
use filmcraft_avi::{open, StreamKind};

let file = open(&bytes)?;                    // any ByteSource: a slice, a file, a web Blob…
for (i, s) in file.streams.iter().enumerate() {
    match s.kind {
        StreamKind::Video => println!("{:?} {} fps", s.video.as_ref().map(|b| b.compression), s.rate as f64 / s.scale as f64),
        StreamKind::Audio => println!("format 0x{:04x}", s.audio.as_ref().map_or(0, |w| w.format_tag)),
        _ => {}
    }
    let first = file.read_chunk(&bytes, i, 0)?;   // frame data, on demand
}
```

## What it reads

| Part | |
|---|---|
| Headers | `avih`; per stream `strh`, `strf` (`BITMAPINFOHEADER`, `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE`), `strn` |
| Index | OpenDML `indx` super index → `ix##` standard indexes (field indexes too), else `idx1` (offsets relative to `movi` or absolute), else a scan of the `movi` lists |
| Large files | `RIFF AVIX` extensions: through the OpenDML index, or scanned after `idx1`, which only covers the first RIFF |
| Key frames | from the index; a scan marks every chunk and says so (`keyframes_known`) |
| Damage | sizes clamped to the file (a cut-off recording opens with what is there), `rec ` lists nested at most 3 deep, tables bounded by what the file could hold |

`filmcraft-codecs` maps the streams onto decoders (`AviSource`): Motion JPEG, H.264, HEVC and
uncompressed RGB / YUV video; PCM, MP3, MP2 and AC-3 audio.

## Tests

`cargo test -p filmcraft-avi`: hand-built files for every index path (`idx1` relative and absolute,
`rec ` lists, OpenDML standard indexes, a scan), truncation, and every single-byte mutation and
truncation of them (never a panic). `tests/ffmpeg_oracle.rs` checks ffmpeg-made files (Motion JPEG,
H.264 with B-frames, BGR24, YUY2; PCM and MP3) against ffprobe; the ignored `opendml_file_past_1_gb`
checks a 1.16 GB OpenDML file with `AVIX` extensions.
