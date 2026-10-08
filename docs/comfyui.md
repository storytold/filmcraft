# ComfyUI clips

A **ComfyUI clip** is a clip whose media is made by a [ComfyUI](https://github.com/comfyanonymous/ComfyUI)
workflow, any workflow. You put it on the timeline, set the workflow's inputs (prompt, seed,
input image…), and generate: FilmCraft runs the workflow on a ComfyUI server and the clip shows
what came back (video, an image, audio, or text). Generate again and the clip gets a new version.

FilmCraft talks to the server over its HTTP API: it never runs models itself, bundles none and
needs no Python. The server can be on this machine (the default `http://127.0.0.1:8188`) or
anywhere on the network.

## In the app

**Window ▸ ComfyUI…** opens the ComfyUI window:

1. **Server**: the address of the ComfyUI server; **Test** checks the connection (and saves the
   address in the preferences).
2. **Load Workflow…**: choose a workflow saved with ComfyUI's **Workflow ▸ Export (API)**. (The
   regular *Save* writes an editor file with `nodes` and `links`; FilmCraft explains when you pick
   one of those.) Every literal input of every node is listed and editable: text, numbers,
   switches, and files for loader nodes (Load Image / Load Audio / Load Video…), which are
   uploaded to the server's input folder when the clip is generated. Changed inputs are marked •;
   ↺ puts the workflow's own value back.
3. **Create Clip** puts a placeholder (a violet matte, as long as **Duration**) at the playhead,
   above the clips there; **Create & Generate** also starts generating.

Select a ComfyUI clip (in the timeline or the Project panel) and the window shows its inputs:
**Apply** stores changes (one undo step), **Generate** runs it, **New Seeds** runs it with new
random seeds (stored with the clip, so the result can be made again). **Clip ▸ Generate ComfyUI
Clip** generates the selected ComfyUI clips. Generation runs in the background: the Progress panel
shows where the prompt is (queued, running, downloading) and Stop takes it off the server's queue.

## What a run brings back

| Output | What happens |
|---|---|
| video | becomes the clip's media; clips longer than the video are shortened to it; a video with sound gets a linked audio clip under each of its video clips |
| image | becomes the clip's media; the clip keeps its length |
| audio | becomes the clip's media; its clips move to an audio track (the first one free at that time, a new one if none is) |
| video / image + a separate audio output | the picture is the clip's media; the sound is imported and laid under it, linked (a video model plus a sound model in one workflow) |
| more files | imported into the project |
| text | kept with the clip (`comfyui.inspect` ▸ `lastRun.texts`, shown in the window); when a run makes no media at all, the placeholder turns transparent and the text becomes a title over each of its clips |

The first video output wins, else the first image, else the first audio. A clip can name the
output nodes it uses (`outputs`); by default it uses all of them. Outputs are classified by file
extension, so any node that writes files works: core *Save Image* / *Save Video* / *Save Audio*,
Video Helper Suite's *Video Combine*, and so on. FilmCraft has to be able to read the file: H.264 /
HEVC / AV1 / VP9 / ProRes MP4, MOV, MKV or WebM, PNG / JPEG / WebP stills, and WAV, FLAC
(ComfyUI's *Save Audio*), MP3, Opus or AAC sound.

Files are written as `<clip name> 001.<ext>`, `002`, … in the output folder set with
`comfyui.settings {outputDir}`, else a `ComfyUI` folder next to the project, else
`ComfyUI Outputs` in the FilmCraft data folder. A generation is one undo step: undo brings the
previous version (or the placeholder) back.

The recipe (workflow, input overrides, server, output nodes) and what the last run reported are
saved in the project (`project.generated`, schema v13), so a project opened elsewhere still knows
how to make every ComfyUI clip again.

## Commands

Everything the window does is a command, so scripts, the CLI and agents (MCP, control channel) can
do it too:

| Command | Does |
|---|---|
| `comfyui.settings {server?, outputDir?, timeoutMinutes?, check?}` | get / set the settings; `check: true` asks the server for its versions (`reachable`, `system`) |
| `comfyui.inspect {workflow? \| path? \| item? \| clip?}` | a workflow's nodes and literal inputs (`kind`: text, int, float, bool, file, other; `seed`), or a ComfyUI clip's recipe, overrides (`override` on each input) and `lastRun` |
| `comfyui.newClip {workflow \| path, inputs?, outputs?, server?, name?, duration=5, time?, track?, place=true, generate=false, wait=false}` | a new ComfyUI clip (placed at the playhead or `time` on video track index `track`, or only in the project with `place: false`) |
| `comfyui.setInputs {item \| clip, inputs?, clearInputs?, workflow? \| path?, outputs?, server?, name?}` | change a clip's recipe; a new workflow keeps the overrides of inputs it still has |
| `comfyui.generate {items? \| item? \| clips? \| clip?, randomizeSeeds=false, dir?, wait=false}` | generate (the selection by default); returns the job id; `wait: true` blocks until done |

`inputs` is a list of overrides: `{"node": "6", "input": "text", "value": "a red lighthouse"}`,
`{"node": "10", "input": "image", "file": "/shots/frame.png"}` (uploaded before the run), or
`{"node": "3", "input": "seed"}` (neither: removes the override). A link between nodes is part of
the graph and can't be overridden.

```sh
filmcraft-cli run - <<'EOF'
{"id": "file.newSequence", "params": {"name": "Shots", "width": 1024, "height": 1024, "fps": 24}}
{"id": "comfyui.newClip", "params": {"path": "sdxl_api.json", "name": "Lighthouse", "duration": 3, "inputs": [{"node": "6", "input": "text", "value": "a red lighthouse on a cliff at dawn"}], "generate": true, "wait": true}}
{"id": "comfyui.generate", "params": {"items": [8], "randomizeSeeds": true, "wait": true}}
EOF
```

## Builds

The desktop app and the CLI talk HTTP(S) to the server with the engine feature `comfyui` (on by
default in `apps/filmcraft` and `apps/filmcraft-cli`; pure-Rust TLS). Without it, and in the web
app, ComfyUI clips can be made, edited and saved, but generating says the build can't reach
ComfyUI. A host can install its own transport (`Session::comfyui.transport`); the tests use the
in-process `filmcraft_comfyui::fake::FakeComfy`.

## Chaining (planned)

Shots that follow each other often need to see each other: the last frame of one shot as the
first frame of the next, a story so far, a voice that carries on. The pieces are in place:

- `comfyui.generate` runs the selected clips **in timeline order, one after the other, in one
  job**, so a clip can wait for the one before it;
- file inputs are **uploaded** to the server before a run, so a previous clip's output file (or a
  frame of it) can be fed to a Load Image / Load Video node like any local file;
- every run records its outputs, files and **text outputs** with the clip (`lastRun`), so text can
  be passed on too (a prompt written by an LLM node for the next shot).

What remains is a binding that names another clip instead of a value or a file (for example
`{"node": "10", "input": "image", "from": "previous", "output": "lastFrame"}`), resolved by the job
just before each run. That is a new shape in the saved recipe, so it will come with a schema bump.
