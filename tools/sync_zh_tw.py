#!/usr/bin/env python3
"""Sync the Taiwan Traditional Chinese catalog keys from the Simplified Chinese catalog."""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates" / "ui-egui" / "src" / "i18n" / "zh-cn.tsv"
TARGET = ROOT / "crates" / "ui-egui" / "src" / "i18n" / "zh-tw.tsv"


@dataclass(frozen=True)
class Entry:
    context: str
    source: str
    translation: str

    @property
    def key(self) -> tuple[str, str]:
        return self.context, self.source


@dataclass(frozen=True)
class Catalog:
    header: tuple[str, ...]
    entries: tuple[Entry, ...]


@dataclass(frozen=True)
class SyncResult:
    text: str
    added: int
    removed: int


def unescape(text: str) -> str:
    result: list[str] = []
    chars = iter(text)
    for char in chars:
        if char != "\\":
            result.append(char)
            continue
        escaped = next(chars, None)
        if escaped == "n":
            result.append("\n")
        elif escaped == "t":
            result.append("\t")
        elif escaped == "\\":
            result.append("\\")
        elif escaped is None:
            result.append("\\")
        else:
            result.extend(("\\", escaped))
    return "".join(result)


def escape(text: str) -> str:
    return text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def parse_catalog(text: str, label: str) -> Catalog:
    header: list[str] = []
    entries: list[Entry] = []
    seen: set[tuple[str, str]] = set()
    found_entry = False

    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            if not found_entry:
                header.append(line)
            continue
        if line.startswith("#"):
            if not found_entry:
                header.append(line)
            continue

        columns = line.split("\t")
        if len(columns) != 3:
            raise ValueError(f"{label}:{number}: expected three tab-separated columns")
        entry = Entry(*(unescape(column) for column in columns))
        if not entry.source or not entry.translation:
            raise ValueError(f"{label}:{number}: source and translation must not be empty")
        if entry.key in seen:
            raise ValueError(f"{label}:{number}: duplicate entry {entry.key!r}")
        found_entry = True
        seen.add(entry.key)
        entries.append(entry)

    return Catalog(tuple(header), tuple(entries))


def render_catalog(header: tuple[str, ...], entries: list[Entry]) -> str:
    lines = list(header)
    lines.extend(f"{escape(e.context)}\t{escape(e.source)}\t{escape(e.translation)}" for e in entries)
    return "\n".join(lines) + "\n"


def sync_text(source_text: str, target_text: str, convert: Callable[[str], str] | None) -> SyncResult:
    source = parse_catalog(source_text, "zh-cn.tsv")
    target = parse_catalog(target_text, "zh-tw.tsv")
    translated = {entry.key: entry.translation for entry in target.entries}
    source_keys = {entry.key for entry in source.entries}
    added = sum(entry.key not in translated for entry in source.entries)
    removed = sum(entry.key not in source_keys for entry in target.entries)

    entries = []
    for entry in source.entries:
        translation = translated.get(entry.key)
        if translation is None:
            translation = convert(entry.translation) if convert is not None else ""
        entries.append(Entry(entry.context, entry.source, translation))

    return SyncResult(render_catalog(target.header, entries), added, removed)


def opencc_converter() -> Callable[[str], str]:
    try:
        from opencc import OpenCC
    except ImportError as error:
        raise RuntimeError(
            "OpenCC is required to translate new catalog entries. "
            "Install it with `python -m pip install opencc-python-reimplemented`."
        ) from error

    converter = OpenCC("s2twp")
    return converter.convert


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="check whether zh-tw.tsv has exactly the zh-cn.tsv source keys without modifying files",
    )
    args = parser.parse_args()

    try:
        source_text = SOURCE.read_text(encoding="utf-8")
        target_text = TARGET.read_text(encoding="utf-8")
        result = sync_text(source_text, target_text, None if args.check else opencc_converter())
    except (OSError, RuntimeError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1

    if args.check:
        if result.added or result.removed:
            print(f"zh-tw.tsv is out of sync: {result.added} missing, {result.removed} obsolete entries.")
            print("Run `python tools/sync_zh_tw.py` to update it.")
            return 1
        print(f"zh-tw.tsv is synchronized with {len(parse_catalog(source_text, 'zh-cn.tsv').entries)} source entries.")
        return 0

    if result.added or result.removed:
        TARGET.write_text(result.text, encoding="utf-8", newline="\n")
    print(f"zh-tw.tsv synchronized: {result.added} added, {result.removed} obsolete entries removed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
