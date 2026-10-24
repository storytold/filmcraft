# Colour: Auto Correct

Lumetri Color's Basic Correction has an **Auto Correct** button (and the menu entry **Color ▸ Auto
Correct**) that measures the clip's picture and sets Temperature, Tint and Exposure for it. The same
thing is one command, `lumetri.auto`, so a script or an agent can correct a whole bin of clips with one
call each and read back what was measured, what was set and how sure the measurement is.

Code: `crates/render/src/auto_correct.rs` (the measurement and the solve, pure functions on frames)
and `crates/engine/src/color_auto.rs` (the command). The other colour commands are listed in
[control-protocol.md](control-protocol.md).

## What it measures

The analysis runs on the clip's **source picture**: the frame is rendered with the clip's own Lumetri
switched off (every other effect, the input colour management and the RAW development still apply),
so running Auto Correct twice measures the same picture and lands on the same values. It reads either
the frame at the playhead (`frames: "playhead"`, the default; the playhead is clamped into the clip)
or `samples` frames spread evenly over the clip (`frames: "sampled"`, 5 by default, at most 16), whose
pixels are pooled. Frames are measured at quarter resolution and shrunk to about 192 px wide, with at
most about a million pixels per call, so a 6K clip costs no more than a small one.

1. **Pixels.** Transparent pixels and pixels with a non-finite component are dropped.
2. **White balance** is a robust grey-world estimate in linear light. Clipped pixels (a channel at or
   above 92 %) and near-black pixels (below 1 %) are rejected, since neither carries the illuminant.
   Strongly saturated pixels (a red car, not a grey wall) are down-weighted by their saturation measured
   *after* a provisional balance, in two passes, so a heavy tungsten cast does not make every neutral
   surface look "saturated" and get thrown out. A second estimate takes the bright but unclipped pixels
   (the 85th to 99th luma percentile, usually lit neutral surfaces); the two are blended (75 / 25 in log
   space) and their disagreement lowers the confidence.
3. **Exposure** comes from luma percentiles of every opaque pixel after the white balance: the 50th
   percentile is brought to the target mid grey (`targetGrey`, 18 % linear by default) and the result
   is capped so the 99th percentile stays at or below `highlightCeiling` (90 %). A correction therefore
   never pushes highlights into clipping, and an over-exposed clip is pulled down.
4. **Confidence** (0…1, per measurement and overall) multiplies simple terms: how much of the picture
   was usable, how many pixels were measured, how well the two illuminant estimates agree, whether the
   signal was very dark or flat, and whether a value had to be clamped to a slider's range. Treat values
   under about 0.4 as a suggestion to look at the clip rather than a correction to ship.

The solve inverts Lumetri's own Basic Correction model (`filmcraft_render::effects::white_balance_gains`:
per-channel gains `1 + 0.35·temperature`, `1 − 0.3·tint`, `1 − 0.35·temperature` in linear light, then
`2^exposure`), so the result is an ordinary, editable grade: Temperature and Tint in slider units
(−100…100) and Exposure in stops (−5…5). Values that hit a slider's end are reported as `clamped`.

Measurements are made in the sequence's working space; the mid-grey target is an SDR target. In a
Rec. 2100 sequence the result is still a usable starting point, but check it against HDR White.

## The command

```jsonc
lumetri.auto {
  "clips": [id] | "clip": id | "items": [id],   // timeline clips, or every video clip of these project items
  "frames": "playhead" | "sampled",             // default playhead
  "samples": 5,                                 // with "sampled": 1…16 frames over the clip
  "whiteBalance": true, "exposure": true,       // what to measure and set
  "into": "lumetri" | "raw",                    // where to write (below)
  "apply": true,                                // false: measure and report only
  "targetGrey": 0.18, "highlightCeiling": 0.9   // linear luma targets
}
```

Without `clips` / `clip` / `items` it works on the timeline selection, else on the Project panel's
selection (every video clip of those items in the active sequence). The menu entry is enabled when a
sequence is open and clips or items are selected; a call that names its targets runs without a
selection, as every FilmCraft command does ([agents.md](agents.md) §2).

**Where the values go.** `into: "lumetri"` writes Temperature, Tint and Exposure into the clip's
Lumetri Color (added before the intrinsic effects when the clip has none, like `effects.apply`), switches
its Basic Correction section and the effect on, and leaves keyframes alone: an animated parameter keeps
its keyframes and gets the new static value, and the result names it under `keyframed`.
`into: "raw"` is for camera RAW clips whose development stage is the better place for white balance and
exposure (media whose decoder offers development controls): the command asks
`clip.sourceSettings` for the clip's media item and, when it offers a `kelvin` control, writes `kelvin`
(from the measured red/blue cast with a mired model around the current setting, 124 mired per stop of
cast), `tint` (Lumetri units) and `exposure` (stops) through it. Media without RAW development controls,
and builds whose `clip.sourceSettings` does not develop yet, fall back to Lumetri and say so in `note`.
The development belongs to the media, so when several clips of one batch use the same media, the first
develops it and the others get Lumetri (with a `note`), each keeping its own correction.

Every clip is **one undo step** ("Auto Correct"), the added effect included. Hostile parameters are
bounded (`samples` to 1…16, the targets to sane ranges, slider values to their ranges); frames with no
opaque pixels or with NaNs measure as zero confidence and set nothing.

