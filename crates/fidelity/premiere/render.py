#!/usr/bin/env python3
"""Render the fidelity corpus in Adobe Premiere Pro, by script.

The Premiere side of ``crates/fidelity`` (``PREMIERE.md``, which ``cargo xtask fidelity`` writes
next to the manifest, is the same protocol for a person): for every case, import the FCP7 XML,
check the sequence settings, apply the effects the XML cannot carry, and export Premiere's render
where the harness looks for it. Talks to a running Premiere Pro on this machine through the
bridge panel in ``bridge/`` (``install.sh`` / ``install.ps1`` put it into Premiere; open it once
from Window > Extensions > FilmCraft Fidelity), which runs the ExtendScript this script drops into
``~/.filmcraft/fidelity-bridge/`` and answers in the same directory. Nothing of Adobe's is used but
Premiere's own scripting API. Maintainer tooling: never runs in CI, needs no Python packages, and
writes only under the reference root (``plan/premiere/fidelity``, gitignored) and the bridge
directory.

    python3 crates/fidelity/premiere/render.py target/fidelity/manifest.json
    python3 crates/fidelity/premiere/render.py MANIFEST --case cut --case crop
    python3 crates/fidelity/premiere/render.py MANIFEST --dry-run

Exports go through Adobe Media Encoder, the path a user's export takes, with two presets saved
once from Premiere's export dialog (File > Export > Media, then Save Preset on the Preset row),
found by name under ``~/Documents/Adobe/Adobe Media Encoder/<version>/Presets/`` (where Premiere
Pro 2026 puts them) or ``~/Library/Application Support/Adobe/Common/AME/<version>/Presets/``:

* ``fidelity-png``: Format PNG, Export As Sequence, 8 bpc, no alpha, Match Source size and rate;
* ``fidelity-wav``: Format Waveform Audio, 48 kHz, 24-bit, stereo.

``--video-preset`` / ``--audio-preset`` name other ``.epr`` files. Without a PNG preset (or with
``--frame-export``) the picture cases come out of Premiere's frame export instead, one PNG per
frame with the playhead at the exact tick of every frame (254 016 000 000 ticks per second, the
same clock as FilmCraft's), in one ExtendScript call per case. Without a WAV preset the sound
cases are skipped and the run says so.

Every run makes a fresh ``.prproj`` under ``<reference root>/_premiere/``, saves it at the end
and closes it (``--keep-open`` leaves it open to look at). Premiere's scripting host only answers
while a project is open, so a run that finds no bridge heartbeat opens the newest saved project
in Premiere itself and waits; the very first time, open Premiere with any project by hand. Do not
use Premiere while a run drives it. Premiere's FCP XML import may drop an "FCP Translation
Results" text file per case with issues into ``~/Documents``; it says what it did not translate.
Exit status 0 when every selected case rendered, 1 otherwise; a case that fails is reported and
the run goes on to the next.
"""

import argparse
import datetime
import glob
import json
import os
import subprocess
import sys
import time
import uuid

TICKS_PER_SECOND = 254_016_000_000
BRIDGE_DIR = "~/.filmcraft/fidelity-bridge"
HEARTBEAT_FRESH_S = 3.0
HEARTBEAT_DEAD_S = 15.0
PING_TIMEOUT_S = 10
WAKE_TIMEOUT_S = 300
COMMAND_TIMEOUT_S = 600
PRESET_DIRS = ("~/Documents/Adobe/Adobe Media Encoder", "~/Library/Application Support/Adobe/Common/AME")
PNG_PRESET = "fidelity-png"
WAV_PRESET = "fidelity-wav"
SETTLE_SECONDS = 30
AME_TIMEOUT_SECONDS = 900


class Failure(Exception):
    """A step that did not do what it said; the message is for the person."""


class Skipped(Failure):
    """A case not rendered for want of a preset; counts against the exit status like a failure."""


class HostBlocked(Exception):
    """A command timed out inside Premiere's scripting host (a dialog, most likely). The script is
    still running there and every later one queues behind it, so the run stops; not a Failure, so
    the per-case handler does not swallow it."""


# ---------------------------------------------------------------------------------------------
# The bridge

