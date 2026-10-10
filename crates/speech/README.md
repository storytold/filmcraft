# filmcraft-speech

Speech-to-text for FilmCraft's Text panel ▸ Transcript (L2). User-facing behaviour, commands,
models and measured speed/accuracy: [docs/transcripts.md](../../docs/transcripts.md).

- `Transcriber` trait (mono 16 kHz in, word-timed `filmcraft_project::Transcript` out) and
  `FixedTranscriber` for tests and agents.
- `models`: speech model catalogue (Parakeet TDT v3/v2; Whisper tiny, base, small, large-v3-turbo, large-v3; pinned revisions,
  SHA-256, sizes, licences) and, with feature `download`, the verified downloader (ureq + rustls +
  RustCrypto). Weights are never bundled.
- `safetensors`, `ct2`: readers of weight files — Hugging Face `model.safetensors` and CTranslate2
  `model.bin` (the "faster-whisper" conversions). Both treat the file as hostile input (capped
  header, every shape/offset/size checked against the file, one tensor read at a time) and are
  mutation-fuzzed in their tests.
- `nn` (feature `whisper`): CPU kernels for transformer inference — matrix products through
  [faer](https://github.com/sarah-quinones/faer-rs) with explicit parallelism, thin products for
  decoding that stream half-precision weights, vectorised row kernels (layer norm, softmax, exact
  GELU), multi-head attention.
- `whisper` (feature `whisper`): Whisper inference (CPU, pure Rust): region-parallel batched
  decoding, timestamp rules, temperature fallback, word timestamps from cross-attention DTW,
  per-window progress and cancellation. Loads a Hugging Face or a CTranslate2 model directory.
- `parakeet` (feature `parakeet`): NVIDIA Parakeet TDT 0.6B (v3 multilingual, v2 English) on
  candle (CPU, pure Rust): NeMo log-mel front end, FastConformer encoder, greedy
  token-and-duration transducer decoding, word times from token frames and durations, long audio
  cut at pauses.
- `nemo` (always built): hostile-input-safe readers for `.nemo` archives (tar, stored zip, the
  `torch.save` pickle subset, the `model_config.yaml` subset, SentencePiece `tokenizer.model`).
- `mel`, `vad`, `diarize`: log-mel front end, word-bound tightening and silence detection,
  MFCC-clustering speaker labels.

The model features are off by default and are never enabled for wasm (`cargo xtask wasm` checks the
crate without them). Whisper runs on faer; Parakeet runs on candle.

## Tuning and diagnostics

- `FILMCRAFT_SPEECH_THREADS`: inference threads (default: two thirds of the logical CPUs; more is
  slower on SMT and hybrid CPUs, and the editor stays responsive).
- `FILMCRAFT_SPEECH_BATCH`: windows decoded together (default: as many as fit 1.5 GB of decoding
  state, at most 16).
- `FILMCRAFT_SPEECH_TRACE`: print a timing summary (windows, steps, mel/encode/decode/align time).
- `FILMCRAFT_SPEECH_SEQUENTIAL`: decode with the classic one-window-after-the-other procedure
  (diagnostics: gives the same transcripts as the earlier candle implementation).

`cargo run --release -p filmcraft-speech --features whisper --example transcribe -- <model dir>
[--lang=en] [--words] file.f32…` transcribes raw mono 16 kHz f32 files and reports WER against
`<file>.txt`; `tests/whisper_model.rs` does the same as a test when weights and samples are present
(`FILMCRAFT_MODELS_DIR`, `FILMCRAFT_SPEECH_MODEL`, `FILMCRAFT_SPEECH_FIXTURES`).

## References

Implemented from the published description of the model: A. Radford et al., "Robust Speech
Recognition via Large-Scale Weak Supervision" (OpenAI, 2022), the model cards and configuration
files published with the weights (`config.json`, `generation_config.json`, `tokenizer.json`), the
safetensors format description (huggingface/safetensors README) and the layout of CTranslate2
model files as found in them. Weights: OpenAI Whisper, MIT licence.

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
