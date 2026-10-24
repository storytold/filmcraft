# filmcraft-aac

A clean-room AAC-LC encoder and decoder (ISO/IEC 14496-3, MPEG-4 Audio, Low Complexity profile),
written from the standard and the public psychoacoustics literature. Layer L0: it depends only on
`filmcraft-bitstream` and `thiserror` (plus optional `rayon`), has no `unsafe`, and builds for
`wasm32-unknown-unknown`.

No FFmpeg/FAAC/FDK-AAC/libfaad or other GPL/LGPL code was read or ported. ffmpeg is used only as an
external **test oracle** (decoding our streams, producing reference streams).

## API

```rust
use filmcraft_aac::{Decoder, Encoder, EncoderConfig, BitrateMode, AdtsHeader, split_adts};

// Encoder: planar f32 (±1.0) → raw access units
let mut enc = Encoder::new(EncoderConfig::cbr(48_000, 2, 128_000))?;   // or EncoderConfig::vbr(48_000, 2, 3.0)
let mut aus: Vec<Vec<u8>> = enc.encode(&[&left, &right]);              // 0..n AUs per call, any chunk size
aus.extend(enc.flush());                                               // zero-pads and drains the look-ahead
let asc = enc.audio_specific_config();                                 // bytes for the MP4 `esds`
let delay = enc.priming_samples();                                     // 1024 → edit list media_time
let stats = enc.stats();                                               // frames, bytes, bitrate(), short blocks, M/S, TNS

// Decoder: AudioSpecificConfig + raw AUs → planar f32, 1024 samples per channel per AU
let mut dec = Decoder::new(&asc)?;
let pcm: Vec<Vec<f32>> = dec.decode(&aus[0])?;

// ADTS
let frames = split_adts(&adts_bytes)?;               // [(AdtsHeader, raw payload)]
let mut dec = Decoder::from_adts(&frames[0].0)?;     // channel config 0 → layout from the in-band PCE
```

`EncoderConfig` fields: `sample_rate` (the 13 AAC rates, 7.35–96 kHz), `channels` (1–8), `mode`
(`BitrateMode::Cbr(bps)` / `BitrateMode::Vbr(quality 1.0..=5.0)`), `bandwidth` (Hz, `None` = automatic
from bitrate/quality), `tns`, `ms_stereo`, `window_shape` (sine/KBD), `adts` (prefix each AU with a
7-byte ADTS header).

**Channel order** is AAC element order, for both encoder input and decoder output:

| channels | configuration | order |
|---|---|---|
| 1 | 1 | C |
| 2 | 2 | L R |
| 3 | 3 | C L R |
| 4 | 4 | C L R Cs |
| 5 | 5 | C L R Ls Rs |
| 6 | 6 | C L R Ls Rs LFE |
| 7 | 0 + PCE | C L R Ls Rs Cs LFE (PCE in the ASC; repeated in every AU when ADTS-framed) |
| 8 | 7 | C Lc Rc L R Ls Rs LFE |

The decoder also reads configurations 11 (6.1: C L R Ls Rs Cs LFE) and 12 (7.1: C L R Ls Rs Lsr Rsr
LFE) from ISO/IEC 14496-3 Amd 4; ffmpeg 9 writes 7.1 AAC as configuration 12.

`Decoder::layout()` reports the element layout (it follows a PCE for configuration 0).

## Encoder

Per 1024-sample frame and channel element:

1. **Transient detection**: energy of the first-difference (high-passed) signal in 128-sample
   sub-blocks against the mean of the previous eight; one frame of look-ahead selects
   `ONLY_LONG` / `LONG_START` / `EIGHT_SHORT` / `LONG_STOP`. Short windows are grouped by energy
   similarity (an attack starts a new group). LFE is always long.
2. **MDCT** (2048/256 points) via a DCT-IV on an N/4-point radix-2 complex FFT; sine or KBD
   (α = 4 long, 6 short) windows.
3. **Psychoacoustic model** in the MDCT domain: band energies, tonality from the spectral flatness of
   a ≥16-line neighbourhood, Schroeder's spreading function in the Bark domain, tonality-dependent
   masking offset (30 dB tonal … 6 dB noise), Terhardt threshold in quiet (capped at 40 dB SPL) as a
   floor, pre-echo control (threshold ≤ 2× previous frame), perceptual entropy.
4. **TNS** on long windows: Levinson–Durbin (order 8) over the spectrum above 1.4 kHz; applied when the
   prediction gain exceeds 1.4, with 4-bit reflection coefficients.
5. **M/S** per band (and group) for channel pairs (always common window): chosen when the estimated
   perceptual cost of M and S (with the stricter L/R threshold, halved) is below that of L and R.
