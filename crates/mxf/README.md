# filmcraft-mxf

A clean-room MXF demuxer and writer. Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
`wasm32-unknown-unknown`. No GPL/LGPL code (FFmpeg, libMXF, bmx, MXFLib) was consulted; FFmpeg is
used only as an external fixture generator and test oracle.

## Specifications

Implemented from the SMPTE documents (editions used):

| Document | Edition | Used for |
|---|---|---|
| SMPTE ST 336 | 2017 | KLV coding: 16-byte UL keys, BER lengths |
| SMPTE ST 377-1 | 2011 (+ Amd 1:2012) | partitions, primer pack, header metadata sets and local tags, index table segments, random index pack, run-in |
| SMPTE ST 378 | 2004 | OP1a |
| SMPTE ST 390 | 2011 | OP-Atom |
| SMPTE ST 379-1 / 379-2 | 2009 / 2010 | generic container: content packages, element keys, frame / clip wrapping |
| SMPTE ST 381-1 | 2005 | MPEG video mapping (MPEG-2 identified, MPEG video descriptor; decoded by `filmcraft-mpeg2v` in `filmcraft-codecs`) |
| SMPTE ST 381-3 | 2013 | AVC byte-stream mapping and AVC sub-descriptor |
| SMPTE ST 382 | 2007 | AES3 and Broadcast Wave audio mapping (wave / AES3 descriptors) |
| SMPTE ST 331 | 2011 | element data of 8-channel AES3 sound (D-10) |
| SMPTE ST 386 | 2004 | D-10 (IMX) mapping: container label, sound element type |
| SMPTE ST 2019-4 | 2009 | VC-3 (DNxHD / DNxHR) mapping |
| SMPTE RDD 44 | 2017 | Apple ProRes mapping |
| Adobe CinemaDNG Image Data Format Specification | 1.0.0.0 (September 2009), §7 | CinemaDNG in MXF: frame-wrapped DNG picture elements (element type 42h), the essence container labels for CFA / LinearRaw content, RGBA descriptor; the labels are the specification's **provisional** values from Adobe's experimental range (byte 9 = 0Fh), not SMPTE-registered |
| SMPTE RP 224 / RP 210 | registers | labels: data definitions, picture coding, essence containers, operational patterns; property labels of the primer pack (writer) |
| SMPTE ST 330 | 2011 | basic UMIDs of the written packages |

## Reading a file

```rust
let file = std::fs::read("clip.mxf")?;
let mxf = filmcraft_mxf::open(&file)?;
println!("{} {:?}", mxf.operational_pattern.name(), mxf.timecode.map(|t| t.format()));
let v = mxf.track_of_kind(filmcraft_mxf::TrackKind::Picture).unwrap();
let t = &mxf.tracks[v];
let key = t.sync_before(t.sample_at(42).unwrap());    // stored index of the random-access picture
let bytes = mxf.read_sample(&file, v, key)?;           // one edit unit's essence
let a = mxf.track_of_kind(filmcraft_mxf::TrackKind::Sound).unwrap();
let pcm = mxf.read_pcm(&file, a, 48_000, 1920)?;       // planar f32, sample-exact
```

- **Open.** The header partition pack is found in the run-in (≤ 64 KiB). Every KLV packet is then
  walked (keys and lengths only; essence values are skipped, never read): partition packs, primer
  packs, header metadata local sets, index table segments, fill, and generic-container elements
  (system items are ignored). A lost KLV sync resynchronises on the next partition pack; a
  truncated file keeps everything before the cut (a truncated frame-wrapped picture is dropped,
  clip-wrapped essence keeps its whole edit units).
- **Metadata.** The header metadata of the most complete partition is used (closed complete >
  open complete > closed > open; later partitions win ties, so the footer's final durations are
  preferred). Dynamic local tags resolve through the primer pack (e.g. `SubDescriptors`).
- **Tracks.** Material package picture/sound tracks → source clip → file package track (track
  number, edit rate, origin) → descriptor (a multiple descriptor's sub-descriptor by
  `LinkedTrackID`). Without a usable material package the file packages' tracks are used directly.
  Elements are matched by track number, or — when the number is 0 or does not match (some OP-Atom
  writers) — the only unclaimed element stream of the right kind.