# ExtendScript has no JSON and no Array.prototype.indexOf; every script gets these.
HELPERS_JSX = r"""
function __str(v) {
  if (v === null || typeof v === "undefined") return "null";
  var t = typeof v;
  if (t === "string") return '"' + v.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\r/g, "\\r").replace(/\n/g, "\\n").replace(/\t/g, "\\t") + '"';
  if (t === "number") return isFinite(v) ? String(v) : "null";
  if (t === "boolean") return v ? "true" : "false";
  if (t === "function") return "null";
  if (v instanceof Array) { var a = []; for (var i = 0; i < v.length; i++) a.push(__str(v[i])); return "[" + a.join(",") + "]"; }
  var o = [];
  for (var k in v) { if (typeof v[k] === "function") continue; o.push(__str(String(k)) + ":" + __str(v[k])); }
  return "{" + o.join(",") + "}";
}
if (typeof JSON === "undefined") { JSON = {}; }
if (typeof JSON.stringify !== "function") { JSON.stringify = __str; }
function __fail(message) { return JSON.stringify({ success: false, error: String(message) }); }
function __seq(id) {
  for (var i = 0; i < app.project.sequences.numSequences; i++) {
    if (String(app.project.sequences[i].sequenceID) === id) return app.project.sequences[i];
  }
  return null;
}
"""


class Bridge:
    """One command at a time through the panel's directory."""

    def __init__(self, directory):
        self.dir = os.path.expanduser(directory)
        os.makedirs(self.dir, exist_ok=True)

    def heartbeat_age(self):
        """Seconds since the panel last ticked, or None when it never has."""
        path = os.path.join(self.dir, "heartbeat.json")
        try:
            with open(path, encoding="utf-8") as handle:
                data = json.load(handle)
            return max(0.0, time.time() - float(data["ts"]) / 1000.0)
        except (OSError, ValueError, KeyError, TypeError):
            return None

    def alive(self):
        age = self.heartbeat_age()
        return age is not None and age < HEARTBEAT_FRESH_S

    def answers(self):
        """A fresh heartbeat and an answered ping: a heartbeat alone can be seconds old from a
        Premiere that has just quit."""
        if not self.alive():
            return False
        try:
            return bool(self.run('return JSON.stringify({ success: true });', timeout=PING_TIMEOUT_S).get("success"))
        except Failure:
            return False

    def run(self, script, timeout=COMMAND_TIMEOUT_S):
        """Run ExtendScript in Premiere; the script `return`s a JSON string, which comes back parsed.
        A `{success: false, error}` answer or a thrown error is a Failure; a timeout inside the host
        is HostBlocked and ends the run."""
        if not self.alive():
            raise Failure("the bridge panel is not answering (Window > Extensions > FilmCraft Fidelity, with a project open)")
        cid = f"{int(time.time() * 1000):013d}-{uuid.uuid4().hex[:8]}"
        tmp = os.path.join(self.dir, cid + ".cmd.tmp")
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump({"script": HELPERS_JSX + script, "timeout_ms": int(timeout * 1000)}, handle)
        os.replace(tmp, os.path.join(self.dir, cid + ".cmd"))
        out = os.path.join(self.dir, cid + ".out")
        deadline = time.monotonic() + timeout + 10
        while not os.path.exists(out):
            if time.monotonic() > deadline:
                raise Failure(f"no answer from the bridge in {timeout}s")
            age = self.heartbeat_age()
            if age is None or age > HEARTBEAT_DEAD_S:
                # the panel stopped ticking (Premiere quit or the panel closed): nobody will answer
                try:
                    os.remove(os.path.join(self.dir, cid + ".cmd"))
                except OSError:
                    pass
                raise Failure("the bridge panel stopped answering: Premiere quit, or the panel was closed")
            time.sleep(0.1)
        with open(out, encoding="utf-8") as handle:
            answer = json.load(handle)
        os.remove(out)
        if not answer.get("ok"):
            if answer.get("timeout"):
                raise HostBlocked(str(answer.get("error", "the command timed out in Premiere")))
            raise Failure(str(answer.get("error", "unknown bridge error")))
        raw = answer.get("result")
        try:
            data = json.loads(raw) if isinstance(raw, str) and raw else raw
        except ValueError:
            data = {"raw": raw}
        if isinstance(data, dict) and data.get("success") is False:
            raise Failure(str(data.get("error", "Premiere reported failure")))
        return data


