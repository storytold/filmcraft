# filmcraft-dng

Clean-room DNG and CinemaDNG camera RAW decoder in pure Rust. Layer L0: depends on `thiserror`,
`image` (its baseline JPEG decoder only, for lossy JPEG DNG; the workspace already uses it in
`codecs` and `media`) and (optional, feature `threads`, default on) `rayon`. Builds for
`wasm32-unknown-unknown`; no `unsafe`. Implemented from the public specifications below only; the
source of no other raw decoder (dcraw, LibRaw, rawspeed, darktable, RawTherapee, FFmpeg, the Adobe
DNG SDK…) was read. LibRaw's `unprocessed_raw` sample program is run as an external test oracle
when it is installed, never linked.

## Specifications

| Document | Edition | Used for |
|---|---|---|
| Adobe Digital Negative (DNG) Specification | 1.4.0.0 (2012) – 1.7.1.0 (2023) | Compression 34892 (lossy JPEG: 8-bit LinearRaw only), raw IFD selection (NewSubFileType 0), CFA / LinearRaw data, BitsPerSample packing, LinearizationTable, BlackLevel (+ RepeatDim, DeltaH / DeltaV), WhiteLevel, ActiveArea, DefaultCrop, the colour model of chapter 6 (ColorMatrix, CameraCalibration, AnalogBalance, ForwardMatrix, AsShotNeutral / AsShotWhiteXY, CalibrationIlluminant interpolation, BaselineExposure), opcode lists (chapter 7: OpcodeList1–3 and opcodes 1–14) |
| TIFF Revision 6.0 | June 1992 | header, IFDs, field types, strips and tiles, PlanarConfiguration, row padding |
| TIFF/EP, ISO 12234-2 | 2001 | CFARepeatPatternDim, CFAPattern |
| TIFF Technical Note 1 (SubIFDs) | 1993 | the SubIFD tree |
| ISO/IEC 18181-1:2022 (JPEG XL) | 2022 | decoded only through `jxl-oxide`'s public API (feature `jxl`) |
| ITU-T T.81 \| ISO/IEC 10918-1 | 09/1992 | lossless JPEG (process 14, SOF3): Annex B syntax, Annex C Huffman tables, F.2.2.3 decoding, Annex H prediction and point transform |
| CinemaDNG Image Data Format Specification | 1.0.0.0 (September 2009) | frames as numbered DNG files, FrameRate, TimeCodes, ReelName tags |
| SMPTE ST 12-1 | 2014 | time code BCD packing (via the CinemaDNG TimeCodes tag) |
| CIPA DC-008 (Exif) | 2.3 | LightSource codes → illuminant temperatures |
| Malvar, He, Cutler, "High-quality linear interpolation for demosaicing of Bayer-patterned color images", IEEE ICASSP | 2004 | the Bayer demosaic filters |
| McCamy, "Correlated color temperature as an explicit function of chromaticity coordinates", Color Res. Appl. 17(2) | 1992 | white xy → temperature for the calibration interpolation |
| IEC 61966-2-1 (sRGB) | 1999 | Rec. 709 / sRGB primaries and D65 white |
| Krystek, "An algorithm to calculate correlated colour temperature", Color Res. Appl. 10(1) | 1985 | the Planckian locus in CIE 1960 uv for temperature / tint white balance |

## API

```rust
if filmcraft_dng::sniff(&bytes) {
    let dng = filmcraft_dng::Dng::parse(&bytes)?;
    let info = dng.info()?;     // size after crop, bits, CFA or LinearRaw, frame rate, time code, camera, reel
    let raw = dng.raw()?;       // the stored samples (u16), bit-exact
    let img = dng.develop()?;   // linear Rec. 709 RGBA f32 (alpha 1), info.width × info.height
}
let img = filmcraft_dng::decode(&bytes)?; // parse + develop
// raw settings: white balance (as shot or temperature / tint), exposure, highlights, demosaic
// quality, output space, and a target size for a reduced-resolution fast path
let o = filmcraft_dng::ProcessOptions { exposure: 0.5, ..Default::default() };
let img = filmcraft_dng::decode_with(&bytes, &o)?;
```

