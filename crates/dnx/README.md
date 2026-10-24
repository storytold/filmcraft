# filmcraft-dnx

Clean-room Avid DNxHD / DNxHR (VC-3) decoder and encoder in pure Rust, written from the public
**SMPTE ST 2019-1:2016** *VC-3 Picture Compression and Data Stream Format* and its
**Amendment 1:2023** (both free from `pub.smpte.org`). Layer L0: depends only on
`filmcraft-bitstream`, `thiserror` and (optional, feature `threads`, default on) `rayon`. Builds
for `wasm32-unknown-unknown`; no `unsafe`.

The constant tables (Annex D quantisation weights, Annex E VLC tables) were extracted from the
standard's text by [`tools/extract_tables.py`](tools/extract_tables.py) into
`src/spec_tables.rs`; the script checks every table's size and that every VLC table is a
complete prefix code. No code was taken from FFmpeg or any other implementation; ffmpeg is used
only as an external fixture generator and test oracle.

## Status

| Area | Status |
|---|---|
| Header (§7.2): all fields, HVN 1–4, RI header growth above 1088 lines, time code | done |
| DNxHD (HD profile) CIDs 1235, 1237, 1238, 1241–1244, 1250–1253, 1258, 1259 | done, oracle-tested |
| CID 1256 (HD RGB 4:4:4) and 1260 (macroblock-adaptive field/frame, 1440×1080i) | done, not oracle-tested (ffmpeg can't encode them) |
| DNxHR (RI profile) 1270–1274: LB, SQ, HQ, HQX, 444; any raster 1…16384 | done, oracle-tested |
| 8-, 10-, 12-bit; 4:2:2, 4:4:4; Y′CbCr and RGB (per-macroblock ACF) | done (12-bit: our encoder's streams, compared with ffmpeg's decode) |
| 4:2:0 (RI only) | done, not oracle-tested (ffmpeg can't encode it) |
| Interlaced: field-coded (two coding units) and frame-coded MACF | done |
| Alpha: DCT-coded and lossless differential RLE (Amendment 1 for HD CIDs) | done, not oracle-tested (no encoder produces it) |
| CBR padding, VBR, scan-line indices, EOF signature / CRC flag | done |
| Encoder: DNxHR LB, SQ, HQ (8-bit), HQX (10/12-bit), 444 (10/12-bit); CBR | done |
| Encoder: DNxHD (HD CIDs), interlaced, alpha, VBR | not yet |

## API

```rust
use filmcraft_dnx::*;

let frame: Frame = decode_frame(&sample)?;               // one or two coding units
let f = decode_frame_with(&sample, &DecodeOptions { threads: false, ..Default::default() })?;
let hdr: FrameHeader = probe(&sample)?;                  // cid, raster, depth, chroma…
let name = cid_name(hdr.cid);                            // "DNxHR HQX", "DNxHD 1080i 8-bit (CID 1242)"

let mut enc = Encoder::new(Profile::Hq, 1920, 1080)?;    // or Encoder::with_config(EncoderConfig { .. })
let sample: Vec<u8> = enc.encode(&frame)?;               // exactly enc.frame_size() bytes (CBR)
```

`Frame { width, height, chroma, rgb, bit_depth, y, cb, cr: Vec<u16>, alpha, interlace,
color_volume, cid, par }` holds planar samples without row padding. Interlaced frames are woven
(field 1 = top field on even lines). RGB frames hold G, B, R in `y`, `cb`, `cr`. In MOV the
sample entry is `AVdn` (DNxHD) or `AVdh` (DNxHR); `filmcraft-codecs` reads the CID from the
first frame header to label the stream.

## Decoder

- Every coding unit's scan-line indices give each macroblock row's byte offset, so rows decode
  independently and in parallel, writing straight into their 16-line bands.
- VLCs are decoded with two-level lookup tables (11-bit primary) built from the Annex E lists.
- Inverse quantisation with the CID's Annex D weights; IDCT: the exact orthonormal 8×8 inverse DCT
  of equation 8.3, evaluated separably in `f32` (shared design with `filmcraft-prores`).
  Output: `floor(x·s + 2^(b−1) + ½)` clipped to `[0, 2^b − 1]`, where `s = 4` at 12 bits (the
  12-bit coefficient domain is ¼ of the orthonormal DCT, as Table 14's 13-bit IDCT input range
  for 12-bit samples implies) and 1 otherwise.
- Robustness: every offset and size is validated, run lengths past coefficient 63 are rejected,
  and corrupted or truncated streams return `Err` without panicking (`tests/it/roundtrip.rs`).

### Where we follow ffmpeg rather than the letter of the standard

ffmpeg's decoder is the reference everyone interoperates with (and the oracle of our tests). In
three places it differs from the text of ST 2019-1:2016; black-box probes (`src/ffmpeg_probe.rs`:
streams with a single coefficient, decoded by ffmpeg, measured with a forward DCT) pinned down
its behaviour, and we match it by default:

1. **Reconstruction offset.** Equation 8.1 reconstructs a quantised amplitude `A` at
   ≈ `(A + ½)·W·qsf/p`. ffmpeg reconstructs at `(A + ¾)·W·qsf/p` (fitted intercept 0.750 ± 0.01
   steps for every CID family and step size). The decoder does the same;
   `DecodeOptions::spec_dequant` selects equation 8.1 instead. The encoder picks the amplitude
   nearest to the `(A + ¾)` reconstruction.
2. **CID 1271 (DNxHR HQX) weights.** Table C.2 assigns Table D.1 to CID 1271. ffmpeg's decoder and
   encoder use Table D.4 (the CID 1241 weights) — fitted RMS error 0.65 against D.4, > 2.6
   against every other table. We use D.4.
3. **4:4:4 signalling.** ffmpeg writes RGB DNxHR 444 with `CLF = 0`, and Y′CbCr 444 with `CLF = 1`
   plus `ACF = 1` on every macroblock; its channel order is G, B, R (Ch1 = G). The decoder treats
   every 4:4:4 stream as RGB except macroblocks flagged `ACF = 1` (a stream that is entirely
   ACF = 1 is returned as Y′CbCr; mixed streams are converted to RGB macroblock by macroblock as
   §8.1.1 describes), and the encoder writes the same signalling.

Also noted: the RLE alpha pseudo code (Figure 50) tests the header bit the opposite way from
the prose of §7.3.1.2; we follow the prose (normative text takes precedence).

## Encoder

- DNxHR LB/SQ/HQ (8-bit 4:2:2), HQX (10- or 12-bit 4:2:2) and 444 (10- or 12-bit 4:4:4,
  Y′CbCr or RGB), progressive, CBR at exactly the equation 7.1 frame size (e.g. 917 504 bytes
  for 1080p HQ, 606 208 for SQ). Input at any depth 8–16 is rescaled to the coded depth.
- Every macroblock is transformed once (orthonormal FDCT) and stored pre-divided by its weights.
  Rate control binary-searches the smallest uniform `qsf` whose frame fits the payload
  (including the 4-byte scan-line alignment), starting from the previous frame's value, then
  gives macroblocks one finer step, spread evenly over the frame, while the budget allows.
- `export::Format::DnxHr` (`file.exportMedia {"format":"dnxhr","dnxProfile":"lb|sq|hq|hqx"}`)
  writes QuickTime `AVdh` + PCM. The Avid-specific `ACLR`/`ADHR` sample-entry atoms are not
  written (ffmpeg and our reader don't need them).

## Accuracy (vs ffmpeg as an external oracle)

`tests/it/oracle_decode.rs` decodes 22 fixtures made by ffmpeg's VC-3 encoder: CIDs 1235, 1237,
1238, 1241, 1242, 1243, 1244, 1250, 1251, 1252, 1253, 1258, 1259 and DNxHR LB/SQ/HQ/HQX/444
(Y′CbCr and RGB), with testsrc2, mandelbrot and heavy noise, at 1920×1080, 1280×720, 960×720,
1440×1080 (thin raster, interlaced), 1000×562, 640×360, 256×120 and 3840×2160. Compared with
ffmpeg's decode at the native depth, **every fixture is within ±2 LSB, with at most ~50 samples
per million at ±2** (typically 1–30 per fixture); 87–99% of samples are bit-identical and the
mean absolute error is 0.01–0.13 LSB. The remaining differences are ffmpeg's integer IDCT; this
decoder evaluates the exact IDCT of equation 8.3, so decode is **not bit-exact, but within IDCT
precision** of ffmpeg. (ffmpeg wraps rather than clips 10-bit reconstructions that overflow
1023, so the noise fixtures keep their samples inside 24…231 before encoding.)

`tests/it/oracle_encode.rs` encodes 1920×1080 frames with every profile and writes them into a
minimal test-only MOV. ffmpeg decodes them with `-xerror`, without errors, and agrees with our
decoder within ±2 (90–99% identical). Luma PSNR against the source: HQ 58.8 dB (mandelbrot) and
66.3 dB (testsrc2), SQ 57.5 dB, LB 56.1 dB, HQX 10-bit 67.5 dB, HQX 12-bit 80.2 dB, 444 10-bit
79.7 dB — all above the 45 dB HQ target.

## Performance

`cargo test --release -p filmcraft-dnx --test it perf:: -- --ignored --nocapture` on an M4 Pro
(14 cores) while the machine was **heavily overloaded** by other jobs (load average ≈ 190).
Under the same load `filmcraft-prores`' perf test measured 30 fps threaded / 11.8 fps single
for a 1080p HQ frame, against 318 / 53 fps in its README on a less loaded machine, so these are
lower bounds by a factor of roughly 4–10:

| Frame (busy synthetic content) | Decode, threaded | Decode, 1 thread | Encode |
|---|---|---|---|
| DNxHR HQ 1080p (896 KB) | 41 fps | 13.2 fps | 4.1 fps |
| DNxHR HQX 10-bit 1080p (896 KB) | 39 fps | 8.9 fps | 3.6 fps |
| DNxHR SQ 2160p (2352 KB) | 21 fps | 15.6 fps | 1.1 fps |
| DNxHR HQX 10-bit 2160p (3556 KB) | 16 fps | 12.2 fps | 1.1 fps |

## Gaps

- Decode is within IDCT precision of ffmpeg, not bit-exact (see above).
- Encoder: no DNxHD (HD CIDs), interlaced, alpha or VBR output; no RDO/trellis.
- CIDs 1256/1260 and alpha decoding are implemented from the standard but have no oracle
  fixtures, because no available encoder produces them.
- No MXF; DNx in MOV only (via `filmcraft-isobmff` / `filmcraft-codecs`).
