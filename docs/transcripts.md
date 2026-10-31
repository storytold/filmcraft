# Transcripts and text-based editing

FilmCraft's Text panel ▸ **Transcript** tab switches between **Sequence** and **Source**. Sequence shows the dialogue of the open sequence as text. Select
words to mark In/Out, then extract or lift them; remove filler words and long pauses in one step;
turn the transcript into captions. Every action is an engine command (`transcript.*`), so the CLI,
the control channel and MCP agents can do the same.

In the Project panel, right-click an audio clip or a movie with audio and choose **Transcribe
Audio…**. The same action is available on empty space for the selected clips, in List, Icon and
Freeform views. Choose the speech model, language and speaker labeling, then confirm. Parakeet
detects its supported languages automatically. Missing models are not bundled: a separate prompt
shows the download size and license and asks before downloading.

Existing transcripts show the model that produced them. **Transcribe Again** replaces them only
after recognition succeeds; cancelling the options or download leaves them intact. Selection
changes do not retarget the open request, and opening another project cancels it. Existing caption
tracks are independent and stay unchanged. The desktop control channel can open these options with
`transcript.generate {"options":true,"items":[id]}`; `transcribeAudio.*` identifies its controls.

The icon beside the speech model offers a confirmed model-only download when files are missing,
or removes the downloaded cache only after confirmation. Downloading here does not transcribe.
Transcripts, captions and project history are unchanged; using that model again offers the download
prompt. Deletion is unavailable during speech work, and cooperative model leases prevent another
session from deleting or overwriting files that are being loaded or used. For a shared-model cache
link, only the link is removed; shared weights stay intact. Parakeet's language field displays
**Auto**; choose Whisper when a manually selected recognition language is needed.

## Source editing, speakers and project search

Right-click a transcribed Project clip and choose **View Transcript**, or open it in Source and
select **Source** in the Text panel. This works without a sequence, including unused media and
subclips. Source words use media time; clicking a word seeks Source without changing sequence
marks. Sequence-only extraction, filler removal and caption creation are disabled in Source view.

Double-click a word to correct its text without moving its timing, confidence or speaker. Each
correction is undoable. A correction for a word changed by another action is rejected and keeps
the draft visible; switching sources or projects closes the draft. Text fields use normal
copy/paste shortcuts. Corrections do not rewrite existing captions.

**Speakers** opens a source-grouped list of identified speakers. Edit several names, then **Apply**
commits all changed names as one Undo step. **Cancel** discards the draft. The same speaker number
in two source clips remains two independent identities. **Preview** auditions a short passage in
Source without changing timeline or source marks; closing the manager stops its preview. Changes
to the target transcript make its draft stale; switching source/sequence or project closes it.

Click a passage's speaker heading to assign or clear its speaker, create a name, or rename the
speaker in that source transcript. **Identify Speakers…** inside the manager opens separate
settings and warns before replacing names/assignments. Apply pending name edits first. It analyzes the existing words
against the audio, without loading a speech model or transcribing again. It replaces names and
assignments as one undoable operation, with progress and Cancel. This lightweight MFCC clustering
is approximate, not voice recognition; inspect and correct its assignments. Explicit analysis
limits clustering to 256 speech turns and reports an error for larger analyses without changing
labels. Cancelled work and work with changed target transcripts are discarded atomically.

The Text panel's **Search** tab searches transcribed source media throughout the project, including
unused clips. Clicking a result opens Source at the matching words, without editing sequence
marks, captions or Undo history. Search is literal and case/punctuation insensitive, runs on a
cancellable project snapshot, and rejects stale results. Queries are limited to 1024 bytes and
32 words; the UI shows up to 100 results, with a visible limit notice. The command can request up
to 500 results; a bounded scan stops after one million words or 100,000 items.

## Caption-card navigation and preview

