#!/usr/bin/env python3
"""Render the fidelity corpus in Adobe Premiere Pro, by script.

The Premiere side of ``crates/fidelity`` (``PREMIERE.md``, which ``cargo xtask fidelity`` writes
next to the manifest, is the same protocol for a person): for every case, import the FCP7 XML,
check the sequence settings, apply the effects the XML cannot carry, and export Premiere's render
where the harness looks for it. Drives a running Premiere Pro on this machine through premiere-cli
(the ``premiere`` command, https://github.com/strangeparts/premiere-cli), which talks to Premiere's
own scripting API and nothing else. Maintainer tooling: it never runs in CI, needs no Python
packages, and writes only under the reference root (``plan/premiere/fidelity``, gitignored).

    python3 crates/fidelity/premiere/render.py target/fidelity/manifest.json
    python3 crates/fidelity/premiere/render.py MANIFEST --case cut --case crop
    python3 crates/fidelity/premiere/render.py MANIFEST --audio-preset ~/wav-48k-24.epr
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

Every run makes a fresh ``.prproj`` under ``<reference root>/_premiere/``, holds premiere-cli's
lock (Premiere shows a scrim) while it drives, and saves and closes the project at the end
(``--keep-open`` leaves it open to look at; Premiere's FCP XML import also drops an "FCP Translation
Results" text file per case with issues into ``~/Documents``, which says what it did not translate). Exit status 0 when every selected case rendered,
1 otherwise; a case that fails is reported and the run goes on to the next.
"""

import argparse
import datetime
import glob
import json
import os
import subprocess
import sys
import time

TICKS_PER_SECOND = 254_016_000_000
PRESET_DIRS = ("~/Documents/Adobe/Adobe Media Encoder", "~/Library/Application Support/Adobe/Common/AME")
PNG_PRESET = "fidelity-png"
WAV_PRESET = "fidelity-wav"
LOCK_NAME = "filmcraft fidelity"
SETTLE_SECONDS = 30
AME_TIMEOUT_SECONDS = 900


class Failure(Exception):
    """A step that did not do what it said; the message is for the person."""


def premiere(args, timeout=600, check=True):
    """Run one ``premiere`` command and return its JSON result."""
    cmd = ["premiere", *args, "--compact"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False)
    except FileNotFoundError:
        raise Failure("the `premiere` command is not installed (premiere-cli: `make install`)") from None
    except subprocess.TimeoutExpired:
        raise Failure(f"premiere {' '.join(args[:2])}: no answer in {timeout}s (run `premiere doctor`; do not kill Premiere)") from None
    text = proc.stdout.strip()
    try:
        data = json.loads(text) if text else {}
    except ValueError:
        data = {"raw": text}
    if not isinstance(data, dict):
        data = {"value": data}
    if check and (proc.returncode != 0 or data.get("success") is False):
        detail = data.get("error") or text or proc.stderr.strip()
        raise Failure(f"premiere {' '.join(args[:2])}: exit {proc.returncode}: {detail}")
    return data


def exec_jsx(source, workdir, name):
    """Run ExtendScript in Premiere from a file (quoting-safe for any payload)."""
    path = os.path.join(workdir, f"{name}.jsx")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(source)
    return premiere(["exec", path])


def ensure_bridge():
    """Premiere up and answering, waking it if it is down; never kills it."""
    report = premiere(["doctor"], timeout=120, check=False)
    verdict = report.get("verdict")
    if verdict == "healthy":
        return
    if verdict in ("not_running", "panel_down", None):
        print(f"  premiere: {verdict or 'unknown'}; waking (first launch takes a few minutes)")
        premiere(["bridge", "wake", "--wake_timeout", "400"], timeout=460)
        return
    raise Failure(f"premiere doctor: {verdict}: {report.get('message', '')}")


def sequences():
    return {s["id"]: s for s in premiere(["sequence", "list"]).get("sequences", [])}