def premiere_app():
    """The newest Premiere Pro app bundle, for waking a cold Premiere (macOS)."""
    apps = sorted(glob.glob("/Applications/Adobe Premiere Pro */Adobe Premiere Pro *.app"))
    return apps[-1] if apps else None


def ensure_bridge(bridge, workdir):
    """A panel that answers: already up, or Premiere opened on the newest saved project."""
    if bridge.answers():
        return
    projects = sorted(glob.glob(os.path.join(workdir, "*.prproj")), key=os.path.getmtime)
    if not projects:
        raise Failure(
            "the bridge is not answering and there is no saved project to wake Premiere with. "
            "Open Premiere Pro with any project (File > New > Project is fine), open Window > Extensions > "
            "FilmCraft Fidelity, and run again (install.sh puts the panel in place)."
        )
    print(f"  bridge: no heartbeat; opening {os.path.basename(projects[-1])} in Premiere to wake it")
    if sys.platform == "darwin":
        app = premiere_app()
        if not app:
            raise Failure("no Adobe Premiere Pro app under /Applications")
        subprocess.run(["open", "-a", app, projects[-1]], check=False)
    elif sys.platform == "win32":
        os.startfile(projects[-1])  # noqa: S606 - the saved project, by its file association
    else:
        raise Failure("waking Premiere is implemented on macOS and Windows only; open it yourself and run again")
    deadline = time.monotonic() + WAKE_TIMEOUT_S
    while time.monotonic() < deadline:
        if bridge.alive() and bridge.answers():
            time.sleep(2.0)  # the project panel settles
            return
        time.sleep(1.0)
    raise Failure(f"Premiere did not bring the bridge up within {WAKE_TIMEOUT_S}s (is the panel open in the workspace?)")


# ---------------------------------------------------------------------------------------------
# Steps in Premiere


def create_project(bridge, path):
    script = f"""
var wanted = {json.dumps(path)};
var folder = new Folder({json.dumps(os.path.dirname(path))});
if (!folder.exists && !folder.create()) return __fail("could not create " + folder.fsName);
app.newProject(wanted);
var got = app.project && app.project.path ? String(app.project.path) : "";
if (got !== wanted) return __fail("Premiere opened " + got + " instead of " + wanted);
return JSON.stringify({{ success: true, path: got }});
"""
    return bridge.run(script)["path"]


def sequences(bridge):
    script = """
var out = {};
for (var i = 0; i < app.project.sequences.numSequences; i++) {
  var s = app.project.sequences[i];
  out[String(s.sequenceID)] = { name: String(s.name) };
}
return JSON.stringify({ success: true, sequences: out });
"""
    return bridge.run(script)["sequences"]


def import_case(bridge, entry):
    """Import the case's FCP7 XML (UI suppressed: no Translation Report dialog); return the new
    sequence's id."""
    before = sequences(bridge)
    script = f"""
var f = new File({json.dumps(entry["fcp7_xml"])});
if (!f.exists) return __fail("missing " + f.fsName);
var ok = app.project.importFiles([{json.dumps(entry["fcp7_xml"])}], true, app.project.rootItem, false);
if (!ok) return __fail("importFiles refused " + f.fsName);
return JSON.stringify({{ success: true }});
"""
    bridge.run(script)
    after = sequences(bridge)
    new = {sid: s for sid, s in after.items() if sid not in before}
    if len(new) != 1:
        named = {sid: s for sid, s in new.items() if s.get("name") == entry["id"]}
        if len(named) != 1:
            raise Failure(f"import made {len(new)} sequences, expected one named {entry['id']}")
        new = named
    return next(iter(new))


