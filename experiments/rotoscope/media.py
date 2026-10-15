"""Bounded reference-test decoding and lossless alpha artifacts."""

import hashlib
import json
import math
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path

import numpy as np
from models import DATA, DOG_PAGE, DOG_SHA, attribution, digest
from PIL import Image


def command(args, timeout=120):
    try:
        return subprocess.run(
            args, check=True, capture_output=True, text=True, timeout=timeout
        ).stdout
    except FileNotFoundError as error:
        raise ValueError(f"Install the external test tool {args[0]} first.") from error
    except subprocess.CalledProcessError as error:
        raise ValueError(
            f"{args[0]} could not process this file: {error.stderr[-1200:]}"
        ) from error
    except subprocess.TimeoutExpired as error:
        raise ValueError(
            f"{args[0]} exceeded the {timeout}s test limit. Use a shorter clip."
        ) from error


def key_for(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, allow_nan=False).encode()
    ).hexdigest()[:24]


@dataclass(frozen=True)
class Clip:
    directory: Path
    source: Path
    sha: str
    count: int
    width: int
    height: int
    source_width: int
    source_height: int
    fps: str

    def frame(self, index):
        if not 0 <= index < self.count:
            raise ValueError(f"Frame must be between 0 and {self.count - 1}.")
        with Image.open(self.directory / f"{index:05d}.jpg") as image:
            return np.asarray(image.convert("RGB"))


def load_clip(path, max_frames=30, max_side=720):
    path = Path(path or "").resolve()
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError("Choose a readable, nonempty video first.")
    if path.stat().st_size > 2 * 1024**3:
        raise ValueError(
            "This experiment accepts files up to 2 GiB. Trim the clip first."
        )
    if not isinstance(max_frames, int) or not 1 <= max_frames <= 120:
        raise ValueError("Frame limit must be an integer from 1 to 120.")
    if not isinstance(max_side, int) or not 64 <= max_side <= 3840:
        raise ValueError("Longest side must be an integer from 64 to 3840 pixels.")
    info = json.loads(
        command(
            [
                "ffprobe",
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,avg_frame_rate",
                "-of",
                "json",
                str(path),
            ],
            timeout=30,
        )
    )
    streams = info.get("streams", [])
    if not streams:
        raise ValueError("The file has no readable video stream.")
    stream = streams[0]
    width, height = int(stream.get("width", 0)), int(stream.get("height", 0))
    if min(width, height) <= 0 or max(width, height) > 16384:
        raise ValueError("Unsupported source dimensions (maximum side: 16384 pixels).")
    try:
        fps = Fraction(stream["avg_frame_rate"])
        if not 0 < fps <= 240:
            raise ValueError()
    except (KeyError, ValueError, ZeroDivisionError) as error:
        raise ValueError(
            "A valid constant frame rate between 0 and 240 fps is required."
        ) from error
    factor = min(1.0, max_side / max(width, height))
    out_w = max(2, int(width * factor) // 2 * 2)
    out_h = max(2, int(height * factor) // 2 * 2)
    if max_frames * out_w * out_h > 150_000_000:
        raise ValueError(
            "This clip exceeds the 150-megapixel test budget. Reduce frames or resolution."
        )
    sha = digest(path)
    key = key_for({"source": sha, "frames": max_frames, "side": max_side, "decoder": 1})
    destination = DATA / "frames" / key
    manifest = destination / "clip.json"
    if not manifest.exists():
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = Path(tempfile.mkdtemp(prefix="decode-", dir=destination.parent))
        try:
            command(
                [
                    "ffmpeg",
                    "-v",
                    "error",
                    "-nostdin",
                    "-noautorotate",
                    "-i",
                    str(path),
                    "-map",
                    "0:v:0",
                    "-vf",
                    f"scale={out_w}:{out_h}",
                    "-frames:v",
                    str(max_frames),
                    "-q:v",
                    "2",
                    "-start_number",
                    "0",
                    str(temporary / "%05d.jpg"),
                ]
            )
            count = len(list(temporary.glob("*.jpg")))
            if count == 0:
                raise ValueError("The decoder returned no frames.")
            (temporary / "clip.json").write_text(
                json.dumps({"count": count}), encoding="utf-8"
            )
            if destination.exists():
                shutil.rmtree(destination)
            temporary.replace(destination)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
    count = int(json.loads(manifest.read_text())["count"])
    return Clip(destination, path, sha, count, out_w, out_h, width, height, str(fps))


def checker(height, width):
    y, x = np.indices((height, width))
    gray = np.where((x // 20 + y // 20) % 2, 72, 106).astype(np.uint8)
    return np.repeat(gray[..., None], 3, axis=2)


def composite(rgb, alpha, background=None):
    alpha = np.asarray(alpha, dtype=np.float32)
    if alpha.shape != rgb.shape[:2] or not np.isfinite(alpha).all():
        raise ValueError("Alpha must be finite and match the image dimensions.")
    if background is None:
        background = checker(*alpha.shape)
    a = np.clip(alpha, 0, 1)[..., None]
    return np.rint(rgb * a + background * (1 - a)).clip(0, 255).astype(np.uint8)


def read_alpha(path):
    with Image.open(path) as image:
        return np.asarray(image).astype(np.float32) / 65535


def write_frame(directory, index, rgb, mask, alpha):
    if not np.isfinite(alpha).all() or alpha.shape != rgb.shape[:2]:
        raise ValueError("Model returned invalid alpha.")
    name = f"{index:05d}.png"
    Image.fromarray((mask * 255).astype(np.uint8)).save(directory / "masks" / name)
    Image.fromarray(np.rint(np.clip(alpha, 0, 1) * 65535).astype(np.uint16)).save(
        directory / "alpha" / name
    )
    rgba = np.dstack([rgb, np.rint(np.clip(alpha, 0, 1) * 255).astype(np.uint8)])
    Image.fromarray(rgba).save(directory / "rgba" / name)
    Image.fromarray(composite(rgb, alpha)).save(directory / "preview" / name)
    tint = rgb.astype(np.float32).copy()
    tint[mask] = tint[mask] * 0.6 + np.array([65, 210, 185]) * 0.4
    Image.fromarray(tint.astype(np.uint8)).save(directory / "overlay" / name)


def previews(directory, fps):
    for name in ["preview", "overlay"]:
        command(
            [
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-framerate",
                str(fps),
                "-start_number",
                "0",
                "-i",
                str(directory / name / "%05d.png"),
                "-an",
                "-c:v",
                "libx264",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
                str(directory / f"{name}.mp4"),
            ]
        )


def annotate_outputs(directory, clip):
    if clip.sha == DOG_SHA:
        author, source, license_name = "Amada44", DOG_PAGE, "CC-BY-SA-3.0"
    else:
        author, source, license_name = (
            "Local clip owner",
            str(clip.source),
            "Local-use-only",
        )
    for extension in ("*.png", "*.mp4", "*.zip"):
        for path in directory.rglob(extension):
            attribution(
                path,
                "Rotoscope test output",
                author,
                source,
                license_name,
                "Trimmed/scaled source with inferred mask or alpha; generated locally.",
            )


def finite_number(value, name):
    try:
        number = float(value)
    except (TypeError, ValueError) as error:
        raise ValueError(f"{name} must be a finite number.") from error
    if not math.isfinite(number):
        raise ValueError(f"{name} must be finite.")
    return number
