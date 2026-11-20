"""Prepare the Android creator font pack outside the source checkout."""

from hashlib import sha256
from pathlib import Path
import sys
from urllib.request import urlopen


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: prepare-fonts.py OUTPUT_DIRECTORY")
    root = Path(sys.argv[1]).resolve()
    manifest = Path(__file__).with_name("creator-fonts.txt").read_text()
    for line in manifest.splitlines():
        if not line or line.startswith("#"):
            continue
        _, _, font_name, _, _, license_name, checksum, url = line.split(" | ")
        font = root / font_name
        license_file = root / license_name
        with urlopen(f"{url}/{font.name}", timeout=30) as response:
            font_bytes = response.read()
        if sha256(font_bytes).hexdigest() != checksum:
            raise ValueError(f"Checksum mismatch for {font.name}")
        with urlopen(f"{url}/{license_file.name}", timeout=30) as response:
            license_bytes = response.read()
        font.parent.mkdir(parents=True, exist_ok=True)
        font.write_bytes(font_bytes)
        license_file.write_bytes(license_bytes)
    (root / "fonts/manifest.txt").write_text(manifest)


if __name__ == "__main__":
    main()
