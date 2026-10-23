"""Pinned, opt-in reference-model downloads; no production dependency."""

import argparse
import hashlib
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "target" / "rotoscope"
EDGE_REV = "7711e012a30a2402c4eaab637bdb00a521302c91"
EDGE_WEIGHT_REV = "14d7ecc48c656b94e5184519f698cd5386c5a2bf"
EDGE_SHA = "ed2d4850b8792c239689b043c47046ec239b6e808a3d9b6ae676c803fd8780df"
MATTE_REV = "6a0e75d7214b01f4d1163ede0f15b23afbbd480b"
MATTE_SHA = "7bbe217ec9d6945679b6191ccd071006a3975675326312df1ecb5518488049a8"
MATTE_ID = "hustvl/vitmatte-small-distinctions-646"
VENDOR = DATA / "vendor" / "EdgeTAM"


def digest(path):
    """Hash a cached file incrementally with SHA-256."""
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def verify(path, expected):
    """Require an existing cached file to match the pinned checksum."""
    path = Path(path)
    if not path.is_file():
        raise ValueError(
            f"Missing {path.name}. Run ./experiments/rotoscope/run-editor.sh to set up models."
        )
    if digest(path) != expected:
        raise ValueError(
            f"Checksum mismatch for {path}. Remove this cached file and run setup again."
        )
    return path


def attribution(path, title, author, source, license_name, notes="Unmodified download"):
    """Write the license and provenance sidecar for a downloaded model asset."""
    Path(f"{path}.attribution").write_text(
        f"asset: {Path(path).name}\ntitle: {title}\nauthor: {author}\nsource: {source}\n"
        f"license: {license_name}\nadded: 2026-10-13 by @yeahboi233\nnotes: {notes}\n",
        encoding="utf-8",
    )


def model_paths(download=False, quality=False):
    """Resolve hash-verified EdgeTAM and optional ViTMatte files; download only when requested."""
    from huggingface_hub import hf_hub_download, snapshot_download

    cache = DATA / "models" / "hub"
    local_edge = VENDOR / "checkpoints" / "edgetam.pt"
    if local_edge.exists():
        edge = verify(local_edge, EDGE_SHA)
    else:
        edge = verify(
            hf_hub_download(
                "facebook/EdgeTAM",
                "edgetam.pt",
                revision=EDGE_WEIGHT_REV,
                cache_dir=cache,
                local_files_only=not download,
            ),
            EDGE_SHA,
        )
    matte = None
    if quality:
        matte = Path(
            snapshot_download(
                MATTE_ID,
                revision=MATTE_REV,
                cache_dir=cache,
                allow_patterns=[
                    "config.json",
                    "preprocessor_config.json",
                    "model.safetensors",
                    "README.md",
                ],
                local_files_only=not download,
            )
        )
        verify(matte / "model.safetensors", MATTE_SHA)
    return edge, matte


def setup():
    """Fetch pinned source/checkpoints and record their licenses in the ignored model cache."""
    DATA.mkdir(parents=True, exist_ok=True)
    if not VENDOR.exists():
        VENDOR.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                "git",
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                "https://github.com/facebookresearch/EdgeTAM.git",
                str(VENDOR),
            ],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(VENDOR),
                "sparse-checkout",
                "set",
                "--no-cone",
                "/sam2/",
                "/setup.py",
                "/pyproject.toml",
                "/README.md",
                "/LICENSE",
            ],
            check=True,
        )
        subprocess.run(["git", "-C", str(VENDOR), "checkout", EDGE_REV], check=True)
    revision = subprocess.check_output(
        ["git", "-C", str(VENDOR), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != EDGE_REV:
        raise ValueError(
            f"EdgeTAM source must be pinned to {EDGE_REV}; found {revision}."
        )
    subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "-q",
            "--python",
            str(DATA / ".venv" / "bin" / "python"),
            "--no-build-isolation",
            "--no-deps",
            "-e",
            str(VENDOR),
        ],
        env={**os.environ, "SAM2_BUILD_CUDA": "0", "UV_LINK_MODE": "copy"},
        check=True,
    )
    edge, matte = model_paths(download=True, quality=True)
    attribution(
        edge,
        "EdgeTAM checkpoint",
        "Meta Platforms, Inc.",
        "https://huggingface.co/facebook/EdgeTAM",
        "Apache-2.0",
        f"sha256 {EDGE_SHA}",
    )
    attribution(
        matte / "model.safetensors",
        "ViTMatte Small checkpoint",
        "ViTMatte authors",
        f"https://huggingface.co/{MATTE_ID}",
        "Apache-2.0",
        f"sha256 {MATTE_SHA}",
    )
    print("Models verified.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--setup", action="store_true")
    args = parser.parse_args()
    if args.setup:
        setup()
