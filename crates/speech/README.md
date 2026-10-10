# filmcraft-speech

Speech-to-text for FilmCraft's Text panel ▸ Transcript (L2). User-facing behaviour, commands and
model handling: [docs/transcripts.md](../../docs/transcripts.md).

- `Transcriber` trait (mono 16 kHz in, word-timed `filmcraft_project::Transcript` out) and
  `FixedTranscriber` for tests and agents.
- `models`: Whisper model catalogue (pinned revisions, SHA-256, sizes, licences) and, with feature
  `download`, the verified downloader (ureq + rustls + RustCrypto). Weights are never bundled.
- `whisper` (feature `whisper`): Whisper inference on candle (CPU, pure Rust).
- `parakeet` (feature `parakeet`): NVIDIA Parakeet TDT 0.6B (v3 multilingual, v2 English) on
  candle (CPU, pure Rust): NeMo log-mel front end, FastConformer encoder, greedy
  token-and-duration transducer decoding, word times from token frames and durations, long audio
  cut at pauses.
- `nemo` (always built): hostile-input-safe readers for `.nemo` archives (tar, stored zip, the
  `torch.save` pickle subset, the `model_config.yaml` subset, SentencePiece `tokenizer.model`).
- `mel`, `vad`, `diarize`: log-mel front end, word-bound tightening, MFCC-clustering speaker labels.

The model features are off by default and are never enabled for wasm (`cargo xtask wasm` checks
the crate without them).

## References

Implemented from the published description of the model: A. Radford et al., "Robust Speech
Recognition via Large-Scale Weak Supervision" (OpenAI, 2022), the model cards and configuration
files published with the weights (`config.json`, `generation_config.json`, `tokenizer.json`), and
the candle tensor library's public API. Weights: OpenAI Whisper, MIT licence.

### Parakeet TDT

Implemented from the published descriptions: D. Rekesh et al., "Fast Conformer with Linearly
Scalable Attention for Efficient Speech Recognition" (2023); A. Gulati et al., "Conformer:
Convolution-augmented Transformer for Speech Recognition" (2020); Z. Dai et al.,
"Transformer-XL" (2019, relative positional attention); H. Xu et al., "Efficient Sequence
Transduction by Jointly Predicting Tokens and Durations" (2023, TDT); the model cards of
`nvidia/parakeet-tdt-0.6b-v3` and `-v2` and the `model_config.yaml` inside each `.nemo` archive.
NVIDIA NeMo (Apache-2.0) was read for module order, state-dict names, the preprocessor's
normalisation and the greedy TDT loop; no code was copied. Formats: POSIX ustar/PAX (IEEE Std
1003.1), PKWARE APPNOTE (zip, zip64), Python `pickletools` opcode documentation, PyTorch's
`torch.save` zip layout, SentencePiece's `sentencepiece_model.proto`, protobuf wire format.

NeMo was used only as an external test oracle (a local Python venv, never shipped): the
front end matches NeMo's preprocessor to 2·10⁻⁵, the encoder output to 10⁻⁶, and greedy tokens and
frames match on the test sets (see `docs/transcripts.md`). Weights: NVIDIA, CC-BY-4.0, downloaded
unmodified, never bundled.