6. **Rate–distortion loop**: one multiplier λ scales every band's allowed noise. Each band gets the
   coarsest scalefactor whose *measured* quantisation error meets its target (binary search,
   `0.4054` rounding), scalefactor differences are kept within ±60, codebooks and sections are chosen
   by dynamic programming over all 12 codebook states, and λ is bracketed and bisected until the frame
   fits its budget (CBR) or fixed from the quality (VBR).
7. **Bit reservoir** (CBR): frame budgets follow the perceptual entropy (short-window frames get extra),
   bounded by the 6144-bit-per-channel decoder buffer; overflow is padded with fill elements and the
   final frame drains the reservoir, so the stream is exactly `frames × bitrate × 1024 / rate`.

Bandwidth: automatic from bitrate per full-bandwidth channel (e.g. 64 kbps/ch → 16 kHz, 96 → 19 kHz,
≥144 → 22 kHz) or from quality in VBR.

Delay: 1024 priming samples (`priming_samples()`); after `flush()` the decoded stream minus the priming
covers every input sample (`ceil(n / 1024) + 1` access units).

Performance (Apple Silicon, release, single thread): 10 s of 44.1 kHz stereo encodes at ~85× realtime
and decodes at ~1000× realtime. With the `rayon` feature channels are quantised in parallel inside the
rate loop.

## Decoder

Full AAC-LC syntax: SCE/CPE/LFE, DSE, FIL, PCE (reconfigures a configuration-0 stream), all window
sequences and both window shapes (shape of the previous frame for the left half), section data,
scalefactors, pulse data, TNS (all-pole synthesis, long and short), M/S (per band / all bands),
intensity stereo (in/out of phase, M/S-inverted), PNS (with correlated noise for `ms_used` noise bands
as specified), IMDCT and overlap-add. Output is planar f32 at ±1.0 full scale. All reads are bounds
checked: malformed access units return `Err`, never panic.

## Test results

All produced by `cargo test -p filmcraft-aac -- --nocapture` (ffmpeg 9.0 at `/opt/homebrew/bin` as
oracle; tests skip when it is absent). Fixtures go to `target/fixtures/aac/`.

### Encoder quality / bitrate (2 s synthetic signals, decoded by ffmpeg)

SNR is full-band; bwSNR is measured after low-passing both signals at the coded bandwidth;
segSNR averages 1024-sample segments above −50 dBFS (clamped to −10…60 dB). "us-vs-ff" is the
largest difference between our decoder and ffmpeg's on the same stream. Every stream decodes without
ffmpeg errors, no output clips (peak < 1.0), and CBR is within ±0.1% of target (test limit ±5%).

| signal | kbps | actual | bw Hz | SNR | bwSNR | segSNR | short frames | us-vs-ff |
|---|---|---|---|---|---|---|---|---|
| sweep 2ch 44.1k | 64 | 64.00 | 11000 | 12.1 | 36.1 | 43.6 | 1 | 5e-7 |
| sweep 2ch 44.1k | 128 | 127.92 | 16000 | 26.1 | 27.3 | 54.2 | 1 | 3e-7 |
| sweep 2ch 44.1k | 320 | 319.90 | 21609 | 46.6 | 46.6 | 59.6 | 1 | 3e-7 |
| white noise 2ch 44.1k | 64 | 64.00 | 11000 | 1.3 | 3.3 | 1.3 | 1 | 5e-7 |
| white noise 2ch 44.1k | 320 | 319.91 | 21609 | 16.4 | 16.5 | 16.6 | 1 | 1.5e-6 |
| speech-like 2ch 48k | 64 | 64.00 | 11000 | 13.6 | 14.0 | 15.2 | 13 | 2e-7 |
| speech-like 2ch 48k | 128 | 127.98 | 16000 | 21.3 | 23.0 | 20.7 | 13 | 2e-7 |
| speech-like 2ch 48k | 320 | 319.96 | 22000 | 31.2 | 31.5 | 31.3 | 13 | 2e-7 |
| chords 2ch 44.1k | 64 | 64.01 | 11000 | 12.4 | 12.4 | 20.6 | 4 | 2e-7 |
| chords 2ch 44.1k | 128 | 128.00 | 16000 | 20.1 | 20.1 | 38.5 | 4 | 2e-7 |
| chords 2ch 44.1k | 320 | 319.99 | 21609 | 35.9 | 35.9 | 55.5 | 4 | 2e-7 |
| clicks 2ch 48k | 64 | 64.00 | 11000 | 10.4 | 11.2 | 48.6 | 11 | 9e-8 |
| clicks 2ch 48k | 128 | 128.00 | 16000 | 20.2 | 25.1 | 53.9 | 11 | 2e-7 |
| clicks 2ch 48k | 320 | 319.95 | 22000 | 39.6 | 39.6 | 58.0 | 11 | 1e-7 |
| speech-like 1ch 44.1k | 64 | 64.00 | 16000 | 18.4 | 19.2 | 18.0 | 15 | 2e-7 |
| chords 5.1 48k | 320 | 320.00 | 16000 | 27.2 | 27.2 | 37.9 | 9 | 2e-7 |
| chords 3/4/5/7/8ch 48k | 192–448 | exact | 16000 | 26–28 | 26–28 | 38 | 7–13 | 2e-7 |
| chords 2ch 8 k … 96 k (10 rates) | 24–256 | exact | 3.9–21 k | 13–38 | | 17–47 | | ≤3e-7 |