def dismiss_translation_report():
    """Premiere answers an FCP XML import with a one-button "Translation Report" dialog (the report
    itself lands in the bin); it blocks the scripting host until someone presses OK."""
    for _ in range(10):
        dialog = premiere(["dialog", "show"], timeout=60, check=False).get("dialog")
        if not dialog:
            return
        if "Translation Report" not in json.dumps(dialog):
            raise Failure(f"a dialog is blocking Premiere: {json.dumps(dialog)[:300]}")
        premiere(["dialog", "click", "--button", "OK"], timeout=60, check=False)
        time.sleep(0.5)


def import_case(entry):
    """Import the case's FCP7 XML; return the id of the sequence it made."""
    before = set(sequences())
    premiere(["project", "import-xml", entry["fcp7_xml"]])
    dismiss_translation_report()
    after = sequences()
    new = [s for sid, s in after.items() if sid not in before]
    if len(new) != 1:
        named = [s for s in new if s.get("name") == entry["id"]]
        if len(named) != 1:
            raise Failure(f"import made {len(new)} sequences, expected one named {entry['id']}")
        new = named
    return new[0]["id"]


def check_settings(seq_id, settings):
    got = premiere(["sequence", "settings", seq_id]).get("settings", {})
    rate = settings["frame_rate"]
    want_timebase = TICKS_PER_SECOND * rate["den"] // rate["num"]
    want_sample = TICKS_PER_SECOND // settings["sample_rate"]
    problems = []
    if (got.get("width"), got.get("height")) != (settings["width"], settings["height"]):
        problems.append(f"frame size {got.get('width')}×{got.get('height')}, want {settings['width']}×{settings['height']}")
    if int(got.get("timebase", 0)) != want_timebase:
        problems.append(f"timebase {got.get('timebase')} ticks, want {want_timebase}")
    sample_ticks = int((got.get("audioSampleRate") or {}).get("ticks", 0))
    if sample_ticks != want_sample:
        problems.append(f"sample period {sample_ticks} ticks, want {want_sample} ({settings['sample_rate']} Hz)")
    if problems:
        raise Failure("sequence settings differ from the corpus: " + "; ".join(problems))


EFFECTS_JSX = r"""
try {
  app.enableQE();
  var seq = app.project.activeSequence;
  if (!seq || String(seq.sequenceID) !== SEQ) return JSON.stringify({ success: false, error: "the active sequence is not the case" });
  if (seq.videoTracks.numTracks < 1 || seq.videoTracks[0].clips.numItems < 1) return JSON.stringify({ success: false, error: "no clip on V1" });
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
  if (!qeClip) return JSON.stringify({ success: false, error: "no QE clip matches the clip on V1" });
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
      if (!fx) return JSON.stringify({ success: false, error: "Premiere has no video effect named " + wanted.name });
      var before = clip.components.numItems;
      qeClip.addVideoEffect(fx);
      if (clip.components.numItems <= before) return JSON.stringify({ success: false, error: "adding " + wanted.name + " made no component" });
      comp = clip.components[clip.components.numItems - 1];
      if (String(comp.displayName) !== wanted.name) return JSON.stringify({ success: false, error: "added " + wanted.name + " but the new component is " + comp.displayName });
      added = true;
    }
    var set = [];
    for (var key in wanted.params) {
      if (!wanted.params.hasOwnProperty(key)) continue;
      var prop = property(comp, key);
      if (!prop) return JSON.stringify({ success: false, error: wanted.name + " has no property " + key });
      prop.setValue(wanted.params[key], true);
      var got = prop.getValue();
      if (String(got) !== String(wanted.params[key])) return JSON.stringify({ success: false, error: wanted.name + " " + key + " reads " + got + " after setting " + wanted.params[key] });
      set.push(key + " = " + got);
    }
    report.push({ effect: wanted.name, added: added, set: set });
  }
  return JSON.stringify({ success: true, effects: report });
} catch (err) {
  return JSON.stringify({ success: false, error: err.toString() });
}
"""