def check_settings(bridge, seq_id, settings):
    script = f"""
var seq = __seq({json.dumps(seq_id)});
if (!seq) return __fail("sequence not found");
var s = seq.getSettings();
return JSON.stringify({{ success: true, width: s.videoFrameWidth, height: s.videoFrameHeight, timebase: String(seq.timebase),
  sampleTicks: s.audioSampleRate ? String(s.audioSampleRate.ticks) : "0" }});
"""
    got = bridge.run(script)
    rate = settings["frame_rate"]
    want_timebase = TICKS_PER_SECOND * rate["den"] // rate["num"]
    want_sample = TICKS_PER_SECOND // settings["sample_rate"]
    problems = []
    if (got.get("width"), got.get("height")) != (settings["width"], settings["height"]):
        problems.append(f"frame size {got.get('width')}×{got.get('height')}, want {settings['width']}×{settings['height']}")
    if int(got.get("timebase", 0)) != want_timebase:
        problems.append(f"timebase {got.get('timebase')} ticks, want {want_timebase}")
    if int(got.get("sampleTicks", 0)) != want_sample:
        problems.append(f"sample period {got.get('sampleTicks')} ticks, want {want_sample} ({settings['sample_rate']} Hz)")
    if problems:
        raise Failure("sequence settings differ from the corpus: " + "; ".join(problems))


def set_active(bridge, seq_id):
    bridge.run(f"""
var seq = __seq({json.dumps(seq_id)});
if (!seq) return __fail("sequence not found");
app.project.activeSequence = seq;
return JSON.stringify({{ success: true }});
""")


EFFECTS_JSX = r"""
app.enableQE();
var seq = __seq(SEQ);
if (!seq) return __fail("sequence not found");
app.project.activeSequence = seq;
if (seq.videoTracks.numTracks < 1 || seq.videoTracks[0].clips.numItems < 1) return __fail("no clip on V1");
var clip = seq.videoTracks[0].clips[0];
var qeSeq = qe.project.getActiveSequence();
var qeTrack = qeSeq ? qeSeq.getVideoTrackAt(0) : null;
var qeClip = null;
if (qeTrack) {
  for (var i = 0; i < qeTrack.numItems; i++) {
    var item = qeTrack.getItemAt(i);
    if (item && String(item.type) === "Clip" && String(item.start.ticks) === String(clip.start.ticks)) { qeClip = item; break; }
  }
}
if (!qeClip) return __fail("no QE clip matches the clip on V1");
function component(name) {
  for (var c = 0; c < clip.components.numItems; c++) if (String(clip.components[c].displayName) === name) return clip.components[c];
  return null;
}
function property(comp, name) {
  for (var p = 0; p < comp.properties.numItems; p++) if (String(comp.properties[p].displayName) === name) return comp.properties[p];
  return null;
}
var report = [];
for (var e = 0; e < EFFECTS.length; e++) {
  var wanted = EFFECTS[e];
  var comp = component(wanted.name);
  var added = false;
  if (!comp) {
    var fx = qe.project.getVideoEffectByName(wanted.name);
    if (!fx) return __fail("Premiere has no video effect named " + wanted.name);
    var before = clip.components.numItems;
    qeClip.addVideoEffect(fx);
    if (clip.components.numItems <= before) return __fail("adding " + wanted.name + " made no component");
    comp = clip.components[clip.components.numItems - 1];
    if (String(comp.displayName) !== wanted.name) return __fail("added " + wanted.name + " but the new component is " + comp.displayName);
    added = true;
  }
  var set = [];
  for (var key in wanted.params) {
    var prop = property(comp, key);
    if (!prop) return __fail(wanted.name + " has no property " + key);
    prop.setValue(wanted.params[key], true);
    var got = prop.getValue();
    if (String(got) !== String(wanted.params[key])) return __fail(wanted.name + " " + key + " reads " + got + " after setting " + wanted.params[key]);
    set.push(key + " = " + got);
  }
  report.push({ effect: wanted.name, added: added, set: set });
}
return JSON.stringify({ success: true, effects: report });
"""


def apply_effects(bridge, seq_id, effects):
    """Apply the case's effects to the clip on V1: set an existing same-named component (the import
    may have made one, e.g. Crop), else add it; every value is read back."""
    script = f"var SEQ = {json.dumps(seq_id)};\nvar EFFECTS = {json.dumps(effects)};\n{EFFECTS_JSX}"
    return bridge.run(script).get("effects", [])


