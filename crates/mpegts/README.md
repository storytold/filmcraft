# filmcraft-mpegts

Clean-room MPEG-2 Systems demuxer: transport streams and program streams. Layer L0: depends only
on `thiserror`. Builds for `wasm32-unknown-unknown`; no `unsafe`. No GPL/LGPL code (FFmpeg,
libavformat) was consulted; FFmpeg is used only as an external fixture generator and test
oracle.

## Specifications

| Document | Edition | Used for |
|---|---|---|
| ITU-T H.222.0 \| ISO/IEC 13818-1 | 2000 (second edition) | TS packets, adaptation fields, PCR, PSI (PAT / PMT, CRC-32 of Annex A), PES headers, PTS/DTS, program streams (pack, system header, program stream map), stream types |
| ISO/IEC 11172-1 | 1993 | MPEG-1 system streams (pack header, PES header stuffing / STD buffer / timestamps) |
| Blu-ray / AVCHD BDAV MPEG-2 TS layout (192-byte source packets with a 4-byte TP_extra_header; registration `HDMV`; stream types 0x80-0x92) | public descriptions | `.m2ts` / `.mts` |
| DVD-Video private stream 1 sub-stream layout (AC-3 / DTS 0x80-0x8F with a 3-byte header, LPCM 0xA0-0xAF with a 6-byte header, sub-pictures 0x20-0x3F) | public descriptions | `.vob` |
| ETSI EN 300 468 (descriptors 0x56, 0x59, 0x6A, 0x7A, 0x7B), ATSC A/52 Annex A (AC-3 in TS) | — | identifying private PES streams |
| ISO/IEC 11172-3, 13818-3 (frame headers), ISO/IEC 13818-7 (ADTS), 14496-3 (LOAS), ATSC A/52 (syncinfo, E-AC-3 frmsiz) | — | audio frame lengths for splitting |

## What it does

```rust
let file = filmcraft_mpegts::open(&bytes)?;           // any ByteSource (file, memory)
for (i, s) in file.streams.iter().enumerate() {
    println!("{:?} {} {} units", s.id, s.codec.name(), s.units.len());
}
let v = file.find(Kind::Video, |_| true).unwrap();
let au: Vec<u8> = file.read_unit(&bytes, v, 0)?;      // the first access unit
```

- **Transport streams**: 188-byte packets, 192-byte BDAV packets (sync byte after a 4-byte
  arrival timestamp) and 204-byte packets; the packet size is sniffed from five consecutive sync
  bytes. Lost sync resynchronises on the next aligned run. PSI sections are reassembled across
  packets and checked with the MPEG CRC-32; the first program of the PAT is demuxed. Continuity
  counter errors, CRC errors and resyncs are reported as warnings.
- **Program streams**: MPEG-2 and MPEG-1 packs, system headers, the program stream map (stream
  types for elementary stream ids), padding, private stream 2 (DVD navigation) skipped, private
  stream 1 split by sub-stream id. Garbage resynchronises on the next pack or PES start code.
- **Streams**: MPEG-1/2 video, H.264, HEVC, MPEG audio, AAC (ADTS, LATM/LOAS), AC-3, E-AC-3,
  DTS, TrueHD, Blu-ray LPCM, DVD LPCM (format from the sub-stream header), PGS / DVB / teletext
  subtitles; ISO 639 language from the PMT.
- **Access units**, found while the file is scanned (one pass, streaming; only a few bytes of
  each stream are buffered):
  - MPEG-1/2 video: a coded frame — a frame picture, or two field pictures (paired by
    picture_structure) — with the sequence / GOP headers before it; picture type, temporal
    reference, field structure, top_field_first, repeat_first_field, progressive_frame, closed
    GOP / broken link. A PTS belongs to the unit whose picture start code is in that PES.
  - H.264 / HEVC: PES packets with a timestamp (a PES without one continues the previous unit);
    random access from IDR / IRAP NAL types; disposable when every slice is a non-reference.
  - Audio: one frame per unit from the frame headers (sync re-hunted after garbage; a header
    found while hunting is confirmed by the next one); a PTS belongs to the first frame starting
    in the PES.
  - LPCM and everything else: one PES packet per unit.
- **Timestamps**: 33-bit PTS/DTS unwrapped across the wrap (and back, for B pictures shown
  before a wrap that came earlier); PCR / SCR range.
- **Reading**: a unit records only the position of its first packet and its offset in that
  packet's payload; `read_unit` re-reads from there (the index costs a few dozen bytes per
  unit, not per packet).

## Tests

- `src/tests.rs`: a small TS muxer (PAT, PMT with CRC, PES split into 188- or 192-byte packets
  with adaptation-field stuffing) and PS writer (packs ≤ 2000 bytes, so frames span PES packets;
  AC-3 in private stream 1): every access unit reads back byte-identical with its PTS, key and
  disposable flags, field pairs as one unit, audio frames with the first-in-PES timestamp;
  33-bit unwrap; truncation every 37 bytes and 400 random mutations never panic.
- `src/es.rs`, `src/pes.rs`: frame sizes (MPEG audio layers I-III, MPEG-2 LSF, AC-3 at all
  rates), MPEG-2 and MPEG-1 PES headers.
- `crates/codecs/tests/it/mpeg_oracle.rs`: FFmpeg-written TS / M2TS / VOB / MPEG-1 system files —
  frame counts and every frame's PTS equal ffprobe's, decoded media against FFmpeg (see
  `filmcraft-codecs`).

## Gaps

- One program per TS (the first in the PAT); no program switching, no section-based
  metadata (SDT / EIT), no scrambled streams.
- H.264 / HEVC access units follow PES packets: streams that put several pictures in one PES
  packet (rare in practice; not seen in broadcast, AVCHD or FFmpeg output) would be indexed as
  one unit.
- DVD navigation (IFO / NAV packets), multiple VOB concatenation and angle interleaving are not
  handled; each file opens on its own.