`ProcessOptions::default()` develops exactly like `decode`. `Highlights::Recover` is accepted but
not implemented yet (it develops like `Clip`). `Dng::as_shot_temperature_tint` gives the as-shot
white as kelvin and tint, the starting point of a white-balance control; temperature and tint
are a point on the Planckian locus (Krystek 1985) and a CIE 1960 Δuv (tint 150 = Δuv +0.05, a
greener white, so a more magenta picture; D65 is about tint +10).

`writer` (feature `writer`, tests only) writes synthetic DNGs: uncompressed at any depth, lossy JPEG (through `image`'s encoder) or
lossless JPEG with any predictor, strips or tiles, either byte order, an optional preview IFD with
the raw image in a SubIFD, and arbitrary extra tags.

## What it decodes

- **Container:** little and big-endian TIFF, IFD chains and SubIFDs (at most 64 IFDs, 4 levels,
  cycles end the walk). The main image is the largest NewSubFileType 0 IFD with a CFA or LinearRaw
  photometric interpretation.
- **Raw data:** 1–16-bit unsigned samples, uncompressed (8 and 16-bit in file byte order, other
  depths packed most significant bit first), lossy JPEG (Compression 34892: baseline DCT tiles or
  strips of one or three components, 8-bit LinearRaw only, as DNG 1.4 allows; the JPEG's own
  colour transform applies), JPEG XL (Compression 52546, DNG 1.7, **feature `jxl`, off by
  default**: decoded by `jxl-oxide`, MIT OR Apache-2.0, pure Rust; 8–16-bit integer LinearRaw of
  one or three channels, bare codestreams or containers; integer samples come back at the
  codestream's depth, exact for lossless JPEG XL) or lossless JPEG (all predictors 1–7, 1–4 components,
  point transform, restart intervals of whole lines), in strips or tiles; tiles decode in
  parallel.
- **Processing:** ActiveArea, LinearizationTable, per-site black levels with row / column deltas,
  per-sample white levels, as-shot white balance (clipped highlights stay neutral), demosaic
  (Malvar–He–Cutler for 2 × 2 Bayer patterns of any phase, bilinear for other patterns up to
  8 × 8), DefaultCrop, the DNG colour model to XYZ D50 and Bradford to linear Rec. 709, and
  BaselineExposure. LinearRaw (3 samples) skips the demosaic; monochrome LinearRaw is grey.
- **Opcode lists (DNG chapter 7), opt-in (`ProcessOptions::apply_opcodes`, off by default
  until the interpretations below are compared with real camera files):** OpcodeList1 on the stored samples (0–65535), OpcodeList2 on
  the linear reference values before white balance, OpcodeList3 on the demosaiced camera values
  (white balance taken out, 0–1), values clipped after every opcode. WarpRectilinear,
  WarpFisheye, WarpRectilinear2 (with the skip rule for a following warp), FixVignetteRadial,
  FixBadPixelsConstant / List, TrimBounds, MapTable, MapPolynomial, GainMap, DeltaPerRow /
  Column, ScalePerRow / Column. Unknown opcodes, and opcodes from a DNG version above 1.7.1.0,
  are skipped when optional and an error naming them when not; preview-only opcodes are skipped
  on the reduced-resolution path. Where the specification leaves room: OpcodeList1 uses stored
  image coordinates, OpcodeList2 and 3 ActiveArea coordinates; GainMap positions are row / height
  and column / width of the whole picture; Delta and MapPolynomial values are in the 0–1 range at
  every stage (×65535 on OpcodeList1), MapTable indexes 16-bit values; warps resample with a
  Catmull–Rom cubic; warps on a CFA mosaic and TrimBounds before OpcodeList3 are not applied
  (an error unless optional). A TrimBounds makes the developed picture smaller than `Info`'s
  size. At most 256 opcodes per list; counts are checked against the parameter bytes before
  anything is allocated.
- **Output:** scene-linear Rec. 709 floats: a white the sensor clipped is 1.0 × 2^BaselineExposure;
  brighter values are kept (FilmCraft's compositor works in linear float). Negative results of the
  colour matrix are clipped to 0. Black levels scale per pixel by (white − black).
- **White balance without a ForwardMatrix** follows the specification: the camera matrix takes the
  values to XYZ and a Bradford adaptation moves the as-shot white to D50. Decoders in the dcraw
  tradition (LibRaw included) scale the camera channels instead, so under a white far from D65
  their saturated colours differ from ours (greys agree).
- **Speed:** a 4096 × 2160 12-bit lossless-JPEG frame decodes and develops about 2–3 times
  faster than in the first version (best of 9 on a shared 16-thread machine: tiles 256 × 256
  about 82 ms on 1 thread and 21 ms on 16, one strip 30 ms of serial entropy decoding plus the
  processing). Lossless JPEG decodes a code and its difference bits with one 13-bit lookup and
  refills its bit buffer eight bytes at a time; tiles decode in parallel straight into place (a
  single strip is used without a copy); without black-level deltas a lookup table per CFA /
  black-level site replaces the normalisation arithmetic; demosaic, crop and colour run row by
  row without intermediate pictures. Output is bit-identical to the first version
  (`tests/it/equivalence.rs` compares f32 bit patterns with the kept `reference` module).
- **Reduced resolution:** with `ProcessOptions::target_size`, a Bayer, LinearRaw or monochrome
  picture develops at 1/2, 1/4 or 1/8 size (the smallest that still covers the target) by
  averaging each colour over 2 × 2 (4 × 4, 8 × 8) blocks of the crop, without a demosaic: about
  three times less processing at half size.
- `cargo run --release -p filmcraft-dng --example bench` times the current pipeline against the
  reference at 1, 4 and 16 threads, interleaved in one run.

## Not supported (errors name them)

JPEG XL DNG without the `jxl` feature, deflate / floating-point DNG (HDR merges), planar raw data, colour
filters other than red / green / blue (CMY, RGBE), non-rectangular CFA layouts, DefaultScale and
BestQualityScale (non-square pixels are shown square), DNG 1.6 IlluminantData for illuminant 255
(calibration 1 is used) and lossless JPEG restart intervals that split a line. This crate reads
DNG frames only; MXF-wrapped CinemaDNG is unwrapped by the MXF reader (`crates/mxf`, #1078),
which hands the frames to it.

Known deviation: the rescale step divides by WhiteLevel minus each pixel's own black level;
DNG chapter 5 uses the plane's largest black level. The two agree unless the black levels
differ across a plane (BlackLevelRepeatDim sites of one colour or BlackLevelDeltaH / V), where
our scale differs by the black-level spread over the white level (well under 1 % for usual
files). Kept for now so the faster pipeline stays bit-identical to the first one.

## Tests

- `tests/it/roundtrip.rs`: container parsing in both byte orders with and without a preview IFD;
  uncompressed samples bit-exact at 8–16 bits, strips and tiles; lossless JPEG bit-exact for every
  predictor and component count, full-swing 16-bit data (every difference category, SSSS = 16),
  restarts and point transform; developed values against hand-computed ones (black / white,
  white balance, ForwardMatrix, BaselineExposure, ActiveArea, DefaultCrop, LinearRaw); CinemaDNG
  FrameRate / TimeCodes / ReelName; hostile files (every truncation, 12 000 mutations under
  `catch_unwind`, cyclic IFDs, huge sizes, bad offsets, zero denominators).
- `tests/it/lossy.rs`: lossy JPEG tiles and strips with one and three components, samples equal to
  the JPEG decoder's, a LinearizationTable expanding 8-bit codes, CFA and 12-bit lossy files
  refused, a tiny file claiming a huge picture, truncations and 3000 mutations.
- `tests/it/jxl.rs` (feature `jxl`): lossless JPEG XL strips made by `cjxl` at test time (skipped
  without it, nothing committed) decode bit-exactly at 8, 10, 12 and 16 bits with one and three
  channels; lossy JPEG XL is close; truncations and mutations; without the feature, JPEG XL is
  named as unsupported.
- `tests/it/opcodes.rs`, `tests/it/equivalence.rs`: opcode lists per stage; the faster pipeline
  against the reference.
- `tests/it/oracle_libraw.rs`: LibRaw's `unprocessed_raw` reads the same samples from our synthetic
  files (uncompressed 8–16-bit in both byte orders, lossless JPEG predictors 1–7 with one and two
  components). Skipped when it is not installed.
- Unit tests: matrix maths, the published sRGB matrix, McCamy temperatures, calibration
  interpolation, sRGB / ForwardMatrix cameras mapping to themselves, demosaic on flat fields
  (every Bayer phase), ramps and edges, black / white / linearisation maths.
