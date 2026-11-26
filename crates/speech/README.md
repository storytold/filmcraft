# filmcraft-speech

Speech-to-text for FilmCraft's Text panel ▸ Transcript (L2). User-facing behaviour, commands and
model handling: [docs/transcripts.md](../../docs/transcripts.md).

- `Transcriber` trait (mono 16 kHz in, word-timed `filmcraft_project::Transcript` out) and
  `FixedTranscriber` for tests and agents.
- `models`: Whisper model catalogue (pinned revisions, SHA-256, sizes, licences) and, with feature
  `download`, the verified downloader (ureq + rustls + RustCrypto). Weights are never bundled.
- `whisper` (feature `whisper`): Whisper inference on candle (CPU, pure Rust).
- `mel`, `vad`, `diarize`: log-mel front end, word-bound tightening, MFCC-clustering speaker labels.
- `stream` (no feature needed): live recognition while audio is captured, for the prompter's Follow
  Speech (#5266). `StreamingRecognizer` takes mono 16 kHz audio in blocks and returns committed and
  tentative words; `RollingRecognizer` runs any `Transcriber` over a rolling window on a worker
  thread; `ScriptedRecognizer` emits given words at given audio times for tests and agents.
  `resample` converts captured audio to 16 kHz as it arrives (windowed sinc, bounded memory).

## Live recognition (`stream`)

**Decisions.**

- *Window and hop.* A pass is due every 1.5 s of new audio and transcribes the audio from 1 s
  before the last committed word to now, at most 10 s. Passes run one at a time on the worker; a
  pass that outlasts the hop makes the passes due meanwhile be skipped (counted), never queued, so
  the work never piles up. The buffer keeps at most 30 s of audio for any take length.
- *Agreement.* Words ending more than 0.25 s before the last committed word, or repeating the last
  committed words, are dropped from a hypothesis; of the rest, the longest prefix equal (case and
  punctuation aside) to the previous pass's uncommitted words is committed. Committed words are never
  retracted and never move back in time; the rest are tentative. A pass is limited to 400 words of
  64 characters.
- *Silence and noise.* A 10 ms frame is voiced above −50 dBFS and 10 dB over the window's 10th
  percentile level. No pass runs without 150 ms of voiced frames in the window, or without voiced
  new audio unless tentative words wait (that pass over a finished phrase is what commits its end);
  a silent window drops the tentative words. A word with fewer than 30 ms of voiced frames within
  100 ms of it is dropped. Steady noise sets its own floor, so it is not speech.
- *Stopping* sets a flag the pass's progress callback checks; `stop` returns at once and the
  worker ends after the model call it is in (Whisper checks between 30-second windows, so within one
  pass).

**Tests** (`cargo test -p filmcraft-speech stream:: resample::`, no model): agreement over scripted
hypotheses (commit on agreement, never retract or move back, disagreeing passes commit nothing,
hostile hypotheses bounded and cleaned, a window starting inside the committed words), voiced
frames for silence, steady noise and bursts, and the worker with a fake transcriber (commits,
silence never reaches the model, a slow model skips passes and stops promptly, a model that fails
to load says why, the buffer stays bounded). Resampling keeps pitch and level, removes tones above
8 kHz, is block-size independent and survives hostile rates and samples.

**Measured** with `examples/stream.rs` (feature `whisper`), which pushes a raw mono 16 kHz file at
real-time pacing and reports the lag from each word's end to its commit: a 30-second English clip
of one speaker (about 75 words with a pause of several seconds), on a laptop CPU **under heavy
load from other builds (load average 35 to 90)**, committed text compared with Whisper base's
offline transcript of the same clip:

| model | window, hop | words | WER vs offline base | lag median / p90 / max | passes run / skipped | longest pass |
|---|---|---|---|---|---|---|
| tiny | 6 s, 1.5 s | 54 | 31.5% | 2.8 / 3.6 / 5.0 s | 20 / 0 | 0.9 s |
| tiny | 10 s, 1.5 s (default) | 73 | 12.3% | 2.6 / 3.6 / 6.5 s | 20 / 0 | 0.8 s |
| tiny | 10 s, 1 s | 50 | 35.6% | 2.4 / 5.8 / 6.9 s | 24 / 9 | 2.6 s |
| base | 10 s, 1.5 s | 64 | 13.7% | 5.7 / 9.9 / 11.8 s | 12 / 10 | 6.2 s |
| tiny, load average 60 to 110 | 10 s, 1.5 s (default) | 34 | 54.8% | 6.8 / 9.3 / 11.5 s | 10 / 12 | 4.1 s |

The rows were run one after another while other builds loaded the machine (load average about 35
for the first four, 60 to 110 for the last); the last row shows what happens when passes take
longer than the hop: passes are skipped, words arrive late and phrases that never get two agreeing
passes are lost. A run on an idle machine is still to be done. Ten seconds of digital silence and
ten seconds of white noise at about −30 dBFS committed no words and ran no pass (all 8 due passes
gated as silent).

With a 6-second bound, a phrase whose passes disagreed was pushed out of the window before it was
committed (whole phrases lost); 10 seconds fixed that. A 1-second hop made passes overrun and skip.
No words were committed during the pause in the clip. Whisper tiny kept up at the lower load; base
did not (its passes take longer than the hop), so its words arrive several seconds
late. The process used about 1.7 cores with tiny and 2.6 with base (candle uses several threads per
pass).

**Limitations.** The words of the last pass before the audio stops are committed only if a pause
follows them (a narrator who stops the take mid-sentence loses its last words). Whisper may still
invent words over voiced non-speech (music, coughs); agreement removes those that change from pass
to pass, not those it repeats. The lag is two passes plus the pass time: about 2 to 4 s with tiny.

Both features are off by default and are never enabled for wasm (`cargo xtask wasm` checks the
crate without them).

## References

Implemented from the published description of the model: A. Radford et al., "Robust Speech
Recognition via Large-Scale Weak Supervision" (OpenAI, 2022), the model cards and configuration
files published with the weights (`config.json`, `generation_config.json`, `tokenizer.json`), and
the candle tensor library's public API. Weights: OpenAI Whisper, MIT licence.
