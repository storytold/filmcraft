# Decupador Inteligente

The studio's decoupage workflow, built into the editor. The decupador project (Python, outside this
repository) keeps deciding *what* to keep: its plans are written from the transcript and the rules
of its `knowledge_base/`. The editor does the rest natively.

## Setup

`<data dir>/decupador.json` (macOS: `~/Library/Application Support/FilmCraft/decupador.json`):

```json
{
  "home": "/path/to/Análise de vídeos",
  "python": "/usr/bin/python3",
  "model": "small",
  "language": "pt"
}
```

`home` is the decupador folder (the one with `src/transcriber.py` and `models/`); `python` must have
`faster-whisper` installed. `FILMCRAFT_DECUPADOR_HOME` overrides `home`. Without the file everything
behaves as upstream FilmCraft.

## Transcription

When the decupador is set up it is the default recogniser: **Transcribe…**, Transcribe Sequence,
auto-transcribe on import and `transcript.generate` without a `model` all go through the
decupador's own `src/transcriber.py` (faster-whisper, word timestamps, per-word probabilities). The
audio is handed over as a 16 kHz mono WAV; the words come back unchanged (`source` =
`decupador-small`), so the word times are exactly the ones its cut plans are written against.
`transcript.generate {"model": "whisper-base"}` still runs the built-in Whisper.

The language is the one in `decupador.json` unless `language` is passed. Speakers are not labelled
(every word is Speaker 1), as in the decupador.

Everything built on transcripts works on top: the Text panel ▸ Transcript tab, text-based editing,
and **Create Captions from Transcript…**.

Code: `crates/speech/src/decupador.rs` (the transcriber), `crates/engine/src/transcript.rs`
(selection of the recogniser).

## The session (how the decupador works with many takes)

Takes transcribed one by one lose the context the recogniser needs, so the decupador transcribes
a whole recording session at once. **Sequence ▸ Decupador Inteligente ▸ Transcribe Session…**
(`decupador.transcribeSession {items?, name?, language?}`) does that inside the editor:

1. The takes (`items`, else the Project selection, else every media item) go in **recording
   order**: the time the camera or recorder wrote in the file (`mvhd`), else the file's creation
   date, then the name. The file date alone is not enough: a copy gets the time it was copied.
2. Their sound is laid end to end and transcribed **once** by the decupador. With separate
   recorder files (Áudio Duplo) the recorder is what is transcribed.
3. The words go back to each file, in its own media time (a word belongs to the take that holds
   its midpoint), so every other transcript feature keeps working.
4. A **session sequence** ("Sessão — …") is built: the transcribed sound end to end on A1; the
   cameras on V1, each where its own audio matches the recorder (waveform cross-correlation,
   `filmcraft_audio_dsp::sync`; the result reports the confidence per camera), or linked to their
   own sound when there is no recorder. Cameras that overlap by a moment stack on V2.

On "Miopia de Marca" (4 camera files + 4 recorder files, imported in shuffled order) the order came
out right and every camera landed within 7 ms of the decupador's `audio_sync.py`.

The session sequence is the reference for plans: its seconds are the decupador's "seconds of the
concatenated audio". `decupador.context {sequence?}` returns what a plan is written from: the
takes and where they sit, the transcript as timed lines (`text`), every word with its confidence
(`words`: `[start, end, text, confidence]` in session seconds) and the step A candidates.
`/decupar` (a Claude Code skill) reads it, decides and applies a plan.

## The script (roteiro)

**Sequence ▸ Decupador Inteligente ▸ Align Script…** (`decupador.alignScript {path?, sequence?}`)
reads the script with the decupador's own reader (`src/multitake.py`: PDF, Markdown or text;
without the decupador, Markdown and text only), keeps the speech of each block (a PDF export glues
the page header to the first line; quoted speech is kept, footers are dropped) and lines every
script line up with the session transcript:

- every **attempt** at every line is found by comparing words (lower case, no accents or
  punctuation, "pra" = "para", plural = singular), best matches first and no two attempts sharing
  a word; an attempt ends where the speaker starts the line over and runs to the end of the
  sentence; a second pass finds false starts in what is left;
