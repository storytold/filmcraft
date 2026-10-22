# Localization parity

> **Last reviewed:** 2026-10-21 (catalog/source audit) · **Last updated:** 2026-10-21 · **Change:** minor (recount all catalogs with one denominator; correct Korean and Russian status) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

Per-language status of FilmCraft's interface. Premiere 26.5.2 ships 10 interface languages (the
installed bundle's `.lproj` folders): German, English, Spanish, French, Italian, Japanese, Korean,
Portuguese (Brazil), Russian, Simplified Chinese.

## How it works

Edit ▸ Language (also Settings ▸ General, `general.interfaceLanguage`) switches the interface and
persists across restarts; System Language (the default) follows the OS's or browser's preferred
languages and falls back to English. Commands `app.language.<english|japanese|spanish|portuguese|ukrainian|chinese|german|russian|french|indonesian|italian|korean>`
are reachable over the control channel. One catalog per language,
`crates/ui-egui/src/i18n/<code>.tsv` (format in the header of `es.tsv`); UI code wraps strings in
`tl!("…")` / `tlf!("…{name}", name)`, and registry names (effects and their parameters, settings,
commands, panels, workspaces, presets) are translated where they are drawn. Searches match the
displayed labels as well as English sources. Tests fail when a `tl!` literal, a menu label or a
registry name has no Spanish entry; `zh-cn.tsv` must also cover every `tl!` literal and menu
label; the other near-complete catalogs have small key-set differences (see the counts below),
and whole-catalog equality is not required by these tests
(French and German must cover the core menus, Brazilian Portuguese every menu label, and neither may keep an
entry without a Spanish one). Engine error messages, the
control channel, the CLI and MCP stay English.

Fonts come from [craft-fonts](https://github.com/storytold/craft-fonts) (`CRAFT_FONTS_DIR`, all
official releases) or an installed system face; without one, switching to Japanese or Chinese is
refused with a message. Simplified Chinese is picked for `zh`, `zh-CN`, `zh-SG`, `zh-Hans`, not
Traditional locales. Ukrainian uses the bundled Cyrillic interface fonts. Vertical text in titles
is supported. The interface (egui) shapes each font run with harfrust and has Arabic and Hebrew
fallback fonts (`crates/ui-egui/src/rtl.rs`), but it has no bidi pass: only captions (timeline
blocks, the Text panel list when not being edited) are laid out in reading order, and the interface
itself is never mirrored right to left. The title engine (`crates/text`, harfrust + bidi) lays out
right to left, but Arabic titles are reported broken (#395).

## Status

Recounted **2026-10-21** at `d643a886` using **3,597 unique `(context, source)` keys** in the union
of the eleven TSV catalogs. Count only valid three-column entries with nonempty source and
translation, deduplicated by key; zero malformed rows were found. This measures catalog presence,
not translation quality, unwrapped source literals, engine messages or script/font support.
Percentages are rounded to one decimal; Spanish is 3,596/3,597, not exact full coverage.

| Language | Code | Unique keys | Catalog coverage | Status / validation still needed |
|---|---|---:|---:|---|
| English | en | source language; no TSV | n/a | Source UI; no new literal inventory counted |
| Simplified Chinese | zh-cn | 3,579 | 99.5% | Partial; CJK fonts and native review |
| Spanish | es | 3,596 | 100.0% | Partial; missing key, native review and engine messages |
| Hindi | hi | 0 | 0% | No catalog; interface shaping/font support to validate |
| Arabic | ar | 0 | 0% | No catalog or mirrored RTL interface; title report #395 needs reproduction |
| French | fr | 3,564 | 99.1% | Partial; missing keys and native review |
| Portuguese (Brazil) | pt-br | 3,559 | 98.9% | Partial; missing keys and native review |
| Indonesian | id | 3,559 | 98.9% | Partial; missing keys and native review |
| Japanese | ja | 3,593 | 99.9% | Partial; CJK fonts and native review |
| German | de | 3,590 | 99.8% | Partial; menus previously native-reviewed (#630), remaining keys/text need review |
| Korean | ko | 3,594 | 99.9% | Implemented, partial; Hangul font availability and native review |
| Vietnamese | vi | 0 | 0% | No catalog; stacked-diacritic font coverage to validate |
| Italian | it | 3,593 | 99.9% | Partial; missing keys and native review |
| Russian | ru | 355 | 9.9% | Partial catalog; broader translation and native review |
| Ukrainian | uk | 355 | 9.9% | Partial catalog; broader translation and native review |

See the [progress review](progress-review.md#recounted-localization-evidence) for scope and limits.
Catalog/font tests were inspected but not run. The `Language` enum currently exposes twelve
choices including English; Hindi, Arabic and Vietnamese are not choices. The twelve *key languages*
in the assessment are a different comparison set.

**Localization dimension:** 9 of the 12 key languages near-complete (English, Spanish, Japanese,
Simplified Chinese, French, Portuguese (Brazil), Indonesian, Korean, German), 3 none (Hindi, Arabic,
Vietnamese); against Premiere's 10 languages we cover 9 well and Russian partly. **Historical estimate
(2026-10-10): ~35%, 150–230 h**, not recalculated from this catalog recount, for all twelve to `full` plus Italian and Russian, of which the script
work (RTL and complex shaping in the egui interface) is the largest and least parallel part.
Every language needs native-speaker review (human).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-21 | minor | Recount eleven catalogs against 3,597 unique keys; Korean implemented, Russian/Ukrainian partial; distinguish catalog presence from language quality/readiness |
| 2026-10-13 | minor | German (de) covers the whole interface: every entry of the Spanish catalog (3,590); the tests require the core menus and the macOS app menu and reject entries without a Spanish one |
| 2026-10-11 | minor | German (de) ships the menus (#630): 371 of 3,498 strings (10.6%), native-reviewed; dimension now 2 menus-only, 5 none |
| 2026-10-10 | major | Created from ROADMAP.md's "Interface localization" section; catalog coverage measured (es 3,496, ja 3,487, zh-cn 3,485, pt-br 348, uk 351 of 3,498); the 12-language table and script-support gaps added |
