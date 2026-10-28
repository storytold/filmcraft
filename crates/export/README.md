# filmcraft-export

Renders sequence ranges, encodes video and audio, and writes export files with progress and
cancellation support. The UI, CLI and engine use the same pipeline.

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

`settings_tests::mp4_faststart_preserves_video_and_audio_for_every_output` covers disk files,
reserved handles and memory sinks, checking top-level box order, sample offsets, video decoding
and audible AAC audio. It also checks the existing MOV layout. A second regression verifies that
a write-only reserved handle returns an I/O error without unwinding or reporting success.

Suggested verification:

```sh
cargo test -p filmcraft-export mp4_faststart
cargo test -p filmcraft-export -p filmcraft-isobmff
cargo test -p filmcraft-engine proxies_tests
cargo xtask ci
```
