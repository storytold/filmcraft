"""Headless inference and benchmark command."""

import argparse
import json
import sys

from media import load_clip
from pipeline import Runtime, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--video", required=True)
    parser.add_argument(
        "--point",
        action="append",
        default=[],
        help="Original-video x,y,label; repeatable",
    )
    parser.add_argument("--box", help="Original-video x0,y0,x1,y1")
    parser.add_argument("--seed-frame", type=int, default=0)
    parser.add_argument("--mode", choices=["fast", "quality"], default="fast")
    parser.add_argument(
        "--device", choices=["auto", "cpu", "mps", "cuda"], default="auto"
    )
    parser.add_argument("--max-frames", type=int, default=30)
    parser.add_argument("--max-side", type=int, default=720)
    parser.add_argument("--band", type=int, default=8)
    args = parser.parse_args()
    try:
        clip = load_clip(args.video, args.max_frames, args.max_side)
        sx, sy = clip.width / clip.source_width, clip.height / clip.source_height
        points = []
        for value in args.point:
            parts = [float(x) for x in value.split(",")]
            if len(parts) != 3:
                raise ValueError("--point requires x,y,label.")
            points.append([parts[0] * sx, parts[1] * sy, parts[2]])
        box = [float(x) for x in args.box.split(",")] if args.box else None
        if box is not None:
            if len(box) != 4:
                raise ValueError("--box requires x0,y0,x1,y1.")
            box = [x * (sx if i % 2 == 0 else sy) for i, x in enumerate(box)]
        directory, metrics = run(
            clip,
            [{"frame": args.seed_frame, "points": points, "box": box}],
            Runtime(args.device),
            mode=args.mode,
            band=args.band,
        )
        print(json.dumps({"output": str(directory), "metrics": metrics}, indent=2))
        return 0
    except (ValueError, OSError, RuntimeError) as error:
        print(f"Rotoscope test: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