def apply_effects(seq_id, effects, workdir):
    """Apply the case's effects to the clip on V1: set an existing same-named component (the import
    may have made one, e.g. Crop), else add it; every value is read back."""
    source = f"var SEQ = {json.dumps(seq_id)};\nvar EFFECTS = {json.dumps(effects)};\n{EFFECTS_JSX}"
    result = exec_jsx(source, workdir, "effects")
    return result.get("effects", [])


FRAMES_JSX = r"""
try {
  var seq = null;
  for (var i = 0; i < app.project.sequences.numSequences; i++) if (String(app.project.sequences[i].sequenceID) === SEQ) seq = app.project.sequences[i];
  if (!seq) return JSON.stringify({ success: false, error: "sequence not found" });
  app.project.activeSequence = seq;
  app.enableQE();
  var qeSeq = qe.project.getActiveSequence();
  if (!qeSeq || String(qeSeq.name) !== String(seq.name)) return JSON.stringify({ success: false, error: "QE active sequence is " + (qeSeq ? qeSeq.name : "none") });
  var tb = parseInt(String(seq.timebase), 10);
  for (var f = FIRST; f < FIRST + COUNT; f++) {
    seq.setPlayerPosition(String(tb * f));
    var tc = String(qeSeq.CTI.timecode);
    var n = String(f);
    while (n.length < 3) n = "0" + n;
    qeSeq.exportFramePNG(tc, OUT_DIR + "/" + STEM + n);
  }
  return JSON.stringify({ success: true, first: FIRST, count: COUNT });
} catch (err) {
  return JSON.stringify({ success: false, error: err.toString() });
}
"""


def frame_path(out_dir, stem, index):
    return os.path.join(out_dir, f"{stem}{index:03d}.png")


def export_frames(seq_id, out_dir, stem, first, count, workdir):
    source = (
        f"var SEQ = {json.dumps(seq_id)};\nvar OUT_DIR = {json.dumps(out_dir)};\nvar STEM = {json.dumps(stem)};\n"
        f"var FIRST = {first};\nvar COUNT = {count};\n{FRAMES_JSX}"
    )
    exec_jsx(source, workdir, "frames")
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


def settle(seq_id, out_dir, workdir):
    """Wait until frame 0 renders the same twice: a freshly imported clip can still be linking or
    conforming, and Premiere renders a placeholder card meanwhile."""
    deadline = time.monotonic() + SETTLE_SECONDS
    previous = None
    while True:
        export_frames(seq_id, out_dir, "_settle", 0, 1, workdir)
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
    for name in os.listdir(out_dir):
        if name.startswith(stem) and name.endswith(".png") and name[len(stem) : -4].isdigit():
            os.remove(os.path.join(out_dir, name))


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


def find_preset(name):
    """The newest `<name>.epr` among the Media Encoder preset folders, or None."""
    hits = [h for d in PRESET_DIRS for h in glob.glob(os.path.join(os.path.expanduser(d), "*", "Presets", name + ".epr"))]
    return max(hits, key=os.path.getmtime) if hits else None


def export_with_preset(seq_id, out_path, preset):
    premiere(["export", "sequence", seq_id, out_path, "--preset", preset, "--range", "entire"])
    wait_for_file(out_path, AME_TIMEOUT_SECONDS)


def stills_of(out_dir, stem):
    return sorted(n for n in os.listdir(out_dir) if n.startswith(stem) and n.endswith(".png") and n[len(stem) : -4].strip("_-.").isdigit())


