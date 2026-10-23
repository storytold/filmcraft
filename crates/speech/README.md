# filmcraft-speech

Speech-to-text for FilmCraft's Text panel ▸ Transcript (L2). User-facing behaviour, commands and
model handling: [docs/transcripts.md](../../docs/transcripts.md).

- `Transcriber` trait (mono 16 kHz in, word-timed `filmcraft_project::Transcript` out) and
  `FixedTranscriber` for tests and agents.
- `models`: Whisper model catalogue (pinned revisions, SHA-256, sizes, licences) and, with feature
  `download`, the verified downloader (ureq + rustls + RustCrypto). Weights are never bundled.
- `whisper` (feature `whisper`): Whisper inference on candle (CPU, pure Rust).
- `parakeet` (feature `whisper`): NVIDIA Parakeet TDT inference on candle, with per-token times
  from the duration decoder.
- `mel`, `vad`, `diarize`: log-mel front end, word-bound tightening, MFCC-clustering speaker labels.

Both features are off by default and are never enabled for wasm (`cargo xtask wasm` checks the
crate without them).

## References

Implemented from the published description of the model: A. Radford et al., "Robust Speech
Recognition via Large-Scale Weak Supervision" (OpenAI, 2022), the model cards and configuration
files published with the weights (`config.json`, `generation_config.json`, `tokenizer.json`), and
the candle tensor library's public API. Weights: OpenAI Whisper, MIT licence.

Parakeet is implemented from D. Rekesh et al., "Fast Conformer with Linearly Scalable Attention for
Efficient Speech Recognition" (2023), H. Xu et al., "Efficient Sequence Transduction by Jointly
Predicting Tokens and Durations" (2023), Z. Dai et al., "Transformer-XL" (2019, relative-position
attention), and the configuration files and tensor names published with the weights
(`nvidia/parakeet-tdt-0.6b-v3`: `config.json`, `processor_config.json`, `tokenizer.json`).
Weights: NVIDIA Parakeet TDT 0.6B v3, CC-BY-4.0.
