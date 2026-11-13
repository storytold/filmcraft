# filmcraft-codecs

The L2 container and codec hub connects media sources to FilmCraft's software and registered
hardware decoders. It provides container openers, GOP-aware video seeking, packet audio decoding
and standalone audio imports. See the crate-level API documentation in `src/lib.rs` and
[`docs/architecture.md`](../../docs/architecture.md) for the source and decoder interfaces.

## PNG-coded video in MOV / MP4 (#402)

A `png ` sample entry stores one whole PNG image file per sample (ISO/IEC 15948 — the W3C PNG
specification, Second Edition; the `image` crate decodes the deflate stream). `png_mov.rs` reads
each sample as straight RGBA8 and hands the alpha channel to the compositor, so graphics renders
with transparency come in without a transcode. Pictures are intra-only, so any sample decodes on
its own and the GOP cache can seek to any frame. The sample entry's `has_alpha` comes from the first
sample's IHDR colour type, since a PNG track can mix pictures with and without an alpha channel.

QuickTime Animation (`rle `) is deliberately not supported: its bitstream has no public
specification, and [AGENTS.md](../../AGENTS.md) §2 allows implementations from public specs only.
Such a track fails with `CodecError::Unsupported("no decoder for QuickTime Animation video (fourcc
\"rle\")")`, which the engine's slate shows with the file name, so the cause is visible instead of
"unknown".

## Symphonia audio failure handling (#2798)

Symphonia remains an unmodified MPL-2.0 dependency. Its decoder construction, packet decoding,
sample conversion and reset calls are guarded against unwinding. On targets built with
`panic = "unwind"`, a panic becomes a `CodecError::Decode` asking the caller to reopen the media.
A packet decoder that panicked is quarantined: later decode calls fail, and reset never calls
into its potentially damaged state.
Because `PacketDecoder::reset` has no error return, a panic during reset is logged and reported
by the next decode. A newly constructed decoder can be used to retry.

Standalone imports guard the whole attempt, including format probing, packet reading and
decoder construction. A decoder panic aborts the import even if earlier packets decoded;
partially decoded samples are not returned as a successful source. Ordinary returned errors
retain the existing behavior: packet decoding returns an error without quarantining the
decoder, and standalone imports skip corrupt packets. This change does not change that
pre-existing damaged-file policy or harden the separate H.264/HEVC worker paths in #2798.

The default `wasm32-unknown-unknown` target uses `panic = "abort"`, so decoder panics cannot be
caught or converted to `CodecError::Decode` there. The guard handles Rust unwinding; it cannot
recover from a process abort or an allocation failure that aborts. It does not replace input
validation or repair bugs inside a dependency.

## Tests

`src/audio_panic_tests.rs` wraps a real PCM decoder through Symphonia's public `Decoder` API
with per-instance synthetic decode/reset failures. No dependency implementation is copied.
The tests check errors after a successful packet, no reuse after panic (including reset),
fresh-decoder recovery, ordinary-error recovery, constructor failure, and rejection of partial
standalone imports. WAV truncations and header mutations run under `catch_unwind`; all fixtures
are generated in memory. Existing MP3, AIFF, FLAC and Vorbis tests cover the normal
audio paths with FFmpeg used only as an external fixture generator/oracle.

```sh
cargo test -p filmcraft-codecs --lib audio::panic_tests
FILMCRAFT_REQUIRE_ORACLES=1 cargo test -p filmcraft-codecs
cargo xtask ci
```
