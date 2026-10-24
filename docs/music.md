# Music generation

Describe a cue (mood, length, tempo, key), pick a provider, and FilmCraft adds the generated music to
the timeline as a clip typed **Music**. Music generation is a built-in feature (commands, a
background job, a panel, timeline placement); only the **provider** layer is pluggable.

Open it from **Window ▸ Music** (it sits next to Text to Speech in the Audio workspace).

## Commands

Every action is a command, so the panel, the CLI, MCP and the control channel share one path.

| command | does |
|---|---|
| `music.providers` | providers, whether each is ready, the on-device model's size and licences |
| `music.render` | generate in a background job into the cache (progress, Cancel) |
| `music.create` | write `Music <n>.wav` and its provenance sidecar, import, place, mark as Music, optionally generate ducking (one undo step) |
| `music.inspect` | provider, model, prompt, seed and licence of a generated clip |
| `music.acceptLicenses` | record that the user accepted the model licences |
| `music.download` | download the on-device model (after the licences are accepted) |

`music.create` takes `prompt`, `duration` (seconds) or `fill: "inout"` (the sequence In to Out range),
`provider`, `bpm`, `key`, `instrumental`, `negativePrompt`, `seed`, `time`, `track`, `ducking` and
`dir`. A cue goes at `time` (default: the playhead, or the In point for `fill: "inout"`) on `track`
if given (it must be free), else on the first free targeted audio track, else the first free audio
track, else a new audio track. Existing clips are never overwritten.

`music.render` followed by `music.create` with the same parameters reuses the render (the render
fixes a random seed and returns it). `music.create` alone generates in the foreground.

## Providers

All providers implement one interface (`filmcraft_music::Provider`: `info()`, `generate()`).

| id | what | needs |
|---|---|---|
| `stable-audio-local` | Stable Audio 3 small music running inside FilmCraft | the model files (about 2.4 GB) and the ONNX Runtime library |
| `stability-api` | Stability AI's hosted Stable Audio | endpoint and API key in Preferences |
| `audio-api` | any service with an official audio API that returns WAV | endpoint and API key |

Settings are preferences (`music.provider`, `music.device`, `music.api.*`). They are written to
`preferences.json` and never to the project file. `prefs.get` and `ui.inspect` report API keys as
`(set)`. The Stability AI results endpoint (`GET /v2beta/audio/results/{id}`, `202` while running)
is built in; the generation endpoint is configuration because it is not in the public reference we
could read (see "Open questions").

### The on-device provider