- an attempt is **complete** when it gets from the line's start to its end — the line's last
  words, or a full stop when it was said in other words — and **incomplete** when it stops in the
  middle (a trailing "…" always is);
- runs of speech that belong to no line are **off-script** (slate, direction, chat).

They become markers on the session: `Roteiro · Fala 6 — tentativa 3/6 (incompleta)` (one colour per
line; the comment holds the script text, what was said and the coverage), `Roteiro · Fora do
roteiro`, and `Roteiro · Fala 1 — não encontrada` at the start for lines nobody said. Running it
again replaces them. `decupador.transcribeSession` does it by itself when a `roteiro*` / `script*`
`.pdf`, `.md` or `.txt` sits next to the takes (or pass `script`), and `decupador.context` returns
the markers as `script`.

The alignment is a map, not a decision: which attempt to keep is decided with the decupador's
rules. On "Miopia de Marca" it found every attempt at lines 5–8, marked lines 1–4 (shot apart) as
not found and the slate, the direction notes and the chat as off-script.

## Commands (`crates/engine/src/decupador.rs`)

| Command | Menu | What it does |
|---|---|---|
| `decupador.transcribeSession {items?, name?, language?, model?}` | Sequence ▸ Decupador Inteligente ▸ Transcribe Session… | See above. |
| `decupador.context {sequence?, paragraphGap=1.5}` | – | The session as the decupador reads it (JSON). |
| `decupador.alignScript {path?, sequence?}` | Sequence ▸ Decupador Inteligente ▸ Align Script… | See "The script". |
| `decupador.applyPlan {plan \| path, sequence?, cut?}` with a **session plan** (no `source_video`) | Sequence ▸ Decupador Inteligente ▸ Apply Decoupage Plan… | Segments in seconds of the session sequence (`sequence`, else the active one). Each cut becomes a new sequence holding those ranges of the session, back to back, every track: camera and recorder stay in sync because they sit in sync there. Ranges snap to frames; pictures come down to V1 when nothing overlaps. On the September "Miopia de Marca" plan every cut point matched `premiere_cuts.json` within one frame. |
| `decupador.applyPlan {path, cut?, root?}` with a `premiere_cuts.json` (segments name their files) | (same) | One sequence per cut of a `premiere_cuts.json`: each segment straight from the original camera file on V1, with its sound on A1 — the external recorder's file when the plan has `source_audio` (Áudio Duplo), linked to the picture. Segments snap to frames and the audio In moves with the video In, so sync is kept. The sequence takes the camera file's size and rate. Paths are relative to the decupador folder (`<root>/analysis/<slug>/premiere_cuts.json`). Replaces the Premiere UXP panel. |
| `decupador.markCandidates {minSilence=0.8, minProb=0.6, maxGap=0.6, muletas?, replace=true}` | Sequence ▸ Decupador Inteligente ▸ Mark Decoupage Candidates | Step A candidates over the sequence transcript as sequence markers: silences ≥ `minSilence` s (Rose), PT-BR fillers (Yellow; "né", "então", "tipo", "é", "aí", "sabe", "na verdade", "meio que", "assim", "tipo assim", accents significant), low-confidence spans (Mango). Re-running replaces the previous ones (names start with `Decupador · `). |
| `decupador.candidates {…}` | – | The same candidates as JSON, for agents. |

Candidates are never cut automatically: a pause can be rhetorical and "é" is also a verb. The cuts
come from the plan.

## Driving it from Claude Code

Open the editor with the control channel on (`filmcraft --control 9876`; on the studio's machine,
`Empresa/Projetos/Abrir FilmCraft.command`). Then `filmcraft-cli --bridge 127.0.0.1:9876 exec …`
reaches the open project. The `/decupar` skill (`~/.claude/skills/decupar/SKILL.md`) reads
`decupador.context`, the script and the decupador's `knowledge_base/`, writes the plan in session
seconds (kept in the decupador's `analysis/<slug>/filmcraft_plan.json`) and applies it.
