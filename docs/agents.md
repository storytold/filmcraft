# Agents

FilmCraft is built to be driven by AI agents, and largely built by them. Part 1 covers using
FilmCraft from an agent. Part 2 covers developing FilmCraft as an agent.

# Part 1: driving FilmCraft

## 1. MCP server

`filmcraft-cli mcp` serves MCP on stdio. It has two modes:

| Mode | Command | What it drives |
|---|---|---|
| Headless | `filmcraft-cli mcp --demo` or `--project p.fcproj` (neither = empty project) | an in-process engine session, no window |
| Bridge | `filmcraft-cli mcp --bridge 127.0.0.1:9876` | the running desktop app started with `filmcraft --control 9876` |

**Claude Code.** The repository's `.mcp.json` registers both servers (`filmcraft` = bridge,
`filmcraft-headless` = demo). Both point at `target/release/filmcraft-cli`, so build it first:

```sh
cargo build --release -p filmcraft-cli -p filmcraft
./target/release/filmcraft --control 9876 &      # only for the bridge server
claude                                           # approve the project MCP servers when asked
```

To register the server by hand, or for another MCP client, use the absolute binary path:

```sh
claude mcp add filmcraft-headless -- /abs/path/filmcraft/target/release/filmcraft-cli mcp --demo
```

```json
{ "mcpServers": { "filmcraft": { "command": "/abs/path/filmcraft-cli", "args": ["mcp", "--bridge", "127.0.0.1:9876"] } } }
```

### Tools

| Tool | Modes | Purpose |
|---|---|---|
| `command_list` | both | every command: id, label, menu, shortcut, params, enabled now (`filter`, `enabled_only`) |
| `command_run` | both | run a command `{id, params}`; edits are undoable |
| `command_batch` | both | run several commands in order `{steps: [{id, params}], stop_on_error}` → `{completed, failed, results}` |
| `doc_inspect` | both | the project tree and the active sequence in one call (`project_inspect` + `sequence_inspect`) |
| `render_preview` | both | same as `render_frame` |
| `project_inspect` | both | bins and items with ids, types, durations; active sequence |
| `sequence_inspect` | both | the active sequence: tracks, clips (`start` / `end` and `sourceIn` / `sourceOut` in ticks, frames, `speed`, `reverse`: the clip plays its `sourceIn` to `sourceOut` stretch backward, `gainDb`), effects, transitions, markers (name and `comment`), playhead, selection |
| `media_import` | both | import files by absolute path (`text`, one path per line) |
| `render_frame` | both | PNG of the program frame at `seconds` (headless renders; bridge screenshots the Program monitor) |
| `ui_inspect` | bridge | UI state: tool, workspace, panels, zoom, playback, fps |
| `ui_elements` | bridge | on-screen interactive elements with id, label and rect (`prefix` filter) |
| `ui_click` | bridge | click by `id` or `x`,`y`; `button`, `count`, `modifiers` |
| `ui_drag` | bridge | press–move–release between points or element ids |
| `ui_key` | bridge | key or shortcut: `Space`, `Cmd+K`, `Shift+Delete` |
| `ui_type` | bridge | type text into the focused field |
| `ui_screenshot` | bridge | PNG of the window or one `panel` |
| `ui_control` | bridge | call any control-channel method directly (`ui.set`, `ui.scroll`, `ui.timeline.locate`, …) |

Typical loop: `project_inspect` / `sequence_inspect` → get ids → `command_run` → `render_frame` or
`ui_screenshot` → look at the result → `edit.undo` if it's wrong.

### Conventions

The server follows a few conventions that make it predictable for agents. They are not
FilmCraft-specific: the sibling craft apps' MCP servers can follow the same ones, so an agent that has
driven one app can drive the others.

- **Core tools.** `command_list`, `command_run`, `command_batch`, `doc_inspect` and `render_preview`
  work in both modes. The FilmCraft tools above stay as they are.
