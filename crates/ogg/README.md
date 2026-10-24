# filmcraft-ogg

A clean-room Ogg demuxer. Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
`wasm32-unknown-unknown`. No libogg / libopusfile / FFmpeg code was consulted; FFmpeg is used only
as an external fixture generator and test oracle.

## Specifications

| Document | Used for |
|---|---|
| RFC 3533 (2003) | pages, lacing, packet continuation, CRC-32, logical bitstreams |
| RFC 7845 (2016) | Ogg Opus: `OpusHead` / `OpusTags`, granule positions, pre-skip, end trimming (§4) |
| RFC 6716 (2012) §3.1 | Opus TOC byte: packet durations |
| Vorbis I specification (2020-07-04), §4.2.2 and Appendix A | Vorbis identification header, Ogg mapping |

## Use

```rust
let bytes = std::fs::read("voice.opus")?;
let ogg = filmcraft_ogg::open(&bytes)?;
let s = ogg.stream_of(filmcraft_ogg::Codec::Opus).unwrap();
let head = &ogg.streams[s].headers[0];                      // OpusHead
let pre_skip = u16::from_le_bytes([head[10], head[11]]) as u32;
let timing = filmcraft_ogg::OpusTiming::of(&ogg.streams[s], pre_skip);
let k = timing.starts.partition_point(|&x| x <= 48_000) - 1; // packet holding sample 48 000
let packet = ogg.read_packet(&bytes, s, k)?;
```

- Every page is read and its CRC checked; damaged pages are dropped and the reader resynchronises
  on the next `OggS`. Packets spanning pages are lists of byte ranges; a continued page whose start
  was lost drops the fragment.
- `OpusTiming`: packet durations from the TOC; positions anchored on the first page's granule
  (backwards from it), then accumulated forwards, re-anchoring on any granule discontinuity; the
  final (end-of-stream) page's smaller granule trims the end. Positions are relative to the first
  sample after the pre-skip (pre-skip samples are negative).

## Tests

`src/tests.rs` writes synthetic pages: timing with pre-skip and end trimming, packets across pages,
a corrupted page (CRC), every truncation and 300 mutations. `crates/codecs/tests/it/ogg_oracle.rs`
compares with FFmpeg's libopus / Vorbis decodes (exact length; SNR ≥ 50 dB CELT, ≥ 10 dB SILK,
Vorbis 139 dB) and checks random seeks against the continuous decode.
