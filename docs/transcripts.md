# Transcripts and text-based editing

FilmCraft's Text panel ▸ **Transcript** tab shows the dialogue of the open sequence as text. Select
words to mark In/Out, then extract or lift them; remove filler words and long pauses in one step;
turn the transcript into captions. Every action is an engine command (`transcript.*`), so the CLI,
the control channel and MCP agents can do the same.

## Premiere parity

The Transcript tab follows Premiere Pro 25.x/26.x's Text panel ▸ Transcript for a **sequence
transcript built from source-clip transcripts**. Behaviour and labels come from Adobe's public help
pages, release notes and Adobe staff answers on community.adobe.com (2023–2026); nothing was taken
from the application. Icons are FilmCraft's own (`crates/ui-egui/src/icons.rs`).

**Toolbar**, left to right: the search field ("Search"; with a filter on it reads "Filler words" or
"Pauses"), the filter button (a funnel; a dot marks a filter other than Text), "{ }" =
*Automatically set In/Out points* (on), Extract and Lift for the text selection, and "•••".

| Where | Premiere | FilmCraft |
|---|---|---|
| Filter menu | "Text" ✓, "Filler words", "Pauses", "Speakers", "Search settings…" | the same without "Speakers" (no speaker labels yet) |
| Results row (searching or filtering) | "Replace", "Delete", "1/33 results" ("no results"), ∧ ∨ | "Delete", the counter, ∧ ∨ (no Replace: transcripts aren't corrected in place yet) |
| Delete row | radios "Extract" (default) / "Lift"; buttons "Delete all", "Delete" | the same; each button is one undo step |
| Matches | all orange, the current one salmon | the same |
| "•••" menu | ACTIONS: "Create captions…", "Transcribe sequence", "Generate static transcript…", "Export ›", "Import ›"; PREFERENCES: "Transcript view options…", "Enable auto-scrolling" ✓, "Spell check ›" | "Create captions", "Transcribe sequence…"; "Transcript view options…", "Enable auto-scrolling" ✓ |
| Transcript view options | "Filler words", "Markers", "Low-confidence words", "Untranscribed sources", "Speakers", "Pauses", "Minimum pause length" (slider + "seconds"); Search settings "Find whole words only", "Match capitalization"; "Cancel" / "Save" | "Filler words" ✓, "Pauses" ✓, "Minimum pause length" 0.1–3.0 s, default **0.75 s**; both search settings (off) |

**Transcribing.** The empty tab says "Transcribe source clips" / "Transcribe your source clips to
view your sequence transcript." with a **Transcribe** button. When some audio clips have no
transcript yet, a banner names them and offers Transcribe again. Transcribe opens the options:

- "Language": "Auto detect", English, German, … (Premiere's language list).
- "Audio analysis": "Audio clips tagged as 'Dialogue'" (Essential Sound type) or "Audio on track"
  with "Mix" (all audio tracks) or one track ("Audio 1", …).
- "Speech model" (FilmCraft: which recogniser of the speech catalogue runs; Premiere has one
  built-in engine). Models not on this computer are marked with their download size.
- "Speaker labeling" is off and hidden for now.
- When the chosen model is missing, a confirmation shows its size, source and licence before
  anything is downloaded ("Download and transcribe"). Premiere instead marks languages whose pack
  is missing with a download icon and has no documented confirmation.

Transcription runs in the background. The Transcript tab shows a progress bar ("Transcribing…",
the percentage) with **Cancel**; the status bar and Window ▸ Progress show the job too. The UI stays
responsive. A finished job adds all its transcripts as **one undo step** ("Transcribe"); a
cancelled one changes nothing.

**The transcript.** Segments (a new one after a pause of 1.5 s) show the timecode range above the
text ("00:00:02:22 - 00:00:05:08"); the speaker column is hidden while speakers are off.

- The word being spoken is highlighted as the playhead moves; with "Enable auto-scrolling" (on) the
  view scrolls to keep it visible.
- A click on a word moves the playhead to it (and clears a text selection and the In/Out it set).
  Dragging over words or Shift+click selects text; with "{ }" on that marks In/Out on the timeline.
  A double-click selects one word.
- Pauses of at least the minimum pause length show inline as a dimmed "[...]"; hovering shows the
  length ("1.2 seconds"); a click selects the pause.
- Filler words are marked (tinted, dimmed) when "Filler words" is on.
- Delete or Backspace with text or a pause selected **extracts** it (ripple delete, gaps close);
  Alt+Backspace **lifts** it (leaves a gap). Ctrl/Cmd+F goes to the search field; ← → ↑ ↓,
  Home / End (with Shift to extend) move the playhead word by word as in Premiere.

**Filler words.** Premiere detects "uh"/"umm" "language agnostic" and publishes no list. FilmCraft
matches per transcript language: English um, uh, umm, uhm, erm, er, ah, hmm, mm, mhm; German äh,
ähm, ähh, ähmm, öh, öhm, ehm, hm, hmm, mm, mhm, um, uh, uhm ("er", "ah", "eh" are German words);
other languages only sounds that are no word in any of them. A recogniser that leaves fillers out
of its text (Whisper often does) gives nothing to find.

**Deliberately different or missing:** speakers (labels, the Speakers filter, renaming in the
panel), Replace, static transcripts, transcript export/import, spell check, "Transcribe In point
to Out point only", the source-clip transcript in the Source monitor, and "Follow active monitor".

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
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default: Settings ▸ Speech model, `parakeet-tdt-0.6b-v3`; also `parakeet-tdt-0.6b-v2`, `whisper-tiny`/`-base`/`-small`/`-large-v3-turbo`/`-large-v3`), `language` (`auto` = detect), `diarize`, `maxSpeakers`, `download` (fetch a missing model first; otherwise a missing model is an error naming its size and licence), `wait` (default `true`: return when done with the per-item report; `false`: start a background job and return `{job, items, running}`). One undo step ("Transcribe") when it finishes; a cancelled or failed job changes nothing. |
| `transcript.status` / `transcript.cancel` | The running transcription (`progress` 0–1, `status`, `etaSeconds`, `items`) or `{"running": false}` / stop it (`jobs.list` and `jobs.cancel` see the same job, label "Transcription"). |
| `sequence.transcribe` | Sequence ▸ Transcribe Sequence…: `transcript.generate` on the audio of `track` — `mix` (every audio track), `dialogue` (clips with the Essential Sound type Dialogue) or one track (`A1`) — with the same `language`, `model`, `download` and `wait`. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip, `filler`), paragraphs, `pauses` (`after`, `start`, `end`, `seconds`; at least `minPauseSeconds`, default Transcript view options ▸ Minimum pause length), speakers, the word (`current`) or pause (`currentPause`) at the playhead. |
| `transcript.search` | Matches of a search: `filter` `text` (a phrase in `query`; punctuation ignored; the last word may be a prefix unless `wholeWords`; case ignored unless `matchCase`; both default to Search settings), `fillers` (word ranges) or `pauses` (`pauseAfter`). Returns `count` and `matches`. |
| `transcript.deleteAll` | Remove every match of a search (same params) in one undo step: Extract (ripple, default) or `lift: true`. Pauses go whole. |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) or the pause `pauseAfter` (frame-snapped inward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' (or the pause's) frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default the list of each transcript's language, see Premiere parity; phrases such as "you know" allowed). |
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

