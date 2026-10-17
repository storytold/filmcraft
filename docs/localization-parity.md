# Localization parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-13 · **Change:** minor (Korean covers the whole interface, #508) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

Per-language status of FilmCraft's interface. Premiere 26.5.2 ships 10 interface languages (the
installed bundle's `.lproj` folders): German, English, Spanish, French, Italian, Japanese, Korean,
Portuguese (Brazil), Russian, Simplified Chinese.

## How it works

Edit ▸ Language (also Settings ▸ General, `general.interfaceLanguage`) switches the interface and
persists across restarts; System Language (the default) follows the OS's or browser's preferred
languages and falls back to English. Commands `app.language.<english|japanese|spanish|portuguese|ukrainian|chinese|german|russian|french|indonesian|italian|georgian>`
are reachable over the control channel. One catalog per language,
`crates/ui-egui/src/i18n/<code>.tsv` (format in the header of `es.tsv`); UI code wraps strings in
`tl!("…")` / `tlf!("…{name}", name)`, and registry names (effects and their parameters, settings,
commands, panels, workspaces, presets) are translated where they are drawn. Searches match the
displayed labels as well as English sources. Tests fail when a `tl!` literal, a menu label or a
registry name has no Spanish entry; `zh-cn.tsv` must also cover every `tl!` literal and menu
label, and `ka.tsv` every Spanish entry, `tl!` literal and menu label; `ja.tsv`, `fr.tsv` and `pt-br.tsv` have the same entries but are not yet required by tests
(French must cover the core menus, Brazilian Portuguese every menu label, and neither may keep an
entry without a Spanish one). Engine error messages, the
control channel, the CLI and MCP stay English.

Fonts come from [craft-fonts](https://github.com/storytold/craft-fonts) (`CRAFT_FONTS_DIR`, all
official releases) or an installed system face; without one, switching to Japanese or Chinese is
refused with a message. Georgian uses the craft-fonts Noto Sans Georgian face, on the web too
(native builds without craft-fonts use an installed Georgian face, like Arabic and Hebrew), and
the header's capitalised workspace name leaves Georgian lowercase (no Mtavruli). Simplified Chinese is picked for `zh`, `zh-CN`, `zh-SG`, `zh-Hans`, not
Traditional locales. Ukrainian uses the bundled Cyrillic interface fonts. Vertical text in titles
is supported. The interface (egui) shapes each font run with harfrust and has Arabic and Hebrew
fallback fonts (`crates/ui-egui/src/rtl.rs`), but it has no bidi pass: only captions (timeline
blocks, the Text panel list when not being edited) are laid out in reading order, and the interface
itself is never mirrored right to left. The title engine (`crates/text`, harfrust + bidi) lays out
right to left, but Arabic titles are reported broken (#395).

## Status

Measured 2026-10-10 by counting entries in each catalog. The denominator is the union of all
catalogs' sources (**3,498 strings**); strings not yet wrapped in `tl!` and engine messages are
not in it, so real coverage is somewhat lower than shown. Estimates calibrated from #449 (Japanese,
whole interface, one PR) at ~4–8 h per language for the catalog plus tests.

| Language | Code | UI strings | Dialogs / tooltips / help | Script support | Native review | Status | To `full` |
|---|---|---|---|---|---|---|---|
| English | en | 3,498 (100%, source) | yes; no help docs in-app | Latin | n/a | **full** | — |
| Simplified Chinese | zh-cn | 3,485 (99.6%) | dialogs, Settings, effects, shortcuts, status; engine errors English | CJK font fallback; system IME via winit | not recorded | partial | 6–10 h + review |
| Spanish | es | 3,496 (99.9%) | as above | Latin | not recorded | partial | 4–8 h + review |
| Hindi | hi | 0 (0%) | — | **no Devanagari shaping in the interface** | — | none | 30–50 h (shaping in UI + catalog + fonts) |
| Arabic | ar | 0 (0%) | — | **no RTL interface layout**; fallback fonts and shaping in the interface, captions in reading order, other text in egui's left-to-right run order; titles broken (#395) | — | none | 40–70 h |
| French | fr | 3,526 (same entries as Spanish, plus the macOS Settings item) | as Spanish | Latin (bundled fonts) | not recorded; welcome | partial (#498) | review |
| Portuguese (Brazil) | pt-br | 3,541 (same entries as Spanish, plus the macOS Settings item) | as Spanish | Latin (bundled fonts) | not recorded; welcome | partial (#859) | review |
| Indonesian | id | 3,541 (same entries as Spanish, plus the macOS Settings item); must cover the core menus, no entry without a Spanish one | as Spanish | Latin (bundled fonts); System Language also maps the legacy `in` code | not recorded; welcome | partial (#867) | review |
| Japanese | ja | 3,487 (99.7%) | as zh-cn; system-font kanji may pick the Chinese fallback (#547) | CJK fonts, vertical title text | not recorded | partial | 4–8 h + review |
| German | de | 371 (10.6%) | menus only | Latin | yes (#630) | menus only | 5–9 h + review |
| Korean | ko | 3,573 (same entries as Spanish, plus the macOS Settings item) | as Spanish | installed system Hangul font (Malgun Gothic, Apple SD Gothic Neo, Noto Sans CJK KR…); refused without one | not recorded; welcome | partial (#508) | review; Hangul in craft-fonts for the web |
| Vietnamese | vi | 0 (0%) | — | Latin with stacked diacritics (font coverage to check) | — | none | 6–12 h + review |
| *Other shipped:* Italian | it | 3,573 (same entries as Spanish, plus the macOS Settings item); must cover the core menus, no entry without a Spanish one | as Spanish | Latin (bundled fonts) | not recorded; welcome | partial | review |
| *Other shipped:* Ukrainian | uk | 351 (10.0%) | menus only | Cyrillic (bundled) | not recorded | menus only | 5–9 h |
| *Other shipped:* Georgian | ka | 3,591 (every Spanish entry, plus the macOS Settings item) | as Spanish | Georgian (craft-fonts Noto Sans Georgian) | native speaker | partial | review |

Premiere also ships Italian, which we now cover like Spanish, and Russian (6–10 h).

**Localization dimension:** 6 of the 12 key languages near-complete (English, Spanish, Japanese,
Simplified Chinese, French, Portuguese (Brazil)), 1 menus-only, 5 none; against Premiere's 10 languages
we cover 6 well and 1 partly. **~35%, 150–230 h** for all twelve to `full` plus Italian and Russian, of which the script
work (RTL and complex shaping in the egui interface) is the largest and least parallel part.
Every language needs native-speaker review (human).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-11 | minor | German (de) ships the menus (#630): 371 of 3,498 strings (10.6%), native-reviewed; dimension now 2 menus-only, 5 none |
| 2026-10-10 | major | Created from ROADMAP.md's "Interface localization" section; catalog coverage measured (es 3,496, ja 3,487, zh-cn 3,485, pt-br 348, uk 351 of 3,498); the 12-language table and script-support gaps added |