- **Titles and annotations.** Every tool has a `title` and the four MCP hints (`readOnlyHint`,
  `destructiveHint`, `idempotentHint`, `openWorldHint`), so a client can let the read-only tools
  (lists, inspects, renders, screenshots) run without asking and ask before edits. When you add a
  tool, set them in its `#[tool(...)]` attribute; a test checks every listed tool.
- **Strict arguments.** An unknown tool argument is a JSON-RPC `-32602` error that names it and the
  accepted ones (`unknown argument "filtr" for command_list; expected: enabled_only, filter`), not a
  silently ignored key. `command_run.params` goes to the command unchanged.
- **Errors.** A failing command is an `isError` result with the engine's message. A line that is not
  JSON gets a `-32700` parse error (id `null`) and the server keeps serving. A command that panics is
  reported as `internal error: …`, and the session stays usable.
- **Resources.** `filmcraft://document` (the project tree and active sequence, as `doc_inspect`) and
  `filmcraft://commands` (the command catalog, as `command_list`), both `application/json`. For MCP
  2026-07-28 clients such as current Claude Code, list and read results carry the `ttlMs` and
  `cacheScope` hints that revision requires.

### Long exports: progress and cancellation

`command_run {"id": "file.exportMedia", "params": {…, "wait": true}}` blocks until the file is
written, in headless and in bridge mode. It uses the MCP progress and cancellation utilities, so a client can show
progress and stop it:

- The export runs as the same background job `jobs.list` shows. The session is locked only for short
  polls, so other requests (`ping`, `doc_inspect`, `jobs.list`, …) are answered while it encodes.
- With `"_meta": {"progressToken": T}` on the `tools/call`, the server sends
  `notifications/progress` `{progressToken: T, progress: <frames done>, total: <frames>, message:
  <job status>}` at most every 100 ms while frames advance. Without a token it sends none.
- `notifications/cancelled` for the request stops the encode at the next batch (`jobs.cancel`),
  deletes the partial output (the movie, or the frames and caption sidecar written so far) and sends
  no response, as the MCP cancellation utility asks.
- Without `wait` the command returns `{job, path}` at once, as before: poll `jobs.list` and stop it
  with `jobs.cancel`. Other exports (`file.exportFrame`, interchange formats, …) finish quickly and
  ignore the token.
- In bridge mode (MCP, and `filmcraft-cli --bridge … exec`) the export is started in the app without
  `wait` and its job polled, however long it takes: the app keeps showing its progress and answering
  other requests, and the call returns the same `{job, path, result}` as headless mode.

## 2. Control channel

`filmcraft --control 9876` (or `FILMCRAFT_CONTROL_PORT=9876`) listens on `127.0.0.1` only. Send one
JSON request per line and get one JSON reply per line. Full method table:
[control-protocol.md](control-protocol.md).

```jsonc
{"id":1,"method":"engine.commands"}
{"id":2,"method":"engine.execute","params":{"command":"file.openDemoProject"}}
{"id":3,"method":"engine.execute","params":{"command":"playhead.set","params":{"seconds":2}}}
{"id":4,"method":"engine.execute","params":{"command":"sequence.addEdit"}}
{"id":5,"method":"engine.execute","params":{"command":"effects.apply","params":{"effect":"Gaussian Blur"}}}
{"id":6,"method":"ui.elements","params":{"prefix":"tools."}}
{"id":7,"method":"ui.click","params":{"id":"tools.Razor"}}
{"id":8,"method":"ui.key","params":{"key":"Cmd+Z"}}
{"id":9,"method":"ui.screenshot","params":{"path":"/tmp/timeline.png","panel":"Timeline"}}
```

Replies look like `{"id":4,"ok":true,"result":{"cuts":2}}` or `{"id":7,"ok":false,"error":"…"}`.
Minimal client:

