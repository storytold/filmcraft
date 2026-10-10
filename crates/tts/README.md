# filmcraft-tts

Text to speech for FilmCraft's Text to Speech panel (narrations, L2). Engine commands (`tts.*`) and
clip behaviour: `crates/engine/src/narration.rs`.

- `Voice` trait (script in, mono 24 kHz `f32` out), the voice catalogue (`voices`, `voice`), and
  the limits (64 KB script, 30-minute narration, pace 0.5–2×, pitch ±12 semitones).
- `script`: pause markers `[pause 1s]` / `[pause 500ms]` (case-insensitive, capped at 10 s;
  malformed markers are read as text).
- `formant`: the built-in voices **Basic Female** and **Basic Male**, an original source–filter
  formant synthesizer: spelling rules → phone targets → glottal pulse train through three two-pole
  resonators, plus filtered noise for fricatives and bursts. No download, works in the web build.

- `kokoro` (feature `kokoro`): **Kokoro-82M neural voices** in pure Rust on candle (CPU): ALBERT
  text encoder, StyleTTS 2 prosody predictor (durations, F0, energy) and decoder, iSTFTNet
  vocoder with a harmonic-plus-noise source. Weights are read from the official PyTorch
  checkpoint with candle's restricted pickle reader (no code from the file runs); voice packs from
  their zip archive. `config.json` is checked against the architecture this code implements.
- `neural` (feature `kokoro`): the natural voices as `Voice`s: script → `filmcraft-tts-text`
  (normalizer, CMUdict + letter-to-sound, ≤ 400-symbol chunks, pauses) → Kokoro per chunk → exact
  silence for pauses, 120 ms between sentences → vocal pitch with `filmcraft-audio-dsp`'s pitch
  shifter (Kokoro has no pitch input). Pace is Kokoro's speed.
- `catalog`: the natural-voice package (Kokoro-82M, 9 US English voices graded C+ or better on the
  model card, CMUdict; 336 MB), every file pinned to a revision and SHA-256. Downloaded by the
  engine (`tts.downloadVoices`) after the user confirms; never bundled or committed.

Everything is deterministic (the vocoder's noise is seeded). Without features the crate depends
only on `serde` and `thiserror` (and checks for wasm).

## Measured quality and speed (M7.12, 2026-10-08)

50 original sentences (`tests/data/sentences.txt`: numbers, money, times, dates, phone numbers,
acronyms, names) spoken by each voice through the full pipeline, transcribed by Whisper-base
(FilmCraft's own, `filmcraft-speech`), word error rate after lowercasing and removing punctuation
(number formatting such as "six" vs "6" still counts as an error):

| Voice | WER | | Voice | WER |
|---|---|---|---|---|
| Heart | 2.1 % | | Puck | 2.8 % |
| Aoede | 2.4 % | | Michael | 3.2 % |
| Nicole | 2.8 % | | Kore | 3.4 % |
| Bella | 3.0 % | | Fenrir, Sarah | 3.6 % |
| Basic Female (built in) | 96.6 % | | | |

Speed: 2.2–2.3× real time on one desktop CPU (Linux x86-64, load average ≈ 15 from parallel
builds), e.g. 186 s of speech in 84 s; model load 0.5 s. Reproduce with the `narrate` example and
`filmcraft-speech`'s `transcribe` example.

## Limitations

- The built-in voices are **robotic** and nearly unintelligible to a recogniser (above); they are
  the no-download fallback and the test voice.
- English (United States) only. Words outside CMUdict go through letter-to-sound rules and are
  often mispronounced (see `crates/tts-text/README.md`); homographs take the first pronunciation.
- About 2× faster than real time on the CPU: a one-minute narration takes about 30 s. The UI runs
  synthesis as a background job.
- Kokoro's model card states the weights are Apache-2.0 but the repository has no LICENSE file.
- Vocal pitch shifts the voice's base pitch (and its formants slightly); pace scales every duration.

## Tests

`cargo test -p filmcraft-tts`: determinism, valid samples (finite, peak ≤ 0.8, audible RMS), pause
markers produce exactly the requested number of zero samples, pace 2× / 0.5× gives 0.5× / 2× the
length (±5 % / ±10 %), measured pitch (autocorrelation) of the female voice is ≥ 1.5× the male, and
+6 semitones raises it by √2 (±0.12); empty, emoji-only, non-Latin, pause-only and over-long scripts
are refused before rendering; hostile scripts never panic.

Model-dependent tests run when `FILMCRAFT_KOKORO_DIR` names a downloaded package directory
(determinism, whole frames, pace changes length, refusals); the loaders have hostile-input tests
(truncated or oversized voice data, wrong byte order, NaN, non-zip files, mismatched config).

## References

- G. E. Peterson and H. L. Barney, "Control Methods Used in a Study of the Vowels", JASA 24 (1952):
  the published average formant frequencies used for the vowels.
- D. H. Klatt, "Software for a cascade/parallel formant synthesizer", JASA 67 (1980): the standard
  two-pole digital resonator form. No code was read or copied; everything else is original.
- Kokoro (implemented from papers, `config.json` and the checkpoint's tensor names and shapes; no
  reference code read): Y. A. Li et al., "StyleTTS 2" (NeurIPS 2023); T. Kaneko et al., "iSTFTNet"
  (ICASSP 2022); Z. Lan et al., "ALBERT" (ICLR 2020); J. Kong et al., "HiFi-GAN" (NeurIPS 2020);
  X. Wang et al., "Neural source-filter waveform models" (2019); L. Ziyin et al., "Neural Networks
  Fail to Learn Periodic Functions and How to Fix It" (Snake, NeurIPS 2020). Weights: hexgrad,
  Kokoro-82M, Apache-2.0.
