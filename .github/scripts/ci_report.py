"""Report the actual quality gate and GPU execution evidence; never treat skips as parity."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

GPU_PROOF = re.compile('GPU vs CPU p99')
GPU_SKIP = re.compile(r"skipp", re.I)
GPU_CONTEXT = re.compile(r"GPU|no\s+(?:GPU\s+)?adapter|no device", re.I)


def report(text, adapter_outcome, quality_outcome):
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    totals = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored", text)
    counts = [sum(int(row[i]) for row in totals) for i in range(3)]
    skips = [line for line in text.splitlines() if "ignored" not in line.lower() and GPU_SKIP.search(line) and GPU_CONTEXT.search(line)]
    problems = []
    if adapter_outcome != "success":
        problems.append("Vulkan software-adapter probe did not succeed")
    if quality_outcome != "success":
        problems.append("Full cargo xtask ci did not pass")
    if counts[0] == 0:
        problems.append("No executed Rust tests were recorded")
    if not GPU_PROOF.search(text):
        problems.append("No successful GPU execution evidence was recorded")
    if skips:
        problems.append("GPU tests skipped at runtime; this is not GPU parity acceptance")
    lines = ["## Source PR quality gate", "", f"- Adapter probe: {adapter_outcome}", f"- Full gate: {quality_outcome}", f"- Printed Rust totals: {counts[0]} passed, {counts[1]} failed, {counts[2]} ignored", f"- GPU runtime-skip messages: {len(skips)}"]
    lines += ["- " + problem for problem in problems]
    if skips:
        lines += ["", "```text", *skips[:20], "```"]
    lines += ["", "Counts come from printed Rust summaries; complete logs and source identity are retained as artifacts.", "Software Vulkan coverage does not qualify physical GPU drivers or other operating systems."]
    return (1 if problems else 0), "\n".join(lines) + "\n"


def main():
    root = Path(os.environ["RUNNER_TEMP"])
    if len(sys.argv) > 1 and sys.argv[1] == "record":
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        pr = event.get("pull_request", {})
        identity = {"event": os.environ.get("GITHUB_EVENT_NAME"), "action": event.get("action"), "pull_request": event.get("number"), "head_sha": pr.get("head", {}).get("sha"), "github_sha": os.environ.get("GITHUB_SHA"), "checkout_sha": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()}
        (root / "source-identity.json").write_text(json.dumps(identity, indent=2))
        print(json.dumps(identity, indent=2))
        return 0
    log = root / "quality-gate.log"
    code, summary = report(log.read_text(errors="replace") if log.exists() else "", os.environ.get("ADAPTER_OUTCOME", "missing"), os.environ.get("QUALITY_OUTCOME", "missing"))
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as stream:
        stream.write(summary)
    print(summary)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
