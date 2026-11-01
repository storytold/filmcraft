# filmcraft-audio-dsp

Real-time audio DSP for FilmCraft. Layer **L1**, **no dependencies**, compiles for
`wasm32-unknown-unknown`, no `unsafe`. Clean-room: written from the public standards
(ITU-R BS.1770-4, EBU R128 / Tech 3341 / Tech 3342) and textbook DSP (RBJ cookbook biquads,
Householder FDN, phase vocoder, spectral subtraction with the statistical late-reverberation
model).

## Conventions

- Planar `f32` audio: `&mut [&mut [f32]]`, one slice per channel, equal lengths.
- Effects are built (allocating) with `(sample_rate, channels)`; `process`, `set_param` and
  `reset` never allocate.
- Parameters change between blocks and are smoothed per sample; output does not depend on how
  the stream is cut into blocks (tested for every effect with 0/1/random-size blocks).
- Recursive state is flushed below ~1e-30, so no denormals without CPU FTZ flags.

## Loudness (`loudness`)

`LoudnessMeter::new(sample_rate, channels)` → `process(&[&[f32]])` / `process_interleaved`, then
`momentary()`, `short_term()`, `integrated()`, `loudness_range()`, `sample_peak()`,
`true_peak()` / `true_peak_dbtp()`, `summary()`.

- K-weighting derived for any sample rate by bilinear transform (matches the 48 kHz BS.1770 table).
- Gated integrated loudness (−70 LUFS / −10 LU) and LRA (−70 / −20 LU, 10th–95th percentile) use a
  0.01 LU histogram with exact energy sums: fixed memory for programmes of any length.
- True peak: 4× (2× at 88.2/96 kHz) polyphase Kaiser-sinc interpolation.
- Oracle-tested against ffmpeg's `ebur128` filter (`tests/loudness_oracle.rs`, skipped without ffmpeg):
  M/S within 0.0005 LU, I within 0.007 LU, LRA within 0.04 LU and true peak within 0.045 dB (0.053 dB of the analytic value on an fs/4 tone) on
  sine, pink-noise, speech-like and gated stereo signals at 44.1/48/96 kHz.
- `normalize_gain_db(measured, target)` and `normalize_gain_db_peak_limited(...)` for Normalize /
  Essential Sound loudness auto-match.

## Effects (`effects`)

All implement `AudioEffect` (`id`, `params`, `set_param`, `param`, `reset`, `process`, `latency`).
`effects()` lists `EffectInfo { id, name, category, params, create }`; each `ParamSpec` has
id, name, range, default, `Unit`, log-scale hint and choice labels so UIs can be generated.