**Result.** One entry per clip, in order; a clip that cannot be measured (an audio clip, a clip
without a picture) is listed under `errors` and the rest proceed. If nothing succeeded the command
fails with the first error. A sampled frame that does not render is left out: `frames` and `times`
name only the frames measured, the item lists the others under `failedTimes`, and `errors` gets an
entry for the clip with those `times`.

```jsonc
{
  "corrected": 1,
  "items": [{
    "clip": 12, "item": 4, "frames": 1, "times": [508032000000], "applied": true,
    "confidence": 0.82,
    "measured": {
      "pixels": 20736, "usable": 15210,
      "mean": [0.21, 0.17, 0.11], "bright": [0.71, 0.62, 0.44], "gains": [0.81, 1.0, 1.55], "cast": 1.91,
      "p1": 0.004, "p50": 0.09, "p99": 0.64,
      "temperature": -31.2, "tint": 4.0, "exposure": 0.98,
      "whiteBalanceConfidence": 0.82, "exposureConfidence": 1.0, "confidence": 0.82, "clamped": false
    },
    "set": {"into": "lumetri", "clip": 12, "effect": 0, "temperature": -31.2, "tint": 4.0, "exposure": 0.98}
  }],
  "errors": []
}
```

`cast` is red / blue of the measured illuminant (above 1 warm, below 1 cool); `gains` are the
per-channel gains that would neutralise it (green = 1); `p1` / `p50` / `p99` are linear luma
percentiles after the white balance and before the exposure change.

## In the app

- **Lumetri Color ▸ Basic Correction ▸ Auto Correct** (automation id `lumetri.auto`) runs
  `lumetri.auto {clip, frames: "playhead"}` on the clip the panel shows.
- **Color ▸ Auto Correct** runs it on the selection (timeline clips first, else Project panel items).
- Undo takes the whole correction back, Edit ▸ Undo once per clip.

## Batch: import a folder, auto-correct every clip, export

Over the control channel (`filmcraft --control 9876`, with `FILMCRAFT_CONTROL_TOKEN` set as
[control-protocol.md](control-protocol.md) describes; the same calls work through the MCP tools
`command_run` / `command_batch` and `filmcraft-cli run`). The script below is
[docs/examples/auto_correct_batch.py](examples/auto_correct_batch.py):

```bash
python docs/examples/auto_correct_batch.py footage/card01 out/reel.mp4 --preset "Match Source – Adaptive High Bitrate" --frames sampled --min-confidence 0.4
```

It imports every media file of the folder (`file.import`), builds a sequence from them back to back
(`file.newSequence {fromItems}`), measures and corrects each clip with one `lumetri.auto` call
(`into: "raw"` for clips with RAW development controls when asked, Lumetri otherwise), prints the table of measured and set
values, leaves clips under the confidence threshold alone (it measures them with `apply: false` first and
applies only the confident ones; a clip that cannot be measured or corrected stops it before the
export), then exports with a named preset (`file.exportMedia {path, preset,
wait}`; `export.presets.list` names the presets) and waits for the job in `jobs.list`, since a long
export outlives the control channel's one-minute request timeout.

```python
import json, os, socket, sys, time, pathlib

def connect(port=9876):
    token = os.environ.get("FILMCRAFT_CONTROL_TOKEN") or sys.exit("set FILMCRAFT_CONTROL_TOKEN")
    s = socket.create_connection(("127.0.0.1", port)); f = s.makefile("rw", encoding="utf-8")
    n = [0]
    def call(method, **params):
        n[0] += 1
        f.write(json.dumps({"id": n[0], "token": token, "method": method, "params": params}) + "\n"); f.flush()
        r = json.loads(f.readline())
        if not r.get("ok"):
            raise RuntimeError(f"{method}: {r.get('error')}")
        return r.get("result")
    return call

def run(call, command, **params):
    return call("engine.execute", command=command, params=params)

folder, out, preset = sys.argv[1], sys.argv[2], "Match Source – Adaptive High Bitrate"
call = connect()
paths = sorted(str(p) for p in pathlib.Path(folder).iterdir() if p.is_file())
items = run(call, "file.import", paths=paths)["items"]
seq = run(call, "file.newSequence", name="Auto", fromItems=items)["sequence"]
measured = run(call, "lumetri.auto", items=items, frames="sampled", apply=False)
if measured["errors"]:  # a clip that could not be measured: don't correct or export
    sys.exit(f"not measured: {measured['errors']}")
good = [m["clip"] for m in measured["items"] if m["confidence"] >= 0.4]
if good:
    result = run(call, "lumetri.auto", clips=good, frames="sampled", into="lumetri")
    print(result)
    if result["errors"]:  # a clip left uncorrected: don't export
        sys.exit(f"not corrected: {result['errors']}")
job = run(call, "file.exportMedia", path=out, preset=preset, sequence=seq, wait=False)["job"]
while any(j["id"] == job and not j.get("finished") for j in run(call, "jobs.list")):
    time.sleep(2)
done = next((j for j in run(call, "jobs.list") if j["id"] == job), {})
if (done.get("result") or {}).get("error"):
    sys.exit(f"export failed: {done['result']['error']}")
```
