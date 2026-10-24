#!/usr/bin/env python3
"""Import a folder of clips, auto-correct every clip, export with a named preset.

Drives a running FilmCraft over its control channel (start it with `filmcraft --control 9876`), from
an environment with the same FILMCRAFT_CONTROL_TOKEN as the app (docs/control-protocol.md).
See docs/color.md for what `lumetri.auto` measures and reports.

    python docs/examples/auto_correct_batch.py footage/card01 out/reel.mp4 \
        --preset "Match Source – Adaptive High Bitrate" --frames sampled --min-confidence 0.4 [--raw]

Steps: `file.import {paths}` -> `file.newSequence {fromItems}` -> `lumetri.auto {items, apply: false}`
to measure -> `lumetri.auto {clips}` on the confident clips -> `file.exportMedia {path, preset}` ->
poll `jobs.list` until the export job is finished (a long export outlives the channel's one-minute
request timeout, so the script never waits inside a request).
"""

import argparse
import json
import os
import pathlib
import socket
import sys
import time


def connect(port):
    # every request carries the token the app was started with
    token = os.environ.get("FILMCRAFT_CONTROL_TOKEN")
    if not token:
        sys.exit("FILMCRAFT_CONTROL_TOKEN is not set: export the token FilmCraft was started with (docs/control-protocol.md)")
    sock = socket.create_connection(("127.0.0.1", port))
    stream = sock.makefile("rw", encoding="utf-8")
    counter = [0]

    def call(method, **params):
        counter[0] += 1
        stream.write(json.dumps({"id": counter[0], "token": token, "method": method, "params": params}) + "\n")
        stream.flush()
        reply = json.loads(stream.readline())
        if not reply.get("ok"):
            raise RuntimeError(f"{method}: {reply.get('error')}")
        return reply.get("result")

    return call


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder", help="folder of media files to import")
    ap.add_argument("output", help="export path (its extension should suit the preset)")
    ap.add_argument("--port", type=int, default=9876)
    ap.add_argument("--preset", default="Match Source – Adaptive High Bitrate", help="an export preset name (export.presets.list)")
    ap.add_argument("--frames", choices=["playhead", "sampled"], default="sampled")
    ap.add_argument("--samples", type=int, default=5)
    ap.add_argument("--min-confidence", type=float, default=0.4, help="clips measured below this are left alone")
    ap.add_argument("--raw", action="store_true", help="write RAW development settings where the clip has them")
    ap.add_argument("--no-white-balance", action="store_true")
    ap.add_argument("--no-exposure", action="store_true")
    args = ap.parse_args()

    run = connect(args.port)
    engine = lambda command, **params: run("engine.execute", command=command, params=params)

    paths = sorted(str(p) for p in pathlib.Path(args.folder).iterdir() if p.is_file() and not p.name.startswith("."))
    if not paths:
        sys.exit(f"no files in {args.folder}")
    imported = engine("file.import", paths=paths)
    items = imported["items"]
    print(f"imported {len(items)} item(s)")
    if imported.get("errors"):
        print("skipped:", imported["errors"])

    seq = engine("file.newSequence", name="Auto Correct", fromItems=items)["sequence"]
    print(f"sequence {seq}")

    options = dict(frames=args.frames, samples=args.samples, whiteBalance=not args.no_white_balance, exposure=not args.no_exposure)
    measured = engine("lumetri.auto", items=items, apply=False, **options)
    print(f"{'clip':>6} {'conf':>5} {'temp':>7} {'tint':>7} {'exp':>6}  cast   p50    p99")
    good = []
    for m in measured["items"]:
        a = m["measured"]
        print(f"{m['clip']:>6} {m['confidence']:>5.2f} {a['temperature']:>7.1f} {a['tint']:>7.1f} {a['exposure']:>6.2f}  {a['cast']:.2f}  {a['p50']:.3f}  {a['p99']:.3f}")
        if m["confidence"] >= args.min_confidence:
            good.append(m["clip"])
    # a clip that could not be measured would be exported uncorrected: report it and stop
    measure_errors = measured.get("errors") or []
    for e in measure_errors:
        print(f"{str(e.get('clip')):>6} not measured: {e.get('error')}", file=sys.stderr)
    if measure_errors:
        sys.exit(f"{len(measure_errors)} clip(s) could not be measured; nothing corrected or exported")

    if good:
        result = engine("lumetri.auto", clips=good, into="raw" if args.raw else "lumetri", **options)
        for it in result["items"]:
            note = f"  ({it['note']})" if it.get("note") else ""
            print(f"clip {it['clip']}: set {json.dumps(it.get('set'))}{note}")
        print(f"corrected {result['corrected']} clip(s), left {len(measured['items']) - len(good)} under confidence {args.min_confidence}")
        # a clip that failed would be exported uncorrected: report it and stop before the export
        errors = result.get("errors") or []
        for e in errors:
            print(f"clip {e.get('clip')}: not corrected: {e.get('error')}", file=sys.stderr)
        if errors:
            sys.exit(f"{len(errors)} clip(s) could not be corrected; not exporting")
    else:
        print("no clip reached the confidence threshold; nothing changed")

    presets = [p["name"] for p in engine("export.presets.list")["presets"]]
    if args.preset not in presets:
        sys.exit(f"no export preset {args.preset!r}; known: {presets}")
    job = engine("file.exportMedia", path=args.output, preset=args.preset, sequence=seq, wait=False)
    print(f"export job {job['job']} -> {job['path']}")
    while True:
        jobs = {j["id"]: j for j in engine("jobs.list")}
        j = jobs.get(job["job"])
        if j is None or j.get("finished"):
            break
        print(f"  {j.get('progress', 0) * 100:5.1f}%  {j.get('status', '')}", end="\r")
        time.sleep(2)
    print()
    if j is not None and isinstance(j.get("result"), dict) and j["result"].get("error"):
        sys.exit(f"export failed: {j['result']['error']}")
    print("export finished:", job["path"])
    # the Export Queue (Send to Queue flows) is listed the same way:
    queue = engine("export.queue.list")
    if queue["items"]:
        print("export queue:", [(q["id"], q["status"], q["path"]) for q in queue["items"]])


if __name__ == "__main__":
    main()