| id | effect |
|---|---|
| `parametric_eq` | 8 bands, each peaking / shelf / LP / HP / notch / band-pass, RBJ biquads in TDF-II |
| `simple_eq` | low shelf, mid peak, high shelf |
| `compressor` | peak/RMS detector, soft knee, attack/release, make-up; stereo-linked |
| `gate` | downward expander / gate with range and hold |
| `limiter` | 5 ms look-ahead, true-peak-aware, ceiling guaranteed (latency reported) |
| `delay` | feedback delay, ms or tempo-synced divisions, mix |
| `reverb` | 8×8 Householder FDN, pre-delay, RT60 decay, HF damping, size, mix |
| `amplify`, `channel_volume`, `balance` | gain, per-channel gain, balance / constant-power pan |
| `invert`, `swap_channels`, `fill_left`, `fill_right` | polarity and routing |
| `dehum` | 50/60 Hz notch comb with up to 10 harmonics |
| `denoise` | STFT spectral gating, minimum-statistics noise floor (latency = FFT size) |
| `pitch_shifter` | ±12 semitones, phase vocoder with peak shifting + phase locking (latency = FFT size) |
| `deesser` | high-pass sidechain at `frequency` (2–12 kHz) detects sibilance relative to the broadband level (`threshold`); a dynamic high shelf cuts the band by up to `reduction` dB (1 ms / 60 ms, stereo-linked); exact identity when idle |
| `dereverb` | STFT late-reverb suppression: λ_r(t) = e^{−2Δ·T_d}·λ_x(t − T_d) (Δ = 3 ln10 / `rt60`, T_d ≈ 50 ms), over-subtraction 1 + `amount`, gain floor down to −18 dB, smoothed gains (latency = FFT size) |
| `speech_enhance` | Enhance Speech DSP chain: 80 Hz HPF, de-mud cut (250 / 350 Hz −3 dB), presence (2.5 / 4 kHz +4 dB) and air (+2 dB @ 10 kHz) boosts by `tone`, downward expander below −50 dBFS (3:1, ≤ 18 dB), 3:1 soft-knee compressor above −24 dBFS, +3 dB make-up, `mix` |
| `stereo_width` | mid/side width 0–200 % (100 % = identity, 0 % = mono) |
| `graphic_eq_10/20/30` | constant-Q peaking bands at octave / half-octave / third-octave ISO centres (Q from the bandwidth), ±24 dB, master gain |
| `parametric_eq_full` | Premiere's Parametric Equalizer: master gain, high-pass and low-pass (12–48 dB/oct Butterworth cascades), low shelf, 5 peaking bands, high shelf |
| `notch_filter` | 6 constant-bandwidth cuts (`FilterType::Cut` = 1 + (g − 1)·BP: the width stays f/Q however deep), Narrow / Very Narrow / Super Narrow |
| `scientific_filter` | Bessel / Butterworth / Chebyshev I / elliptic, LP / HP / BP / BS, order 1–12 (`design`: analog prototype → frequency transform at pre-warped edges → bilinear → biquads; elliptic via Landen-transformation Jacobi functions after Orfanidis) |
| `fft_filter` | 8-point gain curve (log-frequency, linear or smoothstep), applied zero-phase by √Hann STFT at 75 % overlap (latency = FFT size) |
| `dynamics_rack` | Dynamics: auto gate (hold) → expander → compressor (auto make-up) → limiter (soft clip), stereo-linked; `transfer_db` gives the static curve |
| `multiband_compressor` | 4 bands with LR4 crossovers and all-pass phase compensation (bands sum to an all-pass: flat magnitude), per-band threshold/ratio/attack/release/gain/solo/bypass, output limiter, channel link |
| `tube_compressor` | RMS detector, 10 dB soft knee, program-dependent release, gentle `tanh(x/2)·2` saturation |
| `chorus_flanger` | chorus (3 voices, 15–35 ms) or flanger (0.3–5 ms, feedback), transience emphasis, mix |
| `flanger` | initial/final delay sweep, stereo phasing, ±feedback, inverted / special / sinusoidal modes |
| `phaser` | 2–12 swept first-order all-passes, depth (octaves below the upper frequency), feedback, stereo phase difference |
| `analog_delay` | Tape / Tape+Tube / Analog: low-passed, saturating feedback (bounded up to 200 %), trash, stereo spread |
| `multitap_delay` | 4 independent feedback taps, levels, mix |
| `convolution_reverb` | uniformly partitioned overlap-save convolution (256-sample partitions, latency 256) with 7 impulses generated here (noise with frequency-dependent exponential decay + early reflections, unit energy); room size (resampling), LF/HF damping, pre-delay, width |
| `surround_reverb` | early reflections + FDN late field, centre input, low/high cut, width, dry/wet (stereo rendering) |
| `click_remover` | LPC residual outlier detection (k·σ, σ from the median), AR forward/backward interpolation of ≤ 64-sample regions (latency 640) |
| `channel_mixer`, `stereo_expander`, `mute` | 2×2 matrix with inversion; mid/side expand + centre pan; ramped mute |
| `distortion`, `guitar_suite` | waveshaper (soft/hard/tube/foldback, asymmetry, tone); compressor → distortion → amp/cabinet voicing → filter |
| `mastering`, `vocal_enhancer` | EQ → reverb → exciter → widener → loudness maximiser (transparent at defaults); fixed Male/Female/Music voicings |
| `binauralizer` | ±angle virtual speakers through a spherical-head model (Woodworth ITD + Brown–Duda head shadow) |
| `ambisonics_panner` | rotates a ±30° source pair (pan/tilt/roll) and re-pans to stereo; identity at 0/0/0 |
| `loudness_meter` | pass-through BS.1770 meter (`LoudnessMeterFx::meter`) |

`AudioEffect::response_db(freq)` (EQs and filters) and `transfer_db(band, input_db)` (dynamics)
expose the analytic curves for the graphical effect editors. Modulation effects start their LFO
at the beginning of a chain, so a random-access render may differ in LFO phase from continuous
playback.

`effects::premiere_tests` checks every effect: neutral settings are (delayed) identity, output is
deterministic and latency constant (impulse lands at `latency()`), plus a level / frequency-response
test on generated tones, noise or impulses and a realtime-factor floor.

