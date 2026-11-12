# filmcraft-oggmux

A clean-room Ogg page writer and the Ogg Opus / Ogg Vorbis comment headers. Layer L0: no
dependencies beyond `std`, no `unsafe`, builds for `wasm32-unknown-unknown`. No libogg or FFmpeg
code was consulted.

Reading Ogg is `craft_media::ogg` ([craft-libs](https://github.com/storytold/craft-libs)). This
crate stays in FilmCraft until craft-media has a writer; then export switches to it and this crate
is removed.

## Specifications

| Document | Used for |
|---|---|
| RFC 3533 (2003) | pages, lacing, packet continuation, CRC-32 |
| RFC 7845 (2016) | Ogg Opus: `OpusTags`, granule positions, end trimming (§4) |
| Vorbis I specification (2020-07-04), §5 and Appendix A | Vorbis comment header, Ogg mapping |

## Use

```rust
/// An Ogg Opus file from an `OpusHead`, 20 ms packets, the pre-skip and the playable length.
fn ogg_opus(opus_head: &[u8], packets: &[Vec<u8>], pre_skip: i64, samples: i64) -> Vec<u8> {
    let mut w = filmcraft_oggmux::PageWriter::new(0x4643_4F50);
    let mut file = w.header(opus_head);                               // first page: beginning of stream
    file.extend(w.header(&filmcraft_oggmux::opus_tags("FilmCraft", &[("TITLE", "Mix")])));
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

`src/write_tests.rs` writes files with `PageWriter` and reads them back with `craft_media::ogg` (a
dev-dependency): every packet, the headers on their own pages, flags, sequence numbers and CRCs, a
packet spanning three pages, page targets and the Opus end trimming.
