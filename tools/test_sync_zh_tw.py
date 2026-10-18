import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("sync_zh_tw.py")
SPEC = importlib.util.spec_from_file_location("sync_zh_tw", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
SYNC = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SYNC
SPEC.loader.exec_module(SYNC)


class SyncTraditionalChineseTests(unittest.TestCase):
    def test_adds_new_keys_and_preserves_curated_translations(self):
        source = "# source\n\tFile\t文件\n\tNew Label\t新标签\n"
        target = "# Traditional Chinese\n\tFile\t檔案\n\tRemoved Label\t舊標籤\n"

        result = SYNC.sync_text(source, target, lambda text: text.replace("标签", "標籤"))

        self.assertEqual((result.added, result.removed), (1, 1))
        entries = SYNC.parse_catalog(result.text, "result").entries
        self.assertEqual(
            [(entry.source, entry.translation) for entry in entries],
            [("File", "檔案"), ("New Label", "新標籤")],
        )

    def test_check_does_not_require_converter_or_modify_existing_entries(self):
        source = "\tFile\t文件\n"
        target = "# Traditional Chinese\n\tFile\t檔案\n"

        result = SYNC.sync_text(source, target, None)

        self.assertEqual((result.added, result.removed), (0, 0))
        self.assertEqual(SYNC.parse_catalog(result.text, "result").entries[0].translation, "檔案")

    def test_escapes_tabs_backslashes_and_newlines(self):
        entry = SYNC.Entry("", "tab\tkey", "path\\name\nsecond line")

        parsed = SYNC.parse_catalog(SYNC.render_catalog(("# Header",), [entry]), "round-trip")

        self.assertEqual(parsed.entries, (entry,))

    def test_rejects_duplicate_keys(self):
        with self.assertRaisesRegex(ValueError, "duplicate entry"):
            SYNC.parse_catalog("\tFile\t檔案\n\tFile\t文件\n", "duplicate")


if __name__ == "__main__":
    unittest.main()
