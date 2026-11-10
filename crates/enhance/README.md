# filmcraft-enhance

Neural speech enhancement for FilmCraft: the **Enhance Speech (Neural)** audio effect (L2). The
project effect is `neural_enhance` (`crates/project/src/effect/audio.rs`), the renderer builds it
in `crates/render/src/audio_fx.rs`, and the model commands (`enhance.*`) are in
`crates/engine/src/enhance.rs`. User-facing behaviour and the model comparison:
[`docs/neural-enhance.md`](../../docs/neural-enhance.md).

- `catalog`: the downloadable models (id, name, licence, source, files pinned to a commit with
  size and SHA-256) and the store under `<data dir>/models/<id>/`. Checksums are verified once per
  file state and cached, because the audio thread asks whether a model is installed whenever it
  builds an effect chain. **The catalogue order is persistent** (the effect's `model` choice
  stores an index): append, never reorder.
- `NeuralEnhance`: the `AudioEffect`. Parameters `model`, `attenuation` (noise reduction limit,
  dB), `post_filter`, `mix`. Without a usable model it is a zero-latency pass-through and says why
  (`bypass()`).
- `stream` (private): turns blocks of any size at the sequence rate into the model's fixed frames
  at the model's rate and back, with a constant reported latency and a dry path delayed to match.
- `resample` (private): streaming windowed-sinc rate conversion, exact rational positions.
- `dfn` (feature `deepfilter`): DeepFilterNet through libDF on tract. libDF's state is not `Send`
  and may panic on an unexpected graph, so each model lives on a worker thread behind
  `catch_unwind`; a failure turns the effect into a (delayed) pass-through. Idle workers are pooled
  because every seek builds a new chain.

Without features the crate depends on `filmcraft-audio-dsp`, `serde`, `thiserror`, `log` and
`sha2`, and checks for wasm.

Weights are never bundled or committed. `tests/deepfilter_model.rs` runs the real models when
they are on the machine and prints SKIPPED otherwise.