Transcript view options and the panel's toggles are preferences (`prefs.set`): `transcript.fillerWords`,
`transcript.pauses`, `transcript.minPauseLength` (0.1–3.0 s, default 0.75), `transcript.wholeWords`,
`transcript.matchCase`, `transcript.autoScroll`, `transcript.autoInOut`.

### Background jobs and the recogniser

`transcript.generate` checks everything it can before it starts (the items, their audio, the model)
and then does the slow part in a job thread under `catch_unwind`: download (when allowed), loading
the model, decoding the audio in 30-second pieces, recognition. Progress is reported per mille of
the job (decoding takes the first tenth of each item's share, recognition the rest, from the
recogniser's `ProgressFn`). Cancel goes through `Transcriber::transcribe_cancellable`, whose default
stops at the recogniser's next progress report, so a recogniser that reports per window stops
within a window. Any `Transcriber` can be plugged in (`Session::transcriber`, or a catalogue model
that `filmcraft_speech::load` knows); tests use a fake one that waits between progress steps.

## Limits

- Auto-transcription on import (Settings ▸ Media Analysis & Transcription) still waits for the
  transcription inside `file.import`.
- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
- Parakeet runs on the CPU only, needs a 2.5 GB download and about 2.6 GB of memory while loaded.
  It does not report a language: v3's transcript language is the requested one, else a guess from
  frequent words (English when the text is too short to tell).
- Audio made of short utterances separated by stretches of exact digital silence (hard edits,
  noise gates) is the hardest case for Parakeet: dead air is shortened and the decoder restarts
  after long silences, but an occasional sentence can still go missing there (4.8 % WER on
  such a test file, against 1.7 % on continuous read speech).
