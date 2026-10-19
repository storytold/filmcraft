"""Compare native CPU thread budgets without timing cached results as fresh inference.

Run on the benchmark host after building the inference-enabled `oracle` example.
The fixture format is documented in crates/rotoscope/README.md. Model weights,
fixtures and outputs belong outside Git. No third-party Python packages needed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    """Alternate fresh native runs across thread budgets and record timings plus cached-output identity."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    parser.add_argument("--models", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--threads", type=int, nargs="+", default=[4, 8])
    parser.add_argument("--tile-workers", type=int, choices=range(1, 5))
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10 or any(not 1 <= n <= 64 for n in args.threads):
        parser.error("Use 1..10 repeats and 1..64 threads")
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for repeat in range(args.repeats):
        # Alternate ordering so thermal/load drift does not always favor one budget.
        budgets = args.threads if repeat % 2 == 0 else args.threads[::-1]
        for threads in budgets:
            prefix = args.output / f"run-{repeat}-threads-{threads}"
            fresh = prefix.with_suffix(".fresh.json")
            cached = prefix.with_suffix(".cached.json")
            command = [
                str(args.oracle.resolve()),
                str(args.models.resolve()),
                str(args.fixture.resolve()),
                str(fresh.resolve()),
                str(args.fixture.resolve()),
                str(cached.resolve()),
            ]
            environment = dict(os.environ, FILMCRAFT_AI_ROTO_THREADS=str(threads))
            if args.tile_workers is not None:
                environment["FILMCRAFT_AI_ROTO_TILE_WORKERS"] = str(args.tile_workers)
            started = time.monotonic()
            events = []
            with prefix.with_suffix(".stderr.log").open("w") as errors:
                process = subprocess.Popen(
                    command,
                    stdout=subprocess.PIPE,
                    stderr=errors,
                    text=True,
                    env=environment,
                )
                for line in process.stdout:
                    event = {
                        "seconds": time.monotonic() - started,
                        "message": line.strip(),
                    }
                    events.append(event)
                    print(threads, event, flush=True)
                code = process.wait()
            result = {
                "repeat": repeat,
                "threads": threads,
                "tile_workers": environment.get(
                    "FILMCRAFT_AI_ROTO_TILE_WORKERS", "default"
                ),
                "exit": code,
                "wall_seconds": time.monotonic() - started,
                "events": events,
                "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
            }
            if code == 0:
                result["fresh_sha256"] = hashlib.sha256(fresh.read_bytes()).hexdigest()
                result["cached_identical"] = fresh.read_bytes() == cached.read_bytes()
            results.append(result)
            (args.output / "results.json").write_text(
                json.dumps(results, indent=2) + "\n"
            )
            if code:
                raise SystemExit(
                    f"Oracle failed; see {prefix.with_suffix('.stderr.log')}"
                )
            if not result["cached_identical"]:
                raise SystemExit("Cached alpha differs from fresh inference")


if __name__ == "__main__":
    main()