`sa3` runs the three Stable Audio 3 graphs (text and time conditioner, diffusion transformer, SAME
decoder) with ONNX Runtime and implements the 8 step ping-pong sampler in Rust
(`crates/music/src/sa3.rs`). ONNX Runtime is **loaded at run time** (`ort`'s `load-dynamic`), from
`<models>/sa3-small-music/runtime/` or `ORT_DYLIB_PATH`; nothing is linked, so the default build is
unchanged. The feature is off by default: `cargo build -p filmcraft --features music-native`.
`music.device=auto` prefers the platform GPU provider (Core ML on macOS, CUDA/DirectML on Windows,
and CUDA elsewhere). ONNX Runtime falls back per operation, and FilmCraft retries model loading on
the CPU if the accelerator cannot initialize it. Set `music.device=cpu` to disable acceleration.

Checked against the reference implementation (`Stability-AI/stable-audio-3`), same prompt and seed:

| check | result |
|---|---|
| DiT, ONNX Runtime vs PyTorch (dynamic length) | max relative difference 3e-6 |
| SAME decoder | log-spectral distance 0.0142, equal to PyTorch against itself (the decoder injects noise by design) |
| 30 s end to end, Python sampler on the ONNX graphs vs PyTorch on CPU | waveform correlation 0.998, log-spectral distance 0.036, equal RMS |
| 30 s end to end, Rust sampler (own seeded noise generator) | finite, RMS 0.12 vs 0.105, long-term spectrum correlation 0.974 |

The Rust noise generator does not reproduce PyTorch's, so a seed gives a repeatable result in
FilmCraft but not the same audio as the Python reference. Differences from the reference: the T5Gemma
text encoder runs in fp32 (the weights ship as bfloat16, which ONNX Runtime's CPU kernels lack); the
text encoder differs by about 1.6 % relative from the bfloat16 reference.

Speed, 30 s stereo at 44.1 kHz, small music model:

| hardware | runtime | wall time |
|---|---|---|
| Apple M4, 16 GB | FilmCraft (Rust + ONNX Runtime), CPU | 13.6 s (model load 40 s from an external SSD) |
| Apple M4 | PyTorch reference, CPU | 10.5 s |
| Apple M4 | PyTorch reference, MPS | 11.9 s first call (sampler 4.3 s) |
| Apple M4 | FilmCraft, Core ML provider | unresolved; the fixed-shape retry also failed (see below) |
| RTX 3090 (Cyber) | FilmCraft, CUDA | see the PR |

### Exporting the graphs

The weights are gated on Hugging Face (accept the licence on
`stabilityai/stable-audio-3-small-music`). `tools/` in the model working folder holds the export
scripts; the recipe is:

1. Load `StableAudioModel.from_pretrained("small-music", device="cpu")`.
2. Replace `F.rms_norm` by an explicit `x * rsqrt(mean(x²) + eps) * weight` (the legacy exporter has no
   op for it).
3. Export the DiT with `cfg_scale=1` (no negative branch), inputs `x`, `t`, `cross`, `glob`, `local`,
   `pmask`, dynamic latent length, opset 17.
4. Export the decoder with `chunked=False` (the chunk layout would be baked in); FilmCraft does the
   overlapped chunking.
5. Export the conditioner with the T5Gemma encoder in fp32 and `attn_implementation="eager"`; the
   tokenizer stays outside the graph (`tokenizer.json`, read by the `tokenizers` crate).

## Licensing

* Stable Audio 3 weights: **Stability AI Community License**. Free for research, non-commercial use
  and commercial use by people and organisations under US$1 million annual revenue; above that an
  enterprise licence is required. Commercial use needs registration at
  <https://stability.ai/community-license>. Distributing the weights or a derivative (an ONNX
  conversion is one) requires a copy of the licence, the notice
  *"This Stability AI Model is licensed under the Stability AI Community License, Copyright ©
  Stability AI Ltd. All Rights Reserved"*, and a visible *"Powered by Stability AI"*. The panel shows
  that line, and `LICENSE.md` and `NOTICE` travel with the model files.
* The text encoder is Google's T5Gemma, redistributed under the **Gemma Terms of Use**
  (<https://ai.google.dev/gemma/terms>), including their use restrictions.
* Weights are never bundled or committed. The user accepts both licences in the panel before
  anything is downloaded; the acceptance is recorded next to the model.
* ONNX Runtime is MIT licensed. `ort` is MIT OR Apache-2.0. `tokenizers` is Apache-2.0.
* Generated files carry `<file>.music-provenance.json` (provider, model, prompt, seed, licence,
  date) so licensing can be audited.

## Decisions and open questions

1. **ONNX Runtime** is C++, not pure Rust. Accepted: it stays optional (`music-native`, loaded at run time, never linked).
2. **Hosting the converted ONNX files.** For now they are served from the maintainer's host (a tailscale serve endpoint) and the manifest pins each URL and SHA-256. Public redistribution needs the licence notices above and Stability registration.
3. **Stability AI generation endpoint.** The results endpoint is documented; the generation path and fields were not in the reference pages we could read, so it is a setting.
4. **GPU on Apple Silicon.** ONNX Runtime's Core ML provider did not finish compiling the dynamic-shape fp32 DiT. A fixed-shape retry failed because AppleDouble `._weight.bin` metadata files on the external SSD collided with ONNX Runtime's cache copy even with `COPYFILE_DISABLE=1`. A cache on another disk or a patched cache path still needs testing; the working macOS default is CPU.
5. **CUDA timing** is not measured yet (the GPU on the test host was busy).