```python
import json, socket
s = socket.create_connection(("127.0.0.1", 9876)); f = s.makefile("rw")
def call(method, **params):
    f.write(json.dumps({"id": 1, "method": method, "params": params}) + "\n"); f.flush()
    return json.loads(f.readline())
print(call("engine.execute", command="sequence.inspect"))
```

Notes:

- Time is in ticks (254 016 000 000 per second). Commands also accept `seconds`, `frame` or
  `timecode`.
- A track parameter (`track`, `audioTrack`) is a track id or a name such as `"V1"` / `"A2"`.
  Leaving it out picks the command's default track; naming a track the sequence does not have is
  an "invalid parameters" error, never a different track.
  In `timeline.place`, `track` is where the picture goes and `audioTrack` where the sound goes;
  an audio track given as `track` places the sound alone, on that track.
- `engine.execute` also runs UI-only commands (`tool.razor`, `playback.toggle`,
  `window.workspace.color`, `window.panel.<name>`).
- A command that works on the selection is disabled ("no clips selected", "select a clip in the
  Project panel") until something is selected, unless the call names its targets under a key the
  command documents (`clips` / `clip` for timeline clips, `items` / `item` for project items):
  `clip.replaceFromBin {"clips":[12],"item":4}` runs with nothing selected and leaves the
  selection alone. Only the selection condition is answered by the named targets; every other
  one (an open sequence, a filled clipboard, the right kind of clip) still applies, and ids of
  nothing do not count. `command_list` / `engine.commands` keep reporting the selection's state.
- Element ids come from the previous frame. If an element is missing right after a layout change,
  the app retries on later frames before giving up.
- `timeline.move` takes linked partners along while Linked Selection is on: moving a picture
  clip moves its sound by the same offset (pass `"linked": false` to move only the listed clips).
  It returns `{"moved": [ids]}`, every clip that moved.
- Without a window, `filmcraft-cli run script.jsonl` (lines of `{"id":"…","params":{…}}`) runs the
  same commands headlessly. See §2b for the rest of the CLI.

## 2b. Command-line interface

Every command is also one shell call away. Options go anywhere; output is JSON; exit status is 0 on
success, 1 when a command fails and 2 on a usage error. `filmcraft-cli help` prints the reference.

```sh
filmcraft-cli commands razor                     # find ids (add --json for machine output)
filmcraft-cli describe timeline.razor            # one command: menu, shortcut, params, enabled now
filmcraft-cli --demo inspect sequence            # project / sequence as JSON
filmcraft-cli --project p.fcproj --save exec timeline.razor seconds=3.5
filmcraft-cli --project p.fcproj exec effects.apply '{"effect":"Gaussian Blur"}'
filmcraft-cli --project p.fcproj --save import a.mov b.wav
filmcraft-cli --project p.fcproj export out.mp4  # format from the extension; waits for the job
filmcraft-cli --project p.fcproj export out --preset "YouTube 1080p Full HD" --start 0 --end 10
filmcraft-cli export --list-presets prores       # built-in + user presets (--data-dir for another library)
filmcraft-cli --project p.fcproj exec export.queue.add preset="Apple ProRes 422 HQ" path=renders/ start=true wait=true
echo '{"id":"file.newBin","params":{"name":"Selects"}}' | filmcraft-cli --project p.fcproj --save run -
filmcraft-cli --bridge 127.0.0.1:9876 exec window.workspace.color   # the running app
```

`key=value` values are parsed as JSON when they can be (`3.5`, `true`, `[1,2]`), otherwise taken as
strings; dotted keys nest (`color.r=1`). `run` prints one JSON line per command
(`{"line","id","ok","result"|"error"}`) and stops at the first failure unless `--keep-going`.
`--save` writes back to `--project`; `--save-as path` writes elsewhere.

## 3. Verifying UI work

For any UI change, look at the result:

1. `cargo run --release -p filmcraft -- --control 9876`
2. Drive the feature through the control channel or the MCP bridge: commands, then clicks and drags
   by automation id.
3. Assert on `ui.inspect`, `ui.elements` and `sequence.inspect`.
4. Take `ui.screenshot` (the whole window and the panel you changed) and open the PNG.

Screenshots you commit must follow [AGENTS.md](../AGENTS.md) §1: FilmCraft only, openly licensed
media, sidecar and `ATTRIBUTION.md` entry.

# Part 2: developing FilmCraft

## 4. Orientation, in this order

1. [AGENTS.md](../AGENTS.md): absolute rules (assets, clean-room, licences).
2. [CLAUDE.md](../CLAUDE.md): working instructions and non-negotiables.
3. [ROADMAP.md](../ROADMAP.md): read the **honest assessment** and **Where we are lacking** first, then
   milestones, what's done and what's running.
4. [architecture.md](architecture.md), then the README and tests of the crate you'll touch.
5. [contributing.md](contributing.md) (how to add things, gates) and [testing.md](testing.md).

Maintainers also keep a local planning folder, `plan/`. It is gitignored and not in the repository,
and holds the task-level plan, the status file and behaviour reference notes. If you don't have it,
everything you need to contribute is in the public docs above. Ask a maintainer for a task id.

## 5. Autonomous work loop

1. **Orient.** Pick the next task: the next unchecked task in the maintainer status file, or an open
   ROADMAP item. Prefer the gaps in ROADMAP's *Where we are lacking* (speed, correctness on real
   media, measurement, Windows/Linux) over adding more checklist items. Read the relevant
   architecture section and crate README.
