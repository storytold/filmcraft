# Enhance Speech (Neural)

An audio effect that removes background noise from speech with a neural network: street and
traffic noise, wind rumble, air conditioning, crowd murmur, camera handling noise. It is in the
Effects panel under **Audio Effects ▸ Noise Reduction/Restoration**, next to the DSP effects
*DeNoise* and *Enhance Speech*, and works on clips and as a mixer insert like any other audio
effect (id `neural_enhance`).

The network is [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) (Schröter et al.), which
works on full-band 48 kHz audio and runs many times faster than real time on one CPU core.
`filmcraft-enhance` holds the effect; inference is libDF on [tract](https://github.com/sonos/tract),
pure Rust, no GPU.

## Controls

| Control | Range | Default | |
|---|---|---|---|
| **Model** | catalogue | DeepFilterNet 3 | Which network runs. A model is downloaded once (below). |
| **Noise Reduction** | 0–100 dB | 24 dB | The most the noise may be turned down. 100 dB removes all the model can. A moderate limit (12–24 dB) keeps some room tone and keeps quiet, distant voices that full strength tends to remove with the noise. 0 dB leaves the sound as it is. Keyframable. |
| **Post Filter** | on/off | off | A little extra attenuation in the noisiest parts of the spectrum. Can sound over-processed on quiet speech. |
| **Mix** | 0–100 % | 100 % | Blend of processed and original sound. The original is delayed by the effect's latency, so the two stay in phase. Keyframable. |

The effect reports its latency (40 ms for DeepFilterNet 3 at 48 kHz) and the mixer compensates
for it, so picture and other tracks stay in sync. Sequences at other sample rates are converted
to the model's rate and back with a windowed-sinc filter; that adds a few milliseconds of
(compensated) latency.

## Models

Models are **not bundled**. In Effect Controls the effect shows a row under its header: while the
chosen model is missing it offers the download, asks first (size, source, licence), shows progress
and can be cancelled. Files go to `<data dir>/models/<id>/`, next to the speech-to-text and voice
models, each pinned to a commit of its upstream repository and checked against a SHA-256 before it
is used.

| Model | Download | Latency | Licence |
|---|---|---|---|
| `deepfilternet3` (default) | 8 MB | 40 ms | MIT OR Apache-2.0 |
| `deepfilternet3-ll` (low latency) | 36 MB | 20 ms | MIT OR Apache-2.0 |

While the model is missing, in a build without the feature (below), or if the model fails, the
effect **passes audio through unchanged**: a project that uses it opens, plays and exports
everywhere, and never fails because of it.

Commands (also over MCP and the control channel):

| Command | |
|---|---|
| `enhance.models` | The catalogue: name, description, size, licence, source, installed, and whether this build can run and download models. |
| `enhance.downloadModel` `{"model":id?,"wait":bool?}` | Download a model as a background job (progress and cancel through the job). |
| `enhance.removeModel` `{"model":id}` | Delete a downloaded model. |

## Build features

Off by default, like `whisper` and `neural-voices`:

- `neural-enhance` on `filmcraft` / `filmcraft-cli` (→ `filmcraft-engine/neural-enhance` →
  `filmcraft-render/neural-enhance` → `filmcraft-enhance/deepfilter`) builds the inference runtime
  and enables model downloads (`speech-download`).

```sh
cargo run --release -p filmcraft --features neural-enhance
```

Without it the effect, its parameters and the commands are all present (projects stay portable),
`enhance.models` reports `"available": false`, and audio passes through.

libDF is a git dependency pinned to the DeepFilterNet 0.5.6 release commit, because the crate on
crates.io (0.2.5) predates the inference runtime.

## Tests

- `filmcraft-enhance` unit tests: the catalogue (pinned URLs, checksums, stable order), the model
  store (same-length corruption is not "installed"), the resampler (level, frequency, delay,
  anti-aliasing, block-size independence), and the streaming wrapper with a stand-in model
  (sample-exact latency at 48 kHz, wet/dry alignment at other rates, mix, a failing model, reset,
  hostile shapes).
- `crates/enhance/tests/deepfilter_model.rs` (feature `deepfilter`) runs the real models when the
  weights are on the machine (`$FILMCRAFT_MODELS_DIR` or `target/models/<id>/`); otherwise it
  prints SKIPPED. Measured 2026-11-09 on steady broadband noise at −26 dBFS, stereo: DeepFilterNet 3
  removes more than 20 dB unlimited and 9–13.5 dB at a 12 dB limit, at 48 kHz and at 44.1 kHz.
- Speed, release build, one core of a desktop CPU: DeepFilterNet 3 processes 5 s of stereo in
  0.21 s (24× real time); loading a model takes about 0.2 s, and idle models are reused.

## Why DeepFilterNet, and what else was looked at

Requirements: runs locally on a CPU in (better than) real time, works at 48 kHz, causal (it must
stream through the mixer), weights and code under a licence on the allowlist, pure-Rust inference.

| Model | Rate | Size | Quality (published) | Real time on CPU | Licence | Verdict |
|---|---|---|---|---|---|---|
| **DeepFilterNet 3** | 48 kHz | 2.1 M params, 8 MB | PESQ 3.17, STOI 0.944 (VoiceBank+DEMAND) | yes, RTF 0.19 on a laptop core (paper); 0.04 measured here | MIT OR Apache-2.0 | **default** |
| DeepFilterNet 3 low-latency | 48 kHz | 36 MB | a little below DeepFilterNet 3 | yes | MIT OR Apache-2.0 | in the catalogue |
| DeepFilterNet 2 | 48 kHz | 2.3 M params, 9 MB | PESQ 3.08 | yes | MIT OR Apache-2.0 | its published ONNX export does not run on tract 0.19 (an unimplemented reshape); left out |
| DPDFNet (Ceva, 2025) | 8/16/48 kHz | several sizes | reported above DeepFilterNet 2 on long low-SNR recordings | yes (causal, ONNX) | Apache-2.0 | the most promising addition; needs its own front end |
| GTCRN | 16 kHz | 48 K params | PESQ 2.87; DNSMOS 3.44 (DNS3) | yes, tiny | MIT | wide-band only: would discard everything above 8 kHz |
| RNNoise | 48 kHz | < 1 MB | about PESQ 2.3 | yes, tiny | BSD-3-Clause | clearly lower quality; leaves musical noise |
| DTLN | 16 kHz | about 1 M params | below DeepFilterNet | yes | MIT | wide-band only |
| FRCRN | 16 kHz | large | PESQ 3.21; 2nd in DNS 2022 | no | Apache-2.0 | too slow for the mixer, wide-band only |
| MP-SENet | 16 kHz | about 2 M params | PESQ 3.60 (best of this list) | no (non-causal transformer) | MIT | offline only, wide-band only |
| Resemble Enhance | 44.1 kHz | hundreds of MB | generative: restores, but can invent detail | no (GPU) | MIT | not a streaming effect; unsuitable for dialogue fidelity |
| NVIDIA Broadcast / Maxine, Krisp, Adobe Enhance Speech | | | | | proprietary | not usable |

Published figures come from the models' papers and repositories and are not directly comparable
between rows measured on different test sets; VoiceBank+DEMAND is small and mostly synthetic. The
catalogue is a list: another model is an entry plus, when it is not a DeepFilterNet archive, a
`FrameModel` implementation for its front end.
