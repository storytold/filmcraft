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
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default `whisper-base`), `language` (`auto` = detect), `diarize`, `maxSpeakers`. One undo step. |
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
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). The built-in recogniser is
OpenAI's Whisper, run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU,
with timestamp decoding, language detection and word times from cross-attention alignment (see the
`filmcraft_speech::whisper` module docs). The alignment makes words contiguous (each word ends where
the next starts) and places a start that follows a pause late, so the word bounds are then measured
against the audio (`filmcraft_speech::vad`): a pause of 0.2 s or more inside a word's span ends
the word and starts the next one after the pause, and silence at the edges is trimmed. That is
what `transcript.removePauses` and the extract / lift cuts see. Speakers are labelled by
clustering per-chunk MFCC statistics (`filmcraft_speech::diarize`); no model is involved.

Both are **optional features**, off by default and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference.
- `download` (`speech-download` on the engine): HTTPS downloads with rustls + RustCrypto and the
  operating system's certificate verifier.

Without `whisper`, and with no recogniser installed, `transcript.generate` and Transcribe Sequence
are disabled, with "speech-to-text is not available in this build" as the reason (`describe`,
`command_list {"enabled_only": true}` and the menus show it); with Automatically transcribe clips
on with the all-imported scope, `file.import` reports the same reason as a `transcription: …`
entry in its `errors`.

The default sequence-only scope waits until enabled audio is placed in a sequence (including
Source Insert/Overwrite, timeline placement, paste and sequence expansion). It transcribes the
underlying media once, preserving any existing transcript, and includes the new transcript in
the placement's undo step. Undo/Redo and opening or recovering a project do not run recognition.
If recognition fails, the clips stay placed and a warning appears in the Events panel and a toast.

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

## Limits

- Transcription runs synchronously inside the command (no background job or progress bar yet).
- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
- Word bounds are only moved across pauses. A start that the alignment places late inside
  continuous speech (no pause of 0.2 s between the two words) stays where it is, so the word
  before it can hold the beginning of the next one: the audio level gives no boundary to move it
  to. Words that the alignment places a whole sentence away are not moved back either.
