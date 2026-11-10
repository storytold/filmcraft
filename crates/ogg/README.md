# filmcraft-ogg

A clean-room Ogg demuxer and page writer. Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
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

## Writing

```rust
/// An Ogg Opus file from an `OpusHead`, 20 ms packets, the pre-skip and the playable length.
fn ogg_opus(opus_head: &[u8], packets: &[Vec<u8>], pre_skip: i64, samples: i64) -> Vec<u8> {
    let mut w = filmcraft_ogg::PageWriter::new(0x4643_4F50);
    let mut file = w.header(opus_head);                               // first page: beginning of stream
    file.extend(w.header(&filmcraft_ogg::opus_tags("FilmCraft", &[("TITLE", "Mix")])));
    for (k, p) in packets.iter().enumerate() {
        file.extend(w.packet(p, (k as i64 + 1) * 960));               // granule position at the packet's end
    }
    file.extend(w.finish(Some(pre_skip + samples)));                   // end of stream, end trimming
    file
}
```

- Header packets get pages of their own, as RFC 7845 §3 and the Vorbis mapping require.
- Audio pages close at 4 KiB or 50 packets (`with_page_target` changes both), always before a
  packet, so the last packet stays for the end-of-stream page.
- Lacing values of 255 and a shorter last value per packet, at most 255 per page; a packet that
  does not fit continues on the next page (continued-packet flag), and a page on which no packet
  ends gets granule position -1. Every page has its sequence number and CRC-32.
- `opus_tags` and `vorbis_comment` build the comment headers; field names that are not valid
  (empty, `=`, outside 0x20–0x7D) are left out.

## Tests

`src/write_tests.rs` writes files with `PageWriter` and reads them back with `open`: every
packet, the headers on their own pages, flags, sequence numbers and CRCs, a packet spanning three
pages, page targets and the Opus end trimming. `src/tests.rs` writes synthetic pages: timing with pre-skip and end trimming, packets across pages,
a corrupted page (CRC), every truncation and 300 mutations. `crates/codecs/tests/it/ogg_oracle.rs`
compares with FFmpeg's libopus / Vorbis decodes (exact length; SNR ≥ 50 dB CELT, ≥ 10 dB SILK,
Vorbis 139 dB) and checks random seeks against the continuous decode.