In Text ▸ **Captions**, click a card's vacant background or its number to select it and seek to
its start without starting playback. Command/Shift-click adds to the caption selection. Text and
In/Out fields keep their normal editing behavior; their clicks do not trigger seeking or playback.
The small Play button auditions the caption's sequence time range and becomes Stop while active.
Playback stops at the exclusive end and rests on the last caption frame. Switching previews,
normal playback, caption deletion/retiming, and project/sequence changes release the previous
preview safely. Existing loop settings are restored. Caption auditioning does not change marks,
caption/transcript data or Undo history, and does not start mixer or multicamera recording.
Navigation/preview is refused during an active recording. UI commands `textPanel.goToCaption` and
`textPanel.previewCaption` take an integer `caption` id, so control clients can do the same.

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
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default: Settings ▸ Speech model, `whisper-base` out of the box; `parakeet-tdt-0.6b-v3` for word-level timing), `language` (`auto` = detect), `diarize`, `maxSpeakers`. One undo step. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip), paragraphs, speakers, the word at the playhead. |
| `transcript.source` | Source words in media time with original word indices, resolving subclips to parent transcripts. |
| `transcript.correctWord` | Correct one `item` / `index` / `text`; optional `expectedWord` rejects concurrent word changes. Timing is retained. |
| `transcript.identifySpeakers` | Analyze speakers of existing transcripts (`items` or `item`, `maxSpeakers`, `wait`). No speech model required. |
| `transcript.assignSpeaker` | Assign `indices` in one `item` to a `speaker`, a new `name`, or null. Optional `expected` word guards. |
| `transcript.search` | Sequence word-index ranges matching a phrase; `scope: "project"` searches all transcribed sources. |
| `transcript.openSearchResult` | Open and seek Source using `item`, `from`, `to`; optional `expected` hit guards. No project edit. |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.renameSpeakers` | Batch rename source-local identities as one Undo step: `sources: [{item, expected?: Transcript, names: [{speaker, name}]}]`. All groups validate before mutation; full `expected` guards reject stale transcripts. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default um/uh/erm/…; phrases such as "you know" allowed). |
| `transcript.removePauses` | Ripple-delete pauses longer than `minSeconds`, keeping `keepSeconds` of air on both sides. |
| `transcript.createCaptions` | Lay the words out as captions on a new caption track (`maxChars`, `lines`, `wordsPerCaption` (1 = word-by-word captions; with 3 or fewer, each caption keeps its words' own timing unless `minSeconds` is given), `minSeconds`, `maxSeconds`, `gapFrames`). In the desktop app, including over its control channel, running it with no params (or `{}`), as the Text panel's Create captions buttons and Sequence ▸ Transcript do, opens the Create captions dialog instead (its Word by Word preset is `wordsPerCaption: 1`, `lines: 1`, `minSeconds: 0`): the command returns `{"dialog": "captions"}` and creates nothing until Create is pressed. Control-channel callers that want captions straight away pass at least one param (the headless CLI and MCP server have no dialog and always run it). The dialog always sends its Minimum duration, so word-by-word captions keep each word's own timing only when that is 0. |
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). There are two built-in
recognisers, both run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU:

- OpenAI's **Whisper**, with timestamp decoding, language detection and word times from
  cross-attention alignment (see the `filmcraft_speech::whisper` module docs).
- NVIDIA's **Parakeet TDT 0.6B v3**, a FastConformer encoder with a token-and-duration transducer
  decoder. The decoder times every token itself (80 ms frames), so word times are tight enough for
  word-by-word captions, and it writes punctuation and capitals. It recognises English and 24 other
  European languages without being told which; the transcript's language is guessed from common
  words. Long clips are cut into chunks of at most 90 s in pauses (see the
  `filmcraft_speech::parakeet` module docs). Its downloaded weights occupy about 2.5 GB and inference
  needs additional memory; peak memory for long clips remains to be measured in this reconciliation.
  Its known limits are listed in the `filmcraft-speech` README.

Speakers are labelled by clustering per-chunk MFCC statistics (`filmcraft_speech::diarize`); no
model is involved.
Whisper word bounds are also tightened against pauses and silence with `filmcraft_speech::vad`.

Both are **features**: on by default in the `filmcraft` and `filmcraft-cli` apps, so release
builds can transcribe (G19), off by default in the library crates and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference. `--no-default-features` builds the
  apps without it.
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

Weights are **never** bundled or committed. In the app, Transcribe (Text panel, or Sequence ▸
Transcribe Sequence…) without the chosen model asks first: a dialog names the model, its download
size, its source and its licence (with a link), and says the audio stays on the computer.
Download and Transcribe runs `transcript.downloadModel {"wait": false}` as a background job (a
progress bar; Cancel stops it, and a file only counts once it is complete and its SHA-256 checks out), then transcribes. Agents call
`transcript.models` and `transcript.downloadModel` themselves (`wait` defaults to true).

**Licences.** OpenAI releases Whisper code and original weights under MIT; the selected
Hugging Face conversions are labeled Apache-2.0. Candle is MIT OR Apache-2.0. Redistribution
must retain the applicable copyright and licence texts, any required NOTICE information, and
modification notices where applicable. The catalogue summary is not a replacement for these texts.
NVIDIA's Parakeet TDT 0.6B v3 weights are CC BY 4.0: retain NVIDIA attribution, the model source and
licence link, and identify modifications. The inference implementation uses Rust and Candle;
weights are downloaded separately and no FFmpeg, Python or C++ recogniser is shipped.

Models are downloaded into
`<data dir>/models/<id>/` (see `filmcraft_engine::autosave::default_data_dir`), each file pinned to a
revision of the publisher's Hugging Face repository and checked against its SHA-256:

| Id | Languages | Licence |
|---|---|---|
| `whisper-tiny` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-base` (default) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `parakeet-tdt-0.6b-v3` (2.5 GB) | 25 European languages | CC-BY-4.0 (NVIDIA) |

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
the local speech samples (LibriSpeech read speech and dialogue clips) (about 9.5 minutes for the run in a release build on an
Apple-silicon laptop CPU). Measured on 2026-10-22: `parakeet-tdt-0.6b-v3`, English, 5.5 % WER over 799
words of synthesized speech, where every remaining error is a number written as digits
("nine thirty" → "9 30") or one rare word; 11× faster than real time on an Apple-silicon laptop CPU.

## Limits

- The desktop UI runs explicit transcription in a background job with progress and Cancel.
  `transcript.generate` and `sequence.transcribe` keep `wait: true` as their headless default;
  `wait: false` returns a job id. Finished work is one undo step. Cancelled work, work for another
  project, and work whose target media or transcript changed is discarded. Unrelated edits are kept.
- The current contiguous-audio interface supports at most one hour per media item. Audio is decoded
  in short cancellable chunks and allocation failures are reported. Longer sources need splitting
  until streaming recognition is supported.
- Automatic recognition during clip placement remains synchronous to preserve its atomic undo step.
- Source and sequence subclip views resolve the parent transcript, retaining original word indices.
- Word bounds are only moved across pauses. A start that the alignment places late inside
  continuous speech (no pause of 0.2 s between the two words) stays where it is, so the word
  before it can hold the beginning of the next one: the audio level gives no boundary to move it
  to. Words that the alignment places a whole sentence away are not moved back either.