Essential Sound builds on these (Repair: `denoise`, high-pass via `parametric_eq`, `dehum`,
`deesser`, `dereverb`; Clarity: `compressor`, `simple_eq`, `speech_enhance`; Creative: `reverb`,
`stereo_width`).

**Enhance Speech decision.** No machine-learning model is bundled: we found no openly licensed
speech-enhancement model that is also practical to ship here (pure Rust, wasm, asset rules).
DeepFilterNet (MIT/Apache-2.0, with a Rust implementation on `tract`) is the candidate for a future
optional integration behind a trait outside this dependency-free crate, once its weights' licence and
size are vetted as an asset. Until then `speech_enhance` is the DSP chain above.

Measured on synthetic signals (`effects::essential::tests`, 48 kHz):

| Effect | Signal | Result |
|---|---|---|
| `deesser` | 150 Hz harmonic voice + 5–10 kHz noise bursts | sibilance −5.1 dB (8 dB max) / −9.8 dB (16 dB max) during bursts; voice band ±0.01 dB; non-sibilant passages ±0.001 dB |
| `dereverb` (100 %) | harmonic syllables (100 ms / 400 ms gaps) + Polack-model late reverb, RT60 0.8 s | tail-to-burst energy −5.0 → −10.1 dB (5.0 dB better); burst energy −1.7 dB |
| `speech_enhance` | voice + fricative noise syllables over −60 dBFS noise | presence/mud tilt +5.5 dB; noise between syllables −8.7 dB; overall level +1.0 dB |
| Dialogue chain | `denoise` + `parametric_eq` (2 HP bands) + `dehum` + `deesser` + `dereverb` + `compressor` + `simple_eq` + `speech_enhance` + `reverb`, 60 s stereo | 1.36 s = **44× realtime** on one core (release, Apple Silicon; `cargo test --release -p filmcraft-audio-dsp dialogue_chain_realtime -- --ignored --nocapture`) |

## Ducking analysis (`ducking`)

Pure functions for Essential Sound auto-ducking:

- `envelope_db(channels, sample_rate, hop_s, window_s)`: centred-window RMS level per hop (dBFS,
  channel-averaged power, floor −120).
- `activity(env_db, hop_s, threshold_db, min_on_s, bridge_s)`: regions above the threshold; gaps
  shorter than `bridge_s` are merged first, then regions shorter than `min_on_s` are dropped.
- `sensitivity_threshold_db(s)`: Sensitivity 0–10 → −20 − 4·s dBFS.
- `duck_keyframes(regions, clip_start, clip_end, base_db, reduce_db, fade_s)`: (time, dB) volume
  keyframes — ramp down over `fade_s` before each region, hold, ramp up after; overlapping ramps
  merge; clamped to the clip (a ramp cut by an edge gets the ramp's value at the edge); strictly
  increasing times.

## Audio synchronisation (`sync`)

`find_offset(a, b, sample_rate, &SyncOptions)` returns the lag `L` with `b[n] ≈ g·a[n + L]`, a
sub-sample estimate, and two trust figures (`confidence` = peak / RMS of the correlation,
`distinct` = peak / next peak > 50 ms away; `reliable()` = ≥ 8 and ≥ 1.5).

- Coarse: DC removal + normalisation, Blackman-windowed-sinc decimation to ≤ 8 kHz (further for
  long recordings, so the transform stays ≤ 4 M points), GCC-PHAT-β (`R = A·B*/|A·B*|^0.75`,
  40 Hz – 0.4·rate band) with both signals packed into one complex FFT.
- Refine: the same correlation at the full rate on the loudest common window (≤ 2¹⁷ samples), ±2
  coarse steps around the coarse peak, parabolic interpolation.
- Tested within one sample on speech-like multi-microphone signals (gains −20…+10 dB, SNR down to
  3 dB, coloured microphones, fractional delays: worst 0.4 samples integer / 0.11 samples
  sub-sample), a 0 dB SNR room with a strong echo, inverted polarity, unrelated signals
  (unreliable) and `max_lag`. Two 10-minute recordings: 2.4 s (release, one core).

## Known limits

- Effects assume ≤ the channel count they were built with; extra channels pass through
  (channel-generic gain effects process all of them).
- DeNoise adapts continuously; a perfectly stationary tone is (by design) treated as noise after
  ~1.5 s. There is no explicit "learn noise print" mode yet.
- Filter type / choice changes switch immediately (continuous parameters are smoothed).

Settled stereo reverb and mixer automation optimization: [measurements and correctness](PERFORMANCE.md).
