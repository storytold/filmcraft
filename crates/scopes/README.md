# filmcraft-scopes

Video scope maths for the Video Scopes panel (layer L3; depends only on `filmcraft-color` and
`serde`; no UI, builds for wasm32). The UI (`crates/ui-egui/src/panels/scopes.rs`) draws what this
crate computes; `scopes.read` (`crates/engine/src/scopes.rs`) returns it as numbers for agents.

| Item | What |
|---|---|
| `Signal` | a frame's R'G'B' code values, decimated by nearest-sample picking to at most 480 × 270 (`from_rgba8`, `from_rgba_f32_with` + `linear_to_pq` for HDR, `from_fn`) |
| `waveform` | RGB / Luma / YC / YC no Chroma: one count grid per trace (columns = signal width, `rows` levels) |
| `parade` | RGB / YUV / RGB-White traces (drawn side by side) |
| `histogram` | 256 bins of R', G', B', Y'; samples below 0 / above 1 counted separately |
| `vectorscope_yuv` | Cb right, Cr up, ±0.6; `targets(matrix, amplitude)` gives the six colour-bar targets, `SKIN_TONE_DEG` = 123° |
| `vectorscope_hls` | hue as the angle (red at the matrix's YUV red angle, then yellow, green, cyan, blue, magenta counter-clockwise), HLS saturation as the radius |
| `summary` | levels (min / max / mean in percent) per channel and per column range, densest vectorscope cells with angle and magnitude, coarse vectorscope |
| `paint` | count grids → premultiplied RGBA8 (log intensity, additive traces, Brightness gain; vectorscope points spread over 3 × 3) |

Settings types (all serde, camelCase): `ScopeKind`, `WaveformType`, `ParadeType`, `ColorSpace`
(Automatic / Rec. 601 / Rec. 709 / Rec. 2100 → BT.601 / BT.709 / BT.2020 NCL matrices),
`Scale` (8 Bit / Float / HDR), `Brightness`, `Targets` (75 % / 100 %).

## Definitions

- Y'CbCr from R'G'B' with the matrix's Kr/Kb (ITU-R BT.601-7, BT.709-6, BT.2020-2 NCL), Cb and Cr in
  −0.5…0.5; YC chroma amplitude = √(Cb² + Cr²), plotted at Y' ± C.
- Waveform rows span 0…1 with Clamp Signal (values clamped first) or −0.1…1.1 without (values
  outside are dropped). With 256 rows an 8-bit code value `c` lands on row `c`.
- HDR: linear working values (1.0 = 203 cd/m², BT.2408) → SMPTE ST 2084 PQ code values; the axis is
  0…10 000 cd/m². SDR frames on the HDR scale use BT.1886 (γ 2.4, 100 cd/m² white).

## Tests

`src/tests.rs` uses generated frames with known values: a flat colour lands in exact histogram bins;
a 256-step ramp fills every bin and puts column `x` on row `x`; 75 % colour bars put each parade
column on its code value and each bar on its vectorscope target cell (BT.601, BT.709, BT.2020), with
the centre holding white + black; BT.709 red sits at (103, 21) on the 255² grid and 102.91°
(BT.601: 108.65°); HLS hues land at red + 60° steps on the outer ring; YC chroma brackets luma;
Clamp Signal; decimation; NaN and empty frames; summaries; painting.

## Speed

`cargo test --release -p filmcraft-scopes perf -- --ignored --nocapture` (1920×1080 RGBA8 noise,
Apple M4 Pro, one thread, machine shared with other builds):

| Step | ms |
|---|---|
| decimate 1080p → 480 × 270 | 0.30 |
| waveform RGB / YC | 0.75 / 0.88 |
| parade RGB-White | 0.67 |
| histogram | 0.35 |
| vectorscope YUV / HLS | 0.67 / 0.81 |

Every scope is well under the 3 ms budget; the UI recomputes only when the frame or a setting changes.