def export_stills_with_preset(seq_id, out_dir, stem, frames, preset):
    """A PNG sequence through Media Encoder: done when every frame is there and the last one has
    stopped growing (Media Encoder numbers the files itself)."""
    premiere(["export", "sequence", seq_id, os.path.join(out_dir, f"{stem}.png"), "--preset", preset, "--range", "entire"])
    deadline = time.monotonic() + AME_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        names = stills_of(out_dir, stem)
        if len(names) >= frames:
            wait_for_file(os.path.join(out_dir, names[-1]), 60)
            if len(names) > frames:
                raise Failure(f"Media Encoder wrote {len(names)} stills, expected {frames}")
            return
        time.sleep(1.0)
    raise Failure(f"Media Encoder wrote {len(stills_of(out_dir, stem))} of {frames} stills in {AME_TIMEOUT_SECONDS}s")


def render_case(entry, settings, workdir, out_dir, audio_preset, video_preset):
    """Everything for one case; returns a one-line summary."""
    os.makedirs(out_dir, exist_ok=True)
    seq_id = import_case(entry)
    check_settings(seq_id, settings)
    premiere(["sequence", "set-active", seq_id])
    notes = []
    if entry["premiere_effects"]:
        for done in apply_effects(seq_id, entry["premiere_effects"], workdir):
            how = "added" if done["added"] else "set on the imported one"
            notes.append(f"{done['effect']} ({how}{': ' + ', '.join(done['set']) if done['set'] else ''})")
    if entry["compare"] == "video":
        frames = int(entry["frames"])
        clear_stills(out_dir, entry["id"])
        settle(seq_id, out_dir, workdir)
        if video_preset:
            export_stills_with_preset(seq_id, out_dir, entry["id"], frames, video_preset)
            notes.append(f"{frames} stills via Media Encoder")
        else:
            export_frames(seq_id, out_dir, entry["id"], 0, frames, workdir)
            notes.append(f"{frames} frames via frame export")
    else:
        if not audio_preset:
            return f"skipped: no {WAV_PRESET}.epr preset (Waveform Audio, 48 kHz, 24-bit, saved from Premiere's export dialog) and no --audio-preset"
        out_path = os.path.join(out_dir, f"{entry['id']}.wav")
        if os.path.exists(out_path):
            os.remove(out_path)
        export_with_preset(seq_id, out_path, audio_preset)
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
    ensure_bridge()
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    project = premiere(["project", "create", f"fidelity-{stamp}", workdir])
    project_path = project.get("projectPath") or os.path.join(workdir, f"fidelity-{stamp}.prproj")
    print(f"project: {project_path}")
    lock = premiere(["lock", "--lock_project", project_path, "--lock_name", LOCK_NAME, "--lock_timeout", "1800"])
    token = lock.get("token")
    if token:
        os.environ["PREMIERE_CLI_LOCK_TOKEN"] = token

    failures = []
    try:
        for entry in cases:
            started = time.monotonic()
            try:
                summary = render_case(entry, manifest["settings"], workdir, out_dir_of(entry), audio_preset, video_preset)
            except Failure as error:
                summary = f"FAILED: {error}"
                failures.append(entry["id"])
            print(f"  {entry['id']}: {summary} ({time.monotonic() - started:.1f}s)")
            sys.stdout.flush()
        premiere(["project", "save"], check=False)
        if not args.keep_open:
            # closeDocument(saveFirst = 0, promptIfDirty = 0): the save above is the record; no dialog can
            # block the host. Premiere may then sit with no project, which premiere-cli wakes next time.
            exec_jsx("try { app.project.closeDocument(0, 0); return JSON.stringify({ success: true }); } catch (err) { return JSON.stringify({ success: false, error: err.toString() }); }", workdir, "close")
    finally:
        if token:
            premiere(["unlock", token], check=False)
    if failures:
        print(f"{len(failures)} of {len(cases)} cases failed: {', '.join(failures)}")
    print("next: cargo xtask fidelity" + (f" --reference {root}" if args.reference else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Failure as error:
        print(f"render.py: {error}", file=sys.stderr)
        sys.exit(1)
