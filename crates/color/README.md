# filmcraft-color

Colour science for FilmCraft (layer L0, no workspace dependencies): Y'CbCr matrices, transfer
functions, camera log curves, RGB gamuts, colour-managed input/output transforms with tone and
gamut mapping, and LUT files.

| Module | Contents |
|---|---|
| `lib.rs` | `Matrix`/`Transfer`/`Primaries`/`Range`/`ColorInfo` (stream metadata), Y'CbCr↔R'G'B', sRGB, PQ (ST 2084), HLG (ARIB STD-B67), HSL, hex colours |
| `log` | camera log curves (encode/decode), their signal domains |
| `spaces` | gamuts and 3×3 matrices (SMPTE RP 177), `ColorSpace` (Interpret Footage choices), `WorkingSpace`, `ColorPipeline` |
| `transform` | `InputTransform` (source → working), `OutputTransform` (working → monitor / export), BT.2390 tone mapping, gamut compression |
| `lut` | `.cube` (1D, 3D, shaper + 3D) and `.3dl` parse/write, tetrahedral interpolation |

## Working units

The compositor works in linear light with **1.0 = reference white**: SDR white for SDR media, and
**203 cd/m²** for HDR (ITU-R BT.2408, "HDR Reference White"). SDR media keeps its values in an HDR
sequence (the BT.2408 SDR-in-HDR mapping), HDR highlights are values above 1.0, and log media
decodes to scene-linear reflectance (18 % grey = 0.18). Working primaries are BT.709 for a Rec. 709
sequence and BT.2020 for Rec. 2100 PQ/HLG sequences or when "wide gamut" is on.

## Camera log curves

