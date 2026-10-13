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

## Commands (`crates/engine/src/decupador.rs`)

| Command | Menu | What it does |
|---|---|---|
| `decupador.applyPlan {path, cut?, root?}` | Sequence ▸ Decupador Inteligente ▸ Apply Decoupage Plan… | One sequence per cut of a `premiere_cuts.json`: each segment straight from the original camera file on V1, with its sound on A1 — the external recorder's file when the plan has `source_audio` (Áudio Duplo), linked to the picture. Segments snap to frames and the audio In moves with the video In, so sync is kept. The sequence takes the camera file's size and rate. Paths are relative to the decupador folder (`<root>/analysis/<slug>/premiere_cuts.json`). Replaces the Premiere UXP panel. |
| `decupador.markCandidates {minSilence=0.8, minProb=0.6, maxGap=0.6, muletas?, replace=true}` | Sequence ▸ Decupador Inteligente ▸ Mark Decoupage Candidates | Step A candidates over the sequence transcript as sequence markers: silences ≥ `minSilence` s (Rose), PT-BR fillers (Yellow; "né", "então", "tipo", "é", "aí", "sabe", "na verdade", "meio que", "assim", "tipo assim", accents significant), low-confidence spans (Mango). Re-running replaces the previous ones (names start with `Decupador · `). |
| `decupador.candidates {…}` | – | The same candidates as JSON, for agents. |

Candidates are never cut automatically: a pause can be rhetorical and "é" is also a verb. The cuts
come from the plan.

## Driving it from Claude Code

`filmcraft --control 9876` (JSON lines) or `filmcraft-cli mcp --bridge` expose every command, so a
Claude Code session can transcribe, read `decupador.candidates`, write the plan with the
decupador's rules and apply it, without leaving the editor.
