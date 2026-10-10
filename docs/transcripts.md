# Transcripts and text-based editing

FilmCraft's Text panel ▸ **Transcript** tab shows the dialogue of the open sequence as text. Select
words to mark In/Out, then extract or lift them; remove filler words and long pauses in one step;
turn the transcript into captions. Every action is an engine command (`transcript.*`), so the CLI,
the control channel and MCP agents can do the same.

## Model

- A **transcript** belongs to a media item (`Project::transcripts`, saved in the `.fcproj` since
  schema v9). It lists **words** with media-time bounds (`Tick`s), an optional speaker index and a
  confidence, plus the speaker names and the language. Because the times are media time, the
  transcript stays valid however the clip is trimmed, moved, sped up or reused.
- The **sequence transcript** is derived, never stored (`filmcraft_edit::transcript::sequence_words`):
  audio tracks are read top first; a word is heard through the first enabled clip whose range
  covers the word's midpoint (duplicates of the same dialogue on lower tracks read once); disabled,
  reversed and frame-hold clips contribute nothing.
- Speaker names come from the clip transcripts, so renaming "Speaker 1" in every transcript renames
  it across the sequence.

## Commands

| Command | What it does |
|---|---|
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default `whisper-base`; also `whisper-tiny`/`-small`, `parakeet-tdt-0.6b-v3`, `parakeet-tdt-0.6b-v2`), `language` (`auto` = detect), `diarize`, `maxSpeakers`. One undo step. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip), paragraphs, speakers, the word at the playhead. |
| `transcript.search` | Word-index ranges matching a phrase (case and punctuation ignored; the last word may be a prefix). |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default um/uh/erm/…; phrases such as "you know" allowed). |
| `transcript.removePauses` | Ripple-delete pauses longer than `minSeconds`, keeping `keepSeconds` of air on both sides. |
| `transcript.createCaptions` | Lay the words out as captions on a new caption track (`maxChars`, `lines`, `minSeconds`, `maxSeconds`, `gapFrames`). |
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, licence URL, attribution line, installed) / download one. |

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). Two recognisers are built in,
both run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU:

- OpenAI's **Whisper**, with timestamp decoding, language detection and word times from
  cross-attention alignment (see the `filmcraft_speech::whisper` module docs);