Each curve is implemented from the manufacturer's published document. "Domain" is the normalisation
the document uses: **CV** = 10-bit code value / 1023 (full scale), **IRE** = video-range normalised
(code 64 → 0, code 940 → 1). `log::signal_from_normalized` converts the decoder's normalised value
(which already accounts for the file's range flag) into the curve domain, so the same formulas work
for limited- and full-range files.

| Curve | Gamut | Source | Domain | Decode (signal y → linear x) |
|---|---|---|---|---|
| Sony S-Log3 | S-Gamut3.Cine / S-Gamut3 | Sony, *Technical Summary for S-Gamut3.Cine/S-Log3 and S-Gamut3/S-Log3* (2014) | CV | y ≥ 171.2102946929/1023: x = 10^((1023y − 420)/261.5)·0.19 − 0.01; else x = (1023y − 95)·0.01125/(171.2102946929 − 95) |
| Panasonic V-Log | V-Gamut | Panasonic, *V-Log/V-Gamut Reference Manual* (2014) | CV | y < 0.181: x = (y − 0.125)/5.6; else x = 10^((y − d)/c) − b, b = 0.00873, c = 0.241514, d = 0.598206 |
| Canon Log 2 | Cinema Gamut | Canon, *Canon Log Gamma Curves* white paper (2018) | IRE | x' = ±(10^(±(y − 0.092864125)/0.24136077) − 1)/87.09937546 (sign by y vs 0.092864125); x = 0.9·x' (Canon's x is reflectance / 0.9) |
| Canon Log 3 | Cinema Gamut | same | IRE | y < 0.097465473: x' = −(10^((0.12783901 − y)/0.36726845) − 1)/14.98325; y ≤ 0.15277891: x' = (y − 0.12512219)/1.9754798; else x' = (10^((y − 0.12240537)/0.36726845) − 1)/14.98325; x = 0.9·x' |
| ARRI LogC3 (EI 800) | ARRI Wide Gamut 3 | ARRI, *ALEXA Log C Curve — Usage in VFX* (2017), EI 800 row | CV | y > e·cut + f: x = (10^((y − d)/c) − b)/a; else x = (y − f)/e; cut 0.010591, a 5.555556, b 0.052272, c 0.247190, d 0.385537, e 5.367655, f 0.092809 |
| ARRI LogC4 | ARRI Wide Gamut 4 | ARRI, *ARRI LogC4 Logarithmic Color Space — Specification* (2022) | CV | a = (2¹⁸ − 16)/117.45, b = 928/1023, c = 95/1023, s = 7·ln2·2^(7 − 14c/b)/(a·b), t = (2^(6 − 14c/b) − 64)/a; y ≥ 0: x = (2^(14(y − c)/b + 6) − 64)/a; else x = y·s + t |
| Apple Log | BT.2020 | Apple, *Apple Log Profile* white paper (2023) | IRE | R0 = −0.05641088, Rt = 0.01, c = 47.28711236, β = 0.00964052, γ = 0.08550479, δ = 0.69336945; P ≥ c(Rt − R0)²: x = 2^((P − δ)/γ) − β; else x = √(P/c) + R0 |
| DJI D-Log | D-Gamut | DJI, *White Paper on D-Log and D-Gamut of DJI Cinema Color System* (2017) | CV | y ≤ 0.14: x = (y − 0.0929)/6.025; else x = (10^(3.89616y − 2.27752) − 0.0108)/0.9892 |

The encoders are the exact inverses given in the same documents. DJI **D-Log M** is not
implemented: DJI has not published its formula (only conversion LUTs), and we do not derive curves
from third-party LUTs. (Checked again in October 2026: DJI's published white papers cover D-Log /
D-Gamut only; D-Log M remains unsupported, so D-Log M footage should be interpreted with a LUT.)

Gamut primaries (all D65 white, x/y):

| Gamut | R | G | B | Source |
|---|---|---|---|---|
| BT.709 | 0.640, 0.330 | 0.300, 0.600 | 0.150, 0.060 | ITU-R BT.709-6 |
| BT.2020 | 0.708, 0.292 | 0.170, 0.797 | 0.131, 0.046 | ITU-R BT.2020-2 |
| P3 D65 | 0.680, 0.320 | 0.265, 0.690 | 0.150, 0.060 | SMPTE EG 432-1 |
| S-Gamut3.Cine | 0.766, 0.275 | 0.225, 0.800 | 0.089, −0.087 | Sony technical summary |
| S-Gamut3 | 0.730, 0.280 | 0.140, 0.855 | 0.100, −0.050 | Sony technical summary |
| V-Gamut | 0.730, 0.280 | 0.165, 0.840 | 0.100, −0.030 | Panasonic reference manual |
| Cinema Gamut | 0.740, 0.270 | 0.170, 1.140 | 0.080, −0.100 | Canon white paper |
| ARRI Wide Gamut 3 | 0.6840, 0.3130 | 0.2210, 0.8480 | 0.0861, −0.1020 | ARRI Log C document |
| ARRI Wide Gamut 4 | 0.7347, 0.2653 | 0.1424, 0.8576 | 0.0991, −0.0308 | ARRI LogC4 specification |
| D-Gamut | 0.71, 0.31 | 0.21, 0.88 | 0.09, −0.08 | DJI white paper |

Matrices are derived from the chromaticities (SMPTE RP 177 normalised primary matrix); tests check
the result against the published BT.709 RGB→XYZ and BT.2087 BT.709→BT.2020 matrices.

## HDR transfer functions

* **PQ** (SMPTE ST 2084 / BT.2100): decoded to absolute cd/m², then divided by 203.
* **HLG** (ARIB STD-B67 / BT.2100): inverse OETF to scene light, then the BT.2100 OOTF
  `Fd = Lw·Ys^(γ−1)·E` with Lw = 1000 cd/m², γ = 1.2 and Ys from BT.2020 luma, then ÷ 203. A 75 %
  HLG signal lands at reference white (BT.2408). HLG export applies the inverse OOTF and the OETF.

## Tone mapping (HDR/log → SDR)

When an HDR or log source enters a Rec. 709 working space with **Auto Tone Map Media** on (the
default), and on the monitors of an HDR sequence, FilmCraft applies the **ITU-R BT.2390 EETF**:

1. m = max(R, G, B) in working units, converted to cd/m² (×203) and PQ-encoded, normalised by the
   PQ value of the source peak;
2. below the knee KS = 1.5·maxLum − 0.5 (maxLum = PQ(target peak)/PQ(source peak)) the value is
   unchanged; above it a Hermite spline maps the rest of the range onto [KS, maxLum];
3. RGB is scaled by m'/m, which keeps hue and saturation.

Source peak: the content's mastering peak for PQ when known (default 1000 cd/m²), 1000 cd/m² for
HLG, and for log media the curve's own peak capped at 4000 cd/m² (so the knee starts above mid
grey). Target peak: 203 cd/m² = SDR white. HDR reference white then lands at ≈ 0.79 linear (≈ 90 %
on the sRGB curve).

## Grading signal (`grade`)

Lumetri's controls work on a 0…1 signal. `GradeSpace` picks it from the working space:

| Working space | Signal of working-linear `c` (1.0 = 203 cd/m² in HDR) | 1.0 means |
|---|---|---|
| Rec. 709 | sRGB curve, clamped back to 0…1 | SDR white |
| Rec. 2100 PQ | `PQ⁻¹(c·203/10000) / PQ⁻¹(W/10000)` (SMPTE ST 2084) | HDR White `W` cd/m² |
| Rec. 2100 HLG | `F = c·203/W`, `Y = BT.2020 luma(F)`, `E = F·Y^((1−γ)/γ)`, signal = HLG OETF(E), γ = 1.2 + 0.42·log10(W/1000) (BT.2100 inverse OOTF, note 5f) | display peak `W` |

`W` is Lumetri's HDR White (Basic Correction) or HDR Range (Curves), default 1000 cd/m². HDR
values above `W` are signals above 1 and are kept (PQ to 10 000 cd/m², HLG to signal 1.5). Checked
values: PQ 100 / 203 / 1000 cd/m² = 0.508 / 0.581 / 0.752 of the plain PQ signal (BT.2408); HLG on
a 1000 cd/m² display: 203 cd/m² = 0.75, 26 cd/m² (18 % grey) ≈ 0.38 (BT.2408).

## HDR static metadata

`HdrMetadata` (mastering display max / min luminance, MaxCLL, MaxFALL; 0 = unknown) gives the
tone-mapping source peak of PQ media: MaxCLL when known (the brightest pixel of the content),
else the mastering peak, clamped to 203…10 000 cd/m²; without metadata 1000 cd/m² is assumed.

## Gamut mapping

* **Input** (camera gamuts wider than the working gamut): per-channel distance compression towards
  the achromatic axis, after the ACES Reference Gamut Compression: for each channel
  d = (max − c)/max; distances below 0.8 are unchanged, distances from 0.8 to 1.25 are compressed
  into 0.8–1.0 with a power-1.2 curve. Colours within 80 % of the boundary are untouched.
* **Display-referred wide-gamut input** (BT.2020 / P3 / PQ / HLG media into a narrower working
  gamut) and
* **Output** (BT.2020 working → BT.709 monitor/export): colours with a negative component are
  desaturated towards their BT.709 luminance just enough to fit (`desaturate_into_gamut`); in-gamut
  colours, such as BT.709 media in a wide-gamut sequence, come back exactly.

## LUTs

* `.cube`: the published Cube LUT 1.0 text format — `TITLE`, `LUT_1D_SIZE` (2–65536), `LUT_3D_SIZE`
  (2–256), `DOMAIN_MIN`/`DOMAIN_MAX`, legacy `LUT_1D_INPUT_RANGE`/`LUT_3D_INPUT_RANGE`, comments,
  CRLF. A file with both sizes is a 1D shaper followed by a cube. Red changes fastest.
* `.3dl`: optional `3DMESH` / `Mesh <in> <out>` header, the input mesh line (non-uniform meshes
  become a 1D shaper), `n³` integer rows with blue fastest; output depth from the header or the
  largest value. We write 33-point, 10-bit-mesh, 12-bit-output files.
* 3D interpolation is **tetrahedral**. Tests compare it with a brute-force reference (barycentric
  coordinates in each of the six Kuhn tetrahedra, solved with a 3×3 inverse) on random points for
  sizes 2–33, check exactness on affine LUTs and at grid nodes, and round-trip both formats.

FilmCraft ships no LUT files. Its built-in looks and camera conversions are generated from code
(`filmcraft-render`), and are written to disk only when the user exports them.

## Tests

`cargo test -p filmcraft-color`: encode∘decode round trips for every curve (−1 % to 8× and over
the full signal range in both file ranges), published reference points (S-Log3 95/420/598, V-Log
128/433/602, LogC3 18 % → 0.391, LogC4 18 % → 0.2784, Canon Log 2/3 18 % → 39.2 %/34.3 % IRE, Apple
Log 18 % → 0.488, D-Log 0 → 0.0929), PQ/HLG reference white, EETF continuity/monotonicity and peak,
hue preservation, gamut compression, BT.709/BT.2087 matrices, LUT interpolation and file round
trips.

Explicit container Apple Log metadata is represented by `Transfer::AppleLog`
and maps to `ColorSpace::AppleLog`, using the existing published curve above.
It has no ISO transfer-characteristic code point: ordinary BT.2020 / BT.709
metadata does not select Apple Log. Manual interpretation still uses the same
input transform and takes priority over detection.