2. **Plan tests first.** Write down the acceptance test before writing code.
3. **Implement and test.**
4. **Verify.** Run all gates (`cargo xtask ci`). For UI work, run the app with `--control`, drive it
   and look at the screenshots (§3).
5. **Record.** Update the crate README (behaviour decisions, test results, limitations) and
   ROADMAP.md when a milestone moves. Commit as `M<n>.<k>: …`, then move on to the next task.

For parallel agents, use one git worktree and one `CARGO_TARGET_DIR` per agent, and keep each crate
with one owner ([contributing.md §5](contributing.md#5-parallel-work-several-agents-or-worktrees)).

## 6. Decision rules

Don't block on choices you can make yourself:

- **Behaviour.** Match the publicly documented and observable behaviour of professional editors
  (help pages, published shortcut lists, using the app). Never inspect application internals
  ([AGENTS.md](../AGENTS.md) §2). If behaviour is undocumented, choose what editors most commonly
  expect and record the choice in the crate README.
- **Ask a human only about:**
  - licences outside the allowlist;
  - spending money;
  - publishing or pushing;
  - personal data;
  - destructive git operations;
  - anything AGENTS.md leaves unclear ("when in doubt, leave it out").

## 7. Definition of Done

A feature is done when:

- [ ] its command(s) are registered with label, menu path, shortcut (if any) and a parameter doc;
- [ ] engine tests cover it, including undo/redo and the disabled case;
- [ ] the UI is wired: menu, panel control and shortcut;
- [ ] every new interactive widget has an automation id;
- [ ] you have driven it through the control channel and reviewed a screenshot;
- [ ] all gates pass (`cargo xtask ci`), and new assets have sidecars;
- [ ] the crate README and ROADMAP.md are updated where relevant;
- [ ] it is committed with its task id.

### Reporting progress honestly

ROADMAP.md keeps two numbers: the **feature checklist** (does it exist?) and **ready for real
work** (does it hold up?). When you update either:

- say how a number was obtained: *measured* (a test, a diff, a benchmark you ran) or *estimated*;
- count an approximation, a stub or a partly wired setting as such, never as done;
- record known bugs and limitations in the crate README and the honest assessment, not only
  what works;
- benchmarks state the machine, the load average and before/after on the same build;
- don't raise a percentage without evidence a reviewer can rerun.