FRAMES_JSX = r"""
var seq = __seq(SEQ);
if (!seq) return __fail("sequence not found");
app.project.activeSequence = seq;
app.enableQE();
var qeSeq = qe.project.getActiveSequence();
if (!qeSeq || String(qeSeq.name) !== String(seq.name)) return __fail("QE active sequence is " + (qeSeq ? qeSeq.name : "none"));
var tb = parseInt(String(seq.timebase), 10);
for (var f = FIRST; f < FIRST + COUNT; f++) {
  seq.setPlayerPosition(String(tb * f));
  var tc = String(qeSeq.CTI.timecode);
  var n = String(f);
  while (n.length < 3) n = "0" + n;
  qeSeq.exportFramePNG(tc, OUT_DIR + "/" + STEM + n);
}
return JSON.stringify({ success: true, first: FIRST, count: COUNT });
"""


def frame_path(out_dir, stem, index):
    return os.path.join(out_dir, f"{stem}{index:03d}.png")


def export_frames(bridge, seq_id, out_dir, stem, first, count):
    script = (
        f"var SEQ = {json.dumps(seq_id)};\nvar OUT_DIR = {json.dumps(out_dir)};\nvar STEM = {json.dumps(stem)};\n"
        f"var FIRST = {first};\nvar COUNT = {count};\n{FRAMES_JSX}"
    )
    bridge.run(script)
    missing = [i for i in range(first, first + count) if not os.path.isfile(frame_path(out_dir, stem, i))]
    if missing:
        raise Failure(f"Premiere reported the export but {len(missing)} of {count} frames are missing (first: {frame_path(out_dir, stem, missing[0])})")


def png_pixels(data):
    """The IHDR and IDAT chunks only: Premiere stamps the export time into a text chunk, so two
    exports of the same frame never match byte for byte."""
    chunks = []
    pos = 8
    while pos + 8 <= len(data):
        length = int.from_bytes(data[pos : pos + 4], "big")
        kind = data[pos + 4 : pos + 8]
        if kind in (b"IHDR", b"IDAT"):
            chunks.append(data[pos + 8 : pos + 8 + length])
        pos += 12 + length
    return b"".join(chunks)


def settle(bridge, seq_id, out_dir):
    """Wait until frame 0 renders the same twice: a freshly imported clip can still be linking or
    conforming, and Premiere renders a placeholder card meanwhile."""
    deadline = time.monotonic() + SETTLE_SECONDS
    previous = None
    while True:
        export_frames(bridge, seq_id, out_dir, "_settle", 0, 1)
        path = frame_path(out_dir, "_settle", 0)
        with open(path, "rb") as handle:
            current = png_pixels(handle.read())
        os.remove(path)
        if previous is not None and current == previous:
            return
        if time.monotonic() > deadline:
            raise Failure(f"frame 0 kept changing for {SETTLE_SECONDS}s: media still linking?")
        previous = current
        time.sleep(1.0)


def clear_stills(out_dir, stem):
    for name in stills_of(out_dir, stem):
        os.remove(os.path.join(out_dir, name))


def stills_of(out_dir, stem):
    return sorted(n for n in os.listdir(out_dir) if n.startswith(stem) and n.endswith(".png") and n[len(stem) : -4].strip("_-.").isdigit())


def wait_for_file(path, timeout):
    """A Media Encoder job is done when its file exists and stops growing."""
    deadline = time.monotonic() + timeout
    last = None
    stable_since = None
    while time.monotonic() < deadline:
        size = os.path.getsize(path) if os.path.isfile(path) else None
        if size is not None and size > 0 and size == last:
            if stable_since is None:
                stable_since = time.monotonic()
            elif time.monotonic() - stable_since >= 3.0:
                return
        else:
            stable_since = None
        last = size
        time.sleep(1.0)
    raise Failure(f"Media Encoder did not finish {path} in {timeout}s")


def queue_export(bridge, seq_id, out_path, preset):
    """Queue the whole sequence in Media Encoder with a saved preset and start the batch."""
    if not os.path.isfile(preset):
        raise Failure(f"preset not found: {preset}")
    script = f"""
var seq = __seq({json.dumps(seq_id)});
if (!seq) return __fail("sequence not found");
if (typeof app.encoder === "undefined") return __fail("app.encoder is not available in this Premiere");
try {{ app.encoder.launchEncoder(); }} catch (e1) {{}}
var range = (typeof app.encoder.ENCODE_ENTIRE !== "undefined") ? app.encoder.ENCODE_ENTIRE : 0;
var job = app.encoder.encodeSequence(seq, {json.dumps(out_path)}, {json.dumps(preset)}, range, 1);
if (!job) return __fail("encodeSequence returned no job: is Media Encoder installed and the preset valid?");
try {{ app.encoder.startBatch(); }} catch (e2) {{}}
return JSON.stringify({{ success: true, job: String(job) }});
"""
    bridge.run(script)


