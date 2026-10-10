# Localization parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (new file: per-language status measured from the catalogs; replaces ROADMAP.md's "Interface localization" section) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

Per-language status of FilmCraft's interface. Premiere 26.5.2 ships 10 interface languages (the
installed bundle's `.lproj` folders): German, English, Spanish, French, Italian, Japanese, Korean,
Portuguese (Brazil), Russian, Simplified Chinese.

## How it works

Edit ▸ Language (also Settings ▸ General, `general.interfaceLanguage`) switches the interface and
persists across restarts; System Language (the default) follows the OS's or browser's preferred
languages and falls back to English. Commands `app.language.<english|japanese|spanish|portuguese|ukrainian|chinese>`
are reachable over the control channel. One catalog per language,
`crates/ui-egui/src/i18n/<code>.tsv` (format in the header of `es.tsv`); UI code wraps strings in
`tl!("…")` / `tlf!("…{name}", name)`, and registry names (effects and their parameters, settings,
commands, panels, workspaces, presets) are translated where they are drawn. Searches match the
displayed labels as well as English sources. Tests fail when a `tl!` literal, a menu label or a
registry name has no Spanish entry; `zh-cn.tsv` must also cover every `tl!` literal and menu
label; `ja.tsv` has the same entries but is not yet required by tests. Engine error messages, the
control channel, the CLI and MCP stay English.

Fonts come from [craft-fonts](https://github.com/storytold/craft-fonts) (`CRAFT_FONTS_DIR`, all
official releases) or an installed system face; without one, switching to Japanese or Chinese is
refused with a message. Simplified Chinese is picked for `zh`, `zh-CN`, `zh-SG`, `zh-Hans`, not
Traditional locales. Ukrainian uses the bundled Cyrillic interface fonts. Vertical text in titles
is supported. The interface (egui) does **not** shape complex scripts or lay out right to left;
the title engine (`crates/text`, harfrust + bidi) does, but Arabic titles are reported broken (#395).

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
| Arabic | ar | 0 (0%) | — | **no RTL layout, no shaping in the interface**; titles broken (#395) | — | none | 40–70 h |
| French | fr | 0 (0%) | — | Latin | — | none (requested #498) | 6–10 h + review |
| Portuguese (Brazil) | pt-br | 348 (9.9%) | menus only | Latin | not recorded | menus only | 5–9 h + review |
| Indonesian | id | 0 (0%) | — | Latin | — | none | 6–10 h + review |
| Japanese | ja | 3,487 (99.7%) | as zh-cn; system-font kanji may pick the Chinese fallback (#547) | CJK fonts, vertical title text | not recorded | partial | 4–8 h + review |
| German | de | 0 (0%) | — | Latin | — | none (requested #490) | 6–10 h + review |
| Korean | ko | 0 (0%) | — | needs Hangul fonts in craft-fonts and IME testing | — | none (requested #508) | 10–20 h + review |
| Vietnamese | vi | 0 (0%) | — | Latin with stacked diacritics (font coverage to check) | — | none | 6–12 h + review |
| *Other shipped:* Ukrainian | uk | 351 (10.0%) | menus only | Cyrillic (bundled) | not recorded | menus only | 5–9 h |

Premiere also ships Italian and Russian, which we don't (6–10 h each).

**Localization dimension:** 4 of the 12 key languages near-complete (English, Spanish, Japanese,
Simplified Chinese), 1 menus-only, 7 none; against Premiere's 10 languages we cover 4 well and 1
partly. **~35%, 150–230 h** for all twelve to `full` plus Italian and Russian, of which the script
work (RTL and complex shaping in the egui interface) is the largest and least parallel part.
Every language needs native-speaker review (human).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | major | Created from ROADMAP.md's "Interface localization" section; catalog coverage measured (es 3,496, ja 3,487, zh-cn 3,485, pt-br 348, uk 351 of 3,498); the 12-language table and script-support gaps added |