Reference points from ffmpeg's own AAC encoder on the same signals (stereo 48 kHz, segSNR):
sweep 128k 39.7, speech 128k 21.2, chords 128k 32.8, clicks 128k 43.3 (ours: 52/20/39/54). Pure SNR
is not a perceptual measure: the model deliberately leaves noise under the masking threshold, and
full-band white noise is the worst case by design.

VBR (`Vbr(q)`, 44.1 kHz stereo): chords q1 62 kbps / q3 139 / q5 225 (segSNR 18 / 39 / 51);
speech-like q1 42 / q3 166 / q5 272 kbps (segSNR 13 / 29 / 41). Bitrate and quality grow
monotonically with q.

### Decoder accuracy vs ffmpeg (streams from `ffmpeg -c:a aac`, PNS off)

| stream | max abs error |
|---|---|
| stereo 44.1k 128k / 48k 256k / 96k 256k | 1.2e-7 / 1.2e-7 / 1.3e-7 |
| mono 48k 64k / 22.05k 32k / 16k 24k / 8k 16k | 1.5e-7 / 1.2e-7 / 1.2e-7 / 8.9e-8 |
| stereo 44.1k 96k with intensity stereo + M/S + TNS | 1.2e-7 |
| stereo 32k 48k with intensity stereo | 1.2e-7 |
| 3.0, 5.1 (320k), 7.1 wide with PCE (448k) | 1.2e-7, 1.5e-7, 1.3e-7 |
| stereo 44.1k 160k `-aac_coder fast` | 1.2e-7 |

Our own encoder's streams: ≤1.6e-6 against ffmpeg for every case above. PNS (ffmpeg's encoder does not
emit it, so streams are hand-built): total energy within 0.15 dB of ffmpeg; per-frame energy within
0.8 dB (random vectors differ). For noise bands flagged `ms_used` we produce identical L/R noise as
the standard specifies; ffmpeg produces independent noise there.

### Robustness

12 000 mutated access units (bit flips, byte overwrites, truncation, extension, splices, random
buffers) over mono/stereo/5.1/7-channel streams, 20 000 random `AudioSpecificConfig`s and 2 000 random
ADTS streams: no panics. The encoder accepts NaN/±inf/over-range input, any chunk size, and missing
channels.

## Clean-room notes

- Syntax, tables (scalefactor bands, TNS limits), filterbank, TNS, PNS, M/S and intensity stereo are
  implemented from ISO/IEC 14496-3.
- The Huffman codebooks (`src/huffman_tables.rs`) are the normative values of the standard's Annex
  4.A, recovered by black-box probing of the ffmpeg *binary* as an oracle: `tools/` contains the
  Python (stdlib-only) scripts that craft single-frame streams repeating a candidate bit string, fit the
  decoded output to the IMDCT basis, and walk the code tree. Every table is verified to be a complete
  prefix code (Kraft sum exactly 1) covering each index once (unit test), and the decoder matches
  ffmpeg to ~1e-7 on real streams.
- Psychoacoustics follow textbook models (Bark scale, Schroeder spreading, Terhardt ATH, Johnston-style
  tonality/SFM offsets, perceptual entropy).

## Gaps / not implemented

- HE-AAC v1/v2 (SBR/PS): explicit signalling is parsed and only the AAC-LC core is decoded (half-rate,
  no SBR); implicit SBR fill elements are skipped.
- AAC Main (prediction), SSR (gain control), LTP, ER/LD/ELD object types, coupling channel elements
  (CCE → `Err(Unsupported)`), 960-sample frames.
- Encoder: no PNS or intensity stereo (decoder supports both), no TNS on short windows, no pulse data;
  window shape is fixed per stream (sine or KBD). Short blocks at onsets cost segmental SNR on
  speech-like material relative to long windows + TNS; perceptually they avoid pre-echo.
- No error concealment: a corrupt AU returns `Err`; the caller may substitute silence.
- ADTS with CRC is parsed (header skipped, CRC not verified); multi-block ADTS frames are split per
  frame only.