def export_with_preset(bridge, seq_id, out_path, preset):
    queue_export(bridge, seq_id, out_path, preset)
    wait_for_file(out_path, AME_TIMEOUT_SECONDS)


def export_stills_with_preset(bridge, seq_id, out_dir, stem, frames, preset):
    """A PNG sequence through Media Encoder: done when every frame is there and the last one has
    stopped growing (Media Encoder numbers the files itself)."""
    queue_export(bridge, seq_id, os.path.join(out_dir, f"{stem}.png"), preset)
    deadline = time.monotonic() + AME_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        names = stills_of(out_dir, stem)
        if len(names) >= frames:
            wait_for_file(os.path.join(out_dir, names[-1]), 60)
            names = stills_of(out_dir, stem)  # Media Encoder may have written more meanwhile
            if len(names) > frames:
                raise Failure(f"Media Encoder wrote {len(names)} stills, expected {frames}")
            return
        time.sleep(1.0)
    raise Failure(f"Media Encoder wrote {len(stills_of(out_dir, stem))} of {frames} stills in {AME_TIMEOUT_SECONDS}s")


def save_project(bridge):
    bridge.run('app.project.save(); return JSON.stringify({ success: true });')


def close_project(bridge):
    # closeDocument(saveFirst = 0, promptIfDirty = 0): the save before it is the record, and no
    # dialog can block the host. Premiere may then sit with no project; the next run wakes it.
    bridge.run('app.project.closeDocument(0, 0); return JSON.stringify({ success: true });')


def find_preset(name):
    """The newest `<name>.epr` among the Media Encoder preset folders, or None."""
    hits = [h for d in PRESET_DIRS for h in glob.glob(os.path.join(os.path.expanduser(d), "*", "Presets", name + ".epr"))]
    return max(hits, key=os.path.getmtime) if hits else None


# ---------------------------------------------------------------------------------------------
# A case, and the run


