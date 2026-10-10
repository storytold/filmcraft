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
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default: Settings ▸ Speech model), `language` (`auto` = detect), `diarize`, `maxSpeakers`, `download` (fetch a missing model first; otherwise a missing model is an error naming its size and licence), `wait` (default `true`: return when done with the per-item report; `false`: start a background job and return `{job, items, running}`). One undo step ("Transcribe") when it finishes; a cancelled or failed job changes nothing. |
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
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). The built-in recogniser is
OpenAI's Whisper, run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU,
with timestamp decoding, language detection and word times from cross-attention alignment (see the
`filmcraft_speech::whisper` module docs). Speakers are labelled by clustering per-chunk MFCC
statistics (`filmcraft_speech::diarize`); no model is involved.

Both are **optional features**, off by default and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference.
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
revision of OpenAI's Hugging Face repositories and checked against its SHA-256:

| Id | Languages | Licence |
|---|---|---|
| `whisper-tiny` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-base` (default) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
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