- NVIDIA's **Parakeet TDT 0.6B** (FastConformer encoder, token-and-duration transducer), read
  straight from its `.nemo` archive, with word times from the model's own token frames and
  durations (see [Parakeet TDT](#parakeet-tdt) and the `filmcraft_speech::parakeet` module docs).

`transcript.generate` picks the recogniser from the catalogue id in `model`. Speakers are labelled
by clustering per-chunk MFCC statistics (`filmcraft_speech::diarize`); no model is involved.

Both are **optional features**, off by default and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference. On the engine and the apps it
  includes `parakeet`.
- `parakeet` (on `filmcraft-speech`, `filmcraft-engine` and the apps): Parakeet TDT only.
- `download` (`speech-download` on the engine): HTTPS downloads with rustls + RustCrypto and the
  operating system's certificate verifier.

Without `whisper`, and with no recogniser installed, `transcript.generate` and Transcribe Sequence
are disabled, with "speech-to-text is not available in this build" as the reason (`describe`,
`command_list {"enabled_only": true}` and the menus show it); with Automatically transcribe clips
on, `file.import` reports the same reason as a `transcription: …` entry in its `errors`.
Without `speech-download`, `transcript.downloadModel` is disabled the same way. Transcripts can
still be imported with `transcript.set` and edited with every other command. Hosts and tests can
install any recogniser in `Session::transcriber`, which enables transcription in any build.

### Models

Weights are **never** bundled or committed. They are downloaded on request into
`<data dir>/models/<id>/` (see `filmcraft_engine::autosave::default_data_dir`), each file pinned to a
revision of the publisher's Hugging Face repository (OpenAI, NVIDIA) and checked against its SHA-256:

| Id | Languages | Licence |
|---|---|---|
| `whisper-tiny` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-base` (default) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `parakeet-tdt-0.6b-v3` (2.51 GB) | 25 European languages: bg, cs, da, de, el, en, es, et, fi, fr, hr, hu, it, lt, lv, mt, nl, pl, pt, ro, ru, sk, sl, sv, uk | CC-BY-4.0 (NVIDIA) |
| `parakeet-tdt-0.6b-v2` (2.47 GB) | English | CC-BY-4.0 (NVIDIA) |

CC-BY-4.0 requires attribution: the weights are downloaded unmodified and `transcript.models`
returns each model's credit line (`attribution`: name, author, licence with its URL, source), which
the download dialog shows.

### Parakeet TDT

`parakeet-tdt-0.6b-v3` (and the English-only `-v2`) runs NVIDIA's 600 M-parameter
FastConformer-TDT model in pure Rust, read straight from the `.nemo` archive (a tar with the YAML
configuration, a PyTorch checkpoint and the SentencePiece tokenizer; `filmcraft_speech::nemo` reads
all three without Python and treats them as hostile input). How it works:

- **Front end**: NeMo's 128-band log-mel spectrogram (pre-emphasis 0.97, 25 ms Hann window in a
  512-point FFT, 10 ms hop, Slaney mel, per-band normalisation, no dither).
- **Encoder**: 8× depthwise-striding subsampling to 80 ms frames, 24 conformer blocks (macaron
  feed-forward, relative-position self-attention with full context, convolution module).
- **Decoder**: token-and-duration transducer, greedy: an LSTM prediction network and a joint
  network that predicts the next token and how many 80 ms frames it covers.
- **Word times** come from the model itself: a word starts at its first token's frame and ends at
  its last token's frame plus that token's duration, then the bounds are tightened past silence
  like Whisper's. No language is given to the model; v3 handles the 25 languages in one model and
  the transcript's language is guessed from frequent words (or is the one requested).
- **Long audio** (more than 60 s) is cut in pauses into pieces of at most 60 s, each encoded on its
  own, so memory stays flat for clips of hours and no word is split or repeated at a join (see the
  `filmcraft_speech::parakeet` module docs). Runs of digital silence (below −80 dBFS) longer than
  300 ms are shortened before encoding, with times mapped back exactly.

Measured on 2026-10-10 on the development PC (i9-12900KF, 16 cores, CPU only, release build; other
jobs were running, so speeds are conservative), against NVIDIA's NeMo toolkit as the reference and
the Whisper models of this build:

| | Parakeet v3 | Parakeet v2 | Whisper base | Whisper small |
|---|---|---|---|---|
| English WER, LibriSpeech test-clean (100 utterances, 2221 words) | 1.98 % | 1.49 % | 4.86 % | 3.51 % |
| German WER, FLEURS (60 utterances, 1381 words) | 5.03 % | – | 19.08 % | 9.16 % |
| German WER, VoxPopuli (80 spontaneous utterances, references without fillers) | 11.75 % | – | – | – |
| Meeting speech WER, AMI headsets (80 segments, 1133 words) | 15.00 % | 14.65 % | 31.86 % | 28.95 % |
| um/uh kept (AMI, 91 fillers) | 75 % | 68 % | 8 % | 9 % |
| Word start / end error against forced alignment, mean (90th percentile) | 71 / 74 ms (140 / 150) | 61 / 63 ms (120 / 120) | 80 / 80 ms (170 / 170) | 74 / 75 ms (170 / 170) |
| Long-form WER: 14.5 min of continuous reading; the same with three 2 s digital-silence gaps | 1.7 %; 1.7 % | – | – | – |
| Real-time factor (CPU time ÷ audio time; shared CPU) | 0.06–0.09 | 0.09 | 0.2–0.4 | 0.6–1.1 |
| Download | 2.51 GB | 2.47 GB | 293 MB | 969 MB |

- Parity with NeMo: identical tokens and token frames on every test utterance (100 English, 60
  German); front end within 2·10⁻⁵, encoder output within 10⁻⁶ of NeMo's.
- Word times are compared with forced alignments (Montreal Forced Aligner) of the LibriSpeech
  utterances; "within 50 ms" is the share of word starts closer than 50 ms.
- Fillers: AMI meeting segments whose reference contains um/uh/hmm; recall is the share of those
  fillers that appear in the transcript. German äh/ähm are in v3's vocabulary, but none appeared in
  11 minutes of VoxPopuli parliament speech, whose references leave fillers out, so German filler
  coverage is unmeasured.
- Speed: the 39 minutes of long-form audio took 148 s with v3 (real-time factor 0.063; loading the
  model takes about 1.3 s). Other benchmark jobs shared the CPU during every run, so the factors
  are upper bounds; the Whisper rows are this build's Whisper before its speed work.

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. `crates/speech/tests/parakeet_model.rs` (feature `parakeet`) does the same for
Parakeet (`$FILMCRAFT_SPEECH_SAMPLES` may point at another sample directory): WER on the samples,
long-form joins without lost words, cancelling, the checkpoint's filterbank against the computed one,
and v2's English-only check. The `.nemo` readers have mutation-fuzz tests (truncation, bit flips,
corrupt sizes) in the default test suite. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
the local speech samples (LibriSpeech read speech and dialogue clips) (about 9.5 minutes for the run in a release build on an
Apple-silicon laptop CPU).

## Limits

- Transcription runs synchronously inside the command (no background job or progress bar yet).
- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
- Parakeet runs on the CPU only, needs a 2.5 GB download and about 2.6 GB of memory while loaded.
  It does not report a language: v3's transcript language is the requested one, else a guess from
  frequent words (English when the text is too short to tell).
- Audio made of short utterances separated by stretches of exact digital silence (hard edits,
  noise gates) is the hardest case for Parakeet: dead air is shortened and the decoder restarts
  after long silences, but an occasional sentence can still go missing there (4.8 % WER on
  such a test file, against 1.7 % on continuous read speech).