def render_case(bridge, entry, settings, out_dir, audio_preset, video_preset):
    """Everything for one case; returns a one-line summary."""
    os.makedirs(out_dir, exist_ok=True)
    # the previous run's render goes first: a case that fails or is skipped below leaves nothing
    # behind for the harness to score as if it were current
    clear_stills(out_dir, entry["id"])
    wav_path = os.path.join(out_dir, f"{entry['id']}.wav")
    if os.path.exists(wav_path):
        os.remove(wav_path)
    seq_id = import_case(bridge, entry)
    check_settings(bridge, seq_id, settings)
    set_active(bridge, seq_id)
    notes = []
    if entry["premiere_effects"]:
        for done in apply_effects(bridge, seq_id, entry["premiere_effects"]):
            how = "added" if done["added"] else "set on the imported one"
            notes.append(f"{done['effect']} ({how}{': ' + ', '.join(done['set']) if done['set'] else ''})")
    if entry["compare"] == "video":
        frames = int(entry["frames"])
        settle(bridge, seq_id, out_dir)
        if video_preset:
            export_stills_with_preset(bridge, seq_id, out_dir, entry["id"], frames, video_preset)
            notes.append(f"{frames} stills via Media Encoder")
        else:
            export_frames(bridge, seq_id, out_dir, entry["id"], 0, frames)
            notes.append(f"{frames} frames via frame export")
    else:
        if not audio_preset:
            raise Skipped(f"no {WAV_PRESET}.epr preset (Waveform Audio, 48 kHz, 24-bit, saved from Premiere's export dialog) and no --audio-preset")
        export_with_preset(bridge, seq_id, wav_path, audio_preset)
        notes.append("WAV via Media Encoder")
    return "rendered: " + "; ".join(notes)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("manifest", help="manifest.json written by `cargo xtask fidelity`")
    parser.add_argument("--case", action="append", default=[], metavar="ID", help="only this case (repeatable)")
    parser.add_argument("--reference", metavar="DIR", help="render under DIR/<case>/ instead of the manifest's reference root")
    parser.add_argument("--audio-preset", metavar="EPR", help=f"Media Encoder preset for the sound cases (default: {WAV_PRESET}.epr if saved)")
    parser.add_argument("--video-preset", metavar="EPR", help=f"Media Encoder PNG-sequence preset for the picture cases (default: {PNG_PRESET}.epr if saved)")
    parser.add_argument("--frame-export", action="store_true", help="picture cases through Premiere's frame export even when a PNG preset exists")
    parser.add_argument("--bridge-dir", default=BRIDGE_DIR, metavar="DIR", help="the directory the bridge panel watches")
    parser.add_argument("--keep-open", action="store_true", help="leave the project open in Premiere at the end instead of closing it")
    parser.add_argument("--dry-run", action="store_true", help="print the plan; touch nothing")
    args = parser.parse_args(argv)

    with open(args.manifest, encoding="utf-8") as handle:
        manifest = json.load(handle)
    cases = manifest["cases"]
    if args.case:
        known = {c["id"] for c in cases}
        unknown = [c for c in args.case if c not in known]
        if unknown:
            parser.error(f"unknown case(s): {', '.join(unknown)}")
        cases = [c for c in cases if c["id"] in args.case]
    root = os.path.abspath(args.reference) if args.reference else manifest["reference_root"]
    for preset in (args.audio_preset, args.video_preset):
        if preset and not os.path.isfile(preset):
            parser.error(f"preset not found: {preset}")
    audio_preset = args.audio_preset or find_preset(WAV_PRESET)
    video_preset = None if args.frame_export else (args.video_preset or find_preset(PNG_PRESET))

    def out_dir_of(entry):
        return os.path.join(root, entry["id"]) if args.reference else entry["reference_dir"]

    print(f"{len(cases)} cases → {root}")
    print(f"picture: {'Media Encoder, ' + video_preset if video_preset else 'frame export'}; sound: {'Media Encoder, ' + audio_preset if audio_preset else 'skipped (no preset)'}")
    if args.dry_run:
        for entry in cases:
            what = "picture" if entry["compare"] == "video" else "sound"
            fx = "; ".join(f"{e['name']} {json.dumps(e['params'])}" for e in entry["premiere_effects"]) or "XML complete"
            print(f"  {entry['id']}: {what}; {fx}; → {out_dir_of(entry)}")
        return 0

    workdir = os.path.join(root, "_premiere")
    os.makedirs(workdir, exist_ok=True)
    bridge = Bridge(args.bridge_dir)
    ensure_bridge(bridge, workdir)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    project_path = create_project(bridge, os.path.join(workdir, f"fidelity-{stamp}.prproj"))
    print(f"project: {project_path}")

    failures = []
    skipped = []
    for entry in cases:
        started = time.monotonic()
        try:
            summary = render_case(bridge, entry, manifest["settings"], out_dir_of(entry), audio_preset, video_preset)
        except Skipped as error:
            summary = f"skipped: {error}"
            skipped.append(entry["id"])
        except Failure as error:
            summary = f"FAILED: {error}"
            failures.append(entry["id"])
        print(f"  {entry['id']}: {summary} ({time.monotonic() - started:.1f}s)")
        sys.stdout.flush()
    try:
        save_project(bridge)
        if not args.keep_open:
            close_project(bridge)
    except Failure as error:
        print(f"  project: {error}")
    if failures:
        print(f"{len(failures)} of {len(cases)} cases failed: {', '.join(failures)}")
    if skipped:
        print(f"{len(skipped)} of {len(cases)} cases skipped (no preset): {', '.join(skipped)}")
    print("next: cargo xtask fidelity" + (f" --reference {root}" if args.reference else ""))
    return 1 if failures or skipped else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except HostBlocked as error:
        print(f"render.py: {error}; the run stops here (dismiss the dialog in Premiere, then run again)", file=sys.stderr)
        sys.exit(1)
    except Failure as error:
        print(f"render.py: {error}", file=sys.stderr)
        sys.exit(1)
