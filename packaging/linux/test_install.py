#!/usr/bin/env python3
"""Exercise the tarball installer with disposable binaries and installation directories."""

import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time
import unittest


HERE = Path(__file__).resolve().parent
APP_ID = "ai.storyteller.filmcraft"


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="filmcraft-install-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / "extracted bundle"
        (self.bundle / "bin").mkdir(parents=True)
        self.share = self.bundle / "share"
        (self.share / "applications").mkdir(parents=True)
        shutil.copyfile(HERE / "install.sh", self.bundle / "install.sh")
        shutil.copyfile(
            HERE / f"{APP_ID}.desktop", self.share / "applications" / f"{APP_ID}.desktop"
        )
        for name in ("filmcraft", "filmcraft-cli"):
            (self.bundle / "bin" / name).write_text(
                '#!/bin/sh\nprintf \'%s\\n\' "$0" "$@" > "$(dirname "$0")/launched"\n'
            )
        for path in ("doc/filmcraft/LICENSE-MIT", "icons/hicolor/test", "mime/packages/test.xml"):
            target = self.share / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("fixture\n")

    def install(self, *args):
        # Run outside the extracted directory to check that the script finds its own files.
        return subprocess.run(
            ["bash", str(self.bundle / "install.sh"), *map(str, args)],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )

    def test_install_and_update_preserve_other_applications(self):
        prefix = self.root / "user programs"
        other = prefix / "share/applications/other.desktop"
        other.parent.mkdir(parents=True)
        other.write_text("unrelated\n")
        for version in ("first", "updated"):
            (self.bundle / "share/doc/filmcraft/LICENSE-MIT").write_text(version)
            result = self.install("--prefix", prefix)
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in ("filmcraft", "filmcraft-cli"):
                self.assertTrue(os.access(prefix / "bin" / name, os.X_OK))
                self.assertEqual(
                    (prefix / "bin" / name).read_bytes(), (self.bundle / "bin" / name).read_bytes()
                )
            self.assertEqual((prefix / "share/doc/filmcraft/LICENSE-MIT").read_text(), version)
            self.assertTrue((prefix / "share/icons/hicolor/test").is_file())
            self.assertTrue((prefix / "share/mime/packages/test.xml").is_file())
            self.assertEqual(other.read_text(), "unrelated\n")
        desktop = prefix / "share/applications" / f"{APP_ID}.desktop"
        self.assertIn(f'Exec="{prefix}/bin/filmcraft" %F\n', desktop.read_text())
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", str(desktop)], check=True)

    @unittest.skipUnless(shutil.which("gio"), "gio is needed to exercise the desktop launcher")
    def test_desktop_launch_escapes_paths_and_preserves_file_arguments(self):
        prefix = self.root / 'programs with "quotes" $dollar `tick` \\slash & more'
        result = self.install("--prefix", prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        document = self.root / "my film.filmcraft"
        document.touch()
        desktop = prefix / "share/applications" / f"{APP_ID}.desktop"
        subprocess.run(["gio", "launch", str(desktop), str(document)], check=True, timeout=10)
        launched = prefix / "bin/launched"
        for _ in range(100):
            if launched.exists() and launched.read_text().splitlines() == [
                str(prefix / "bin/filmcraft"), str(document)
            ]:
                break
            time.sleep(0.01)
        self.assertEqual(launched.read_text().splitlines(), [str(prefix / "bin/filmcraft"), str(document)])

    def test_a_nightly_bundle_keeps_an_installed_stable_tree(self):
        prefix = self.root / "shared prefix"
        stable = self.install("--prefix", prefix)
        self.assertEqual(stable.returncode, 0, stable.stderr)
        owned = {
            path: path.read_bytes()
            for path in (
                prefix / "bin/filmcraft",
                prefix / "bin/filmcraft-cli",
                prefix / "share/applications" / f"{APP_ID}.desktop",
                prefix / "share/icons/hicolor/test",
                prefix / "share/mime/packages/test.xml",
            )
        }
        nightly = self.root / "nightly bundle"
        for path in ("bin", "share/applications", "share/icons/hicolor/256x256/apps", "share/doc/filmcraft"):
            (nightly / path).mkdir(parents=True)
        shutil.copyfile(HERE / "install.sh", nightly / "install.sh")
        for name in ("filmcraft", "filmcraft-bin", "filmcraft-cli"):
            binary = nightly / "bin" / name
            binary.write_text("#!/bin/sh\necho 'nightly fixture'\n")
            binary.chmod(0o755)
        shutil.copyfile(
            HERE / f"{APP_ID}.desktop", nightly / "share/applications" / f"{APP_ID}.desktop"
        )
        (nightly / "share/icons/hicolor/256x256/apps" / f"{APP_ID}.png").write_bytes(b"nightly icon")
        (nightly / "share/doc/filmcraft/NOTICE").write_text("nightly notice\n")
        result = subprocess.run(
            ["bash", str(nightly / "install.sh"), "--prefix", str(prefix)],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        for path, content in owned.items():
            self.assertEqual(path.read_bytes(), content, f"{path.name} belongs to the Stable install")
        for name in ("filmcraft-nightly", "filmcraft-bin", "filmcraft-nightly-cli"):
            self.assertTrue(os.access(prefix / "bin" / name, os.X_OK), name)
        entry = prefix / "share/applications" / f"{APP_ID}.nightly-v1.desktop"
        text = entry.read_text()
        self.assertIn(f'Exec="{prefix}/bin/filmcraft-nightly" %F\n', text)
        self.assertIn("Name=FilmCraft Nightly\n", text)
        self.assertIn(f"Icon={APP_ID}.nightly-v1\n", text)
        self.assertNotIn("MimeType=", text, "a Nightly entry claims no file associations")
        self.assertTrue((prefix / "share/icons/hicolor/256x256/apps" / f"{APP_ID}.nightly-v1.png").is_file())
        self.assertEqual((prefix / "share/doc/filmcraft-nightly/NOTICE").read_text(), "nightly notice\n")

    def test_the_nightly_tarball_stages_what_the_installer_selects(self):
        version = "0.0.1-nightly.20251124.1.1.g0123456789ab"
        target = self.root / "nightly-target"
        binaries = target / "release"
        binaries.mkdir(parents=True)
        for name in ("filmcraft", "filmcraft-cli"):
            binary = binaries / name
            binary.write_text("#!/bin/sh\necho 'FilmCraft fixture'\n")
            binary.chmod(0o755)
        dist = self.root / "nightly-dist"
        result = subprocess.run(
            ["bash", str(HERE / "package.sh"), "--skip-build", "--formats", "tar"],
            env={**os.environ, "CARGO_TARGET_DIR": str(target), "DIST": str(dist),
                 "FILMCRAFT_BUILD_CHANNEL": "nightly-v1", "FILMCRAFT_VERSION": version},
            capture_output=True, text=True, timeout=120,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        base = f"filmcraft-{version}-linux-x86_64"
        with tarfile.open(dist / f"{base}.tar.gz") as archive:
            names = set(archive.getnames())
        # The installer picks the Nightly layout from the GUI beside the launcher, and rewrites the
        # Stable-named entry it stages into the Nightly's own id.
        self.assertIn(f"{base}/bin/filmcraft", names)
        self.assertIn(f"{base}/bin/filmcraft-bin", names)
        self.assertIn(f"{base}/share/applications/{APP_ID}.desktop", names)

    def test_invalid_arguments_fail_before_writing(self):
        for args in (("--prefix",), ("--prefix", "relative"), ("--prefix", "/"), ("--unknown",),
                     ("--prefix", "///"), ("--prefix", tempfile.gettempdir() + "/../"),
                     ("--prefix", str(self.bundle)),
                     ("--prefix", str(self.bundle / "nested")),
                     ("--prefix", str(self.root / "bad=path")),
                     ("--prefix", str(self.root / "bad%path")),
                     ("--prefix", str(self.root / "bad\npath"))):
            with self.subTest(args=args):
                self.assertNotEqual(self.install(*args).returncode, 0)

    def test_incomplete_bundle_does_not_install(self):
        (self.bundle / "bin/filmcraft-cli").unlink()
        prefix = self.root / "destination"
        result = self.install("--prefix", prefix)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("extract the complete tarball", result.stderr)
        self.assertFalse(prefix.exists())

    def test_help(self):
        self.assertEqual(self.install("--help").returncode, 0)

    def test_release_tarball_includes_executable_installer(self):
        target = self.root / "target"
        binaries = target / "release"
        binaries.mkdir(parents=True)
        for name in ("filmcraft", "filmcraft-cli"):
            binary = binaries / name
            binary.write_text("#!/bin/sh\necho 'FilmCraft fixture'\n")
            binary.chmod(0o755)
        dist = self.root / "dist"
        result = subprocess.run(
            ["bash", str(HERE / "package.sh"), "--skip-build", "--formats", "tar"],
            env={**os.environ, "CARGO_TARGET_DIR": str(target), "DIST": str(dist)},
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        archives = list(dist.glob("*.tar.gz"))
        self.assertEqual(len(archives), 1)
        with tarfile.open(archives[0]) as archive:
            installer = next(m for m in archive.getmembers() if m.name.endswith("/install.sh"))
            self.assertEqual(installer.mode & 0o777, 0o755)
            self.assertEqual(archive.extractfile(installer).read(), (HERE / "install.sh").read_bytes())


if __name__ == "__main__":
    unittest.main()