- **Pictures.** One `Sample` per edit unit in stored order: frame wrapping (one element each) or
  clip wrapping (one element split by the index: constant `EditUnitByteCount` or VBR entries).
  Random access from index flag bit 7 (intra-only codings: every picture); without an index the
  essence is scanned (AVC IDR, MPEG-2 sequence header). Presentation order from the index temporal
  offsets (display position *n* is stored at *n* + `TemporalOffset[n]`). When the index marks B
  pictures but has no temporal offsets (FFmpeg-written AVC), `needs_reorder` is set: the caller
  orders by the bitstream (`filmcraft-codecs` uses the AVC picture order counts).
- **Codecs.** From the picture essence coding label, then the essence container label, then the
  essence bytes: AVC / AVC-Intra, VC-3, ProRes (profile from the label), MPEG-2 (decoded in `filmcraft-codecs`),
  MPEG-4 visual, JPEG 2000, DV, uncompressed, CinemaDNG (the CinemaDNG container labels, the
  registered MXF-GC TIFF/EP container and TIFF/EP picture coding labels, a 42h picture element
  type, or a TIFF header at the start of the essence; developed by `filmcraft-dng` in
  `filmcraft-codecs`).
- **Sound.** PCM chunks (frame- or clip-wrapped) with sample counts; `read_pcm` decodes
  little-endian 8/16/24/32-bit PCM (wave / AES3 descriptors) and ST 331 AES3 elements (D-10).
- **Timecode.** The material package timecode component (start frame count, rounded base,
  drop frame), falling back to the file package's; `Timecode::format` gives `HH:MM:SS:FF` /
  `HH:MM:SS;FF`.

Not supported: OP1b/OP2x+ edit lists beyond the first source clip, external essence (OP-Atom
material packages whose other tracks live in other files are opened track by track), partial or
encrypted essence (ST 429-6), and descriptive metadata (parsed sets are ignored).

## Writing a file

```rust
use filmcraft_mxf::*;
let ids = PackageIds::from_seed("unique seed (path + time)", "Clip name"); // material + file UMIDs
let mut cfg = WriterConfig::new(Pattern::Op1a, Rational::new(25, 1), ids);
cfg.picture = Some(PictureDesc::new(PictureCoding::Vc3 { cid: 1272 }, 1920, 1080));
cfg.sound = vec![SoundDesc { sample_rate: 48_000, channels: 2, bits: 24 }];
cfg.timecode = Some(StartTimecode { frames: 90_000, rate: Rational::new(25, 1), drop_frame: false });
let mut w = MxfWriter::new(std::io::BufWriter::new(std::fs::File::create("out.mxf")?), cfg)?;
w.push_picture(&frame_bytes, FrameInfo::intra())?;   // stored order; AVC: key / P / B + display position
w.push_sound(0, &interleaved_le_pcm)?;              // any chunking: content packages are cut per edit unit
w.finish()?;
```

- **Streaming.** Any `Write + Seek` sink. `new` writes the header partition (open, incomplete,
  durations unknown) and a body partition; essence follows as it is pushed; `finish` writes the
  footer partition (closed, complete header metadata with the final durations and the index table
  segments), the random index pack, then rewrites the header partition pack and header metadata in
  place (the metadata's size does not depend on the durations; a reserved fill item absorbs any
  difference) and the body partition pack's footer offset. Only the index entries (11 bytes per
  edit unit, plus 4 per sound slice) are kept in memory.
- **OP1a** (ST 378): one content package per edit unit, frame wrapping (ST 379-1): the picture
  element then one element per sound track. Sound is buffered and cut into edit units exactly
  (`floor((n+1)·rate·den/num) − floor(n·rate·den/num)` samples: 1601/1602 at 29.97). Pictures
  without their sound yet wait; `finish` pads the last units' sound with silence. A multiple
  descriptor with one sub-descriptor per track (linked track ids), the "multiple wrappings"
  container label.
- **OP-Atom** (ST 390, Avid style): exactly one essence track per file, clip-wrapped (one element
  with a 9-byte BER length patched by `finish`). Several atom files of one clip can share the
  material package UMID with distinct track ids (`WriterConfig::first_track_id`). Sound files may
  use the sample rate as the edit rate (a constant-bytes index) or the frame rate (VBR entries).
