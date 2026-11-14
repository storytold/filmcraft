# Feature naming migration

Status: draft, 2026-11-08. This is an engineering naming policy and rollout plan, not trademark clearance.

## Scheme and selected names

Use the existing ArtCraft app identity with short, descriptive feature names. Share names across apps when they describe the same function. Use stable identifiers for dispatch and persistence; translate display labels without changing those identifiers. Preserve interoperability labels and required notices.

| Previous display label | Selected display label | Stable key |
| --- | --- | --- |
| Lumetri Color | Color Grading | `lumetri`, `PanelKind::LumetriColor` |
| Lumetri Scopes | Video Scopes | `PanelKind::LumetriScopes` |
| Lumetri Presets | Color Grading Presets | `Lumetri Presets` folder key; existing preset ids |
| Enable Mercury Transmit | Enable External Video Output | `playback.enableTransmit` |

The external video output setting currently only stores a preference; this rename does not implement hardware output.

### Deferred to RFC-0029

Three Adobe names stay until that RFC decides the replacement, so their labels, catalog rows and
docs are unchanged here:

| Display label (unchanged) | Stable key |
| --- | --- |
| Ultra Key | `ultra_key` |
| Warp Stabilizer | `warp_stabilizer` |
| Morph Cut | `morph_cut` |

They are not retired marks: the display guard in `crates/ui-egui/src/i18n/tests.rs` does not ban
them, and their old/new-name aliases are gone because the label is the name again.

## Integration and attribution

[PR #1880](https://github.com/storytold/filmcraft/pull/1880), for [issue #1868](https://github.com/storytold/filmcraft/issues/1868), is the single integration PR. Its original author's commits are preserved unchanged in history. The complementary work from [draft #4545](https://github.com/storytold/filmcraft/pull/4545) is integrated here rather than maintained as a competing PR. The interim Color Grade wording is replaced by the selected suite-wide Color Grading name.

## Compatibility and localization

- Effect, renderer, command, preference, preset and panel enum ids are unchanged. Old human-readable effect/panel names remain lookup aliases, and the Effects panel search still matches them (`Lumetri`, `Color Grade`).
- Interchange files are unchanged: FCP7 XML, FCPXML and EDL write `filmcraft_project::interchange_name`, which is the name an export has always written (`Lumetri Color`), so an export made before and after the rename is byte-identical. Display labels come from `EffectDef::name`.
- Scoped shortcut imports accept both old and current panel names. The Effects panel keeps its original serialized folder key and old automation paths, with descriptive-name aliases for callers of the original draft.
- Folder dispatch and persistence use the stable key, not its translated label. Old folder expansion state still opens the same folder.
- All 16 shipped locale catalogs include the selected effect names, grading/scopes/preset labels, add-to-clip button and color-management settings note. The locale regression checks parsing, unique keys and absence of retired display marks in translated entries. Additional guards scan the actual built-in interface names and translated output, while retaining English fallback and excluding compatibility identifiers and user text. A feature label must also read differently from every other label in its catalog (German and Russian had both read "Color Grading" and "Color Correction" the same way).
- Existing license, source and author records are preserved. Internal filenames and golden scene ids are not renamed.

## Remaining validation gates

1. Run `cargo xtask ci` on the integrated head, including project lookup/serialization, panel aliases, shortcut import, folder-state and i18n regressions. Focused harnesses do not replace the aggregate gate.
2. Review translations and the displayed controls in all locales, particularly long labels and CJK layouts.
3. Recapture affected screenshots from the updated app. Existing images, including `docs/images/filmcraft-color-panel.png`, still contain old UI text. Captions and descriptions now use the selected terminology, but the pixels have not been refreshed. Update screenshot pixels and attribution records together before release.
4. Review current help/marketing copy and retain factual format/comparison references and required notices where appropriate.

The app name, package names, file formats, iconography and existing first-party trademark license are unchanged.
