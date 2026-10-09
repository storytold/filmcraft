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
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default: Settings ▸ Speech model), `language` (`auto` = detect), `diarize`, `maxSpeakers`, `download` (fetch the model first when it isn't installed), `wait` (default `true`: run here and return the report; `false`: run as a background job with progress, as the Text panel does). One undo step. |
| `transcript.find` | The Text panel's search with its filter: `filter` `text` (with `query`), `fillers` or `pauses` (`minSeconds` overrides the pause length). Returns the results with what each covers and what Delete would remove (`cut`). |
| `transcript.deleteHits` | Delete one result (`hit`, its index in `transcript.find`) or all of them, `mode` `extract` (ripple; the default) or `lift`, on every unlocked track. One undo step. Pause cuts keep the margins in the settings (`keepAfterSeconds`, `keepBeforeSeconds` override them). |
| `transcript.findPauses` | Measure the voice of transcripts that have no voice map (imported, or made before voice analysis), so their pauses come from the waveform (background job with `wait: false`). |
| `transcript.cancel` | Stop the running transcription; nothing changes. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip), paragraphs, speakers, the word at the playhead. |
| `transcript.search` | Word-index ranges matching a phrase (case and punctuation ignored; the last word may be a prefix). |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default um/uh/erm/…; phrases such as "you know" allowed). |
| `transcript.removePauses` | Ripple-delete every pause of at least the pause length (`minSeconds`), keeping the margins from the settings (`keepAfterSeconds`, `keepBeforeSeconds`; `keepSeconds` sets both). |
| `transcript.createCaptions` | Lay the words out as captions on a new caption track (`maxChars`, `lines`, `minSeconds`, `maxSeconds`, `gapFrames`). |
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

## Pauses

Premiere finds pauses from its transcript's word timings; FilmCraft measures them in the waveform,
the way an editor's waveform pass does, so they are as tight as the audio allows:

- When a clip is transcribed, its audio is analysed in 10 ms windows every 5 ms
  (`filmcraft_speech::voice`): the **voice map** (where the recording has speech, against its own
  noise floor) is stored in the transcript (`Transcript::voice`, media time). Whisper's words are
  then snapped to it (attention alignment lets the word after a pause swallow the pause).
- A **pause** is a silence of at least the **pause length** (Settings ▸ Media Analysis &
  Transcription ▸ Pauses; default 150 ms, down to 80 ms) between voiced spans, inside the
  transcribed clips only (`filmcraft_edit::transcript::find_voice_pauses`). Two safety rules: a
  pause never overlaps a recognised word (a quiet syllable or word ending below the gate is
  speech), and an untranscribed clip is never read as silence.
- **Deleting a pause** keeps a margin after the word before it and before the word after it
  (default 30 ms and 35 ms) and is rounded inward to frames (`pause_cut`).
- **Voice with no words** (a stutter or restart the recogniser tidied away) shows as `[speech]`
  in the transcript; it is never a pause.
- A transcript without a voice map (imported, or older) uses its word gaps until
  `transcript.findPauses` measures it.

## Text panel ▸ Transcript

As in Premiere: **Transcribe** opens the options (language, separate speakers; the model and its
one-time download), then a progress bar with **Stop**. The transcript shows paragraphs with
`[...]` at every pause (hover: its length and what Delete removes) and `[speech]` for wordless
voice. The search row has the **filter** (Transcript text / Filler words / Pauses), the result
count ("3 of 20") and ▲ ▼; **Delete** removes the current result and **Delete all** every one,
with **Extract** (close the gaps) or **Lift** (leave them). The "…" menu holds the pause settings,
Re-transcribe and Create captions. Clicking a pause selects it; Backspace extracts it and
Alt+Backspace lifts it. Sequence ▸ Transcribe Sequence… opens the same options.

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). The built-in recogniser is
OpenAI's Whisper, run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU,
with timestamp decoding, language detection and word times from cross-attention alignment (see the
`filmcraft_speech::whisper` module docs). Speakers are labelled by clustering per-chunk MFCC
statistics (`filmcraft_speech::diarize`); no model is involved.

With the `metal` feature (`speech-metal` on the engine) Whisper runs on the Apple GPU in half
precision through candle's Metal backend, falling back to the CPU when no Metal device can be
created: large-v3-turbo transcribes a 3.4-minute screen recording in about 6 s on an M4 Max
(about 88 s on the CPU alone). The macOS packages are built with it (`packaging/macos/package.sh`).

The features are **optional**, off by default and never built for the web:

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
| `whisper-base` (default without `metal`) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-large-v3-turbo` (default with `metal`) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |

The setting is stored as `mediaAnalysis.speechModel` (the older `whisperModel` key, which every
saved preferences file filled with the old default, is ignored).

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
the local speech samples (LibriSpeech read speech and dialogue clips) (about 9.5 minutes for the run in a release build on an
Apple-silicon laptop CPU).

## Limits

- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