- **Index tables** (ST 377-1 §11): VBR entries per edit unit — stream offset (from the first
  essence byte; clip-wrapped: from the element value), key-frame offset, flags (`0x80` random
  access, `0xC0` AVC IDR with its parameter sets, `0x22` P, `0x33` B) and temporal offsets from the
  display positions (`TO[d] = stored(d) − d`); a delta entry and a slice per sound element. Split
  into segments of at most ~60 KB (the entry array is a 2-byte-length local set item).
- **Metadata.** Preface, identification (company / product / version / modification date),
  content storage, essence container data (BodySID 1, IndexSID 2), material and file packages
  (`PackageIds`: UMIDs and names) with a timecode track (start, rounded base, drop frame) and one
  timeline track per essence (source clip → file package track; track numbers = element keys),
  CDCI picture descriptors (size, aspect ratio, depth, subsampling, black / white / range, BT.709 /
  BT.2020 PQ / HLG colour labels, coding label) and wave audio descriptors.
- **Mappings.** VC-3 (ST 2019-4: coding label `…71 cc` from the compression id, container `02 11`,
  element types 0x0C / 0x0D), ProRes (RDD 44: coding label profile byte, container `02 1C`),
  AVC byte stream (ST 381-3: container `02 10 60`, coding label from `profile_idc`; Annex B access
  units with in-band SPS / PPS — the AVC sub-descriptor is not written), Broadcast Wave PCM
  (ST 382: container `02 06 01/02`, element types 0x01 / 0x02), CinemaDNG (frame-wrapped DNG
  datasets, element type 0x42, the provisional container label `0F 0A 0D 0B 02 7F 01/02 00`, an
  RGBA descriptor with pixel layout and reference levels of the decoded 16-bit RGB picture and no
  coding label; OP-Atom is refused because it is clip-wrapped here and DNG datasets cannot be).
- `write_opatom_pcm(&OpAtomPcm, &[i32])` builds a complete in-memory OP-Atom PCM file (AAF media
  consolidation); `encode_pcm` interleaves planar f32 as little-endian PCM.

## Tests

- `src/tests.rs`: a small OP1a writer builds synthetic files (temporal offsets, key flags, PCM,
  timecode); open, presentation order, sample bytes, PCM values; truncation at every 37 bytes and
  600 random mutations never panic and only list fully-present samples.
- `crates/codecs/tests/it/mxf_oracle.rs` (FFmpeg-written fixtures, FFmpeg as the decode oracle):
  H.264 long-GOP with B pictures (bit-exact every frame + 25 random seeks), H.264 29.97 DF
  timecode, DNxHR LB (±2), ProRes 422 (±1), MPEG-2 long GOP, XDCAM HD422 and D-10 (±4),
  OP-Atom VC-3 (clip-wrapped) and PCM, D-10 AES3 audio; PCM sample-exact in every file;
  truncated and corrupted files. `cargo xtask fixtures codecs` pre-generates them.
- `crates/codecs/tests/cinemadng_mxf.rs`: OP1a CinemaDNG files from our writer wrapping DNG frames
  from the `filmcraft-dng` test writer (CFA lossless JPEG and LinearRaw): every frame developed to
  its known level in any order, frame rate, timecode, sample-exact sound; a non-DNG picture is
  an error; truncation and mutation never panic. No tool we can run reads CinemaDNG MXF, so no
  real camera file has been tested.
- `src/write/tests.rs`: writer output read back by `open`: OP1a with two sound tracks pushed in
  uneven chunks (durations, codec label, aspect, PCM exact, index slices, closed partitions),
  29.97 sound per edit unit (1601/1602), a long-GOP AVC index (presentation order, key and B
  flags), OP-Atom picture (clip wrapping) and PCM files at both edit rates, multi-segment index
  of 12 000 units, UMIDs; truncation and random mutation of written files never panic.
- `crates/export/src/mxf_tests.rs` (ffprobe / ffmpeg as external oracles): OP1a exports with
  DNxHR, ProRes and H.264 — ffprobe reports the OP1a label, codec, 24 frames, frame rate, duration
  and `01:00:00:00` timecode; ffmpeg decodes the MXF picture exactly as it decodes the same
  essence exported as MOV, and its PCM equals our demuxer's sample for sample; OP-Atom exports —
  picture file and one mono file per channel, each channel equal to the WAV export's.
