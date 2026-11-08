# Feature naming migration

Status: draft, 2026-11-08. This is an engineering naming policy and rollout plan, not trademark clearance.

## Scheme

Use the existing ArtCraft app identity with short, descriptive feature names. Share names across apps when they describe the same function. Use stable internal identifiers for dispatch and persistence; display names are not identifiers. Preserve interoperability labels and required notices.

| Previous display label | Proposed display label | Stable key |
| --- | --- | --- |
| Ultra Key | Chroma Key | `ultra_key` |
| Warp Stabilizer | Motion Stabilizer | `warp_stabilizer` |
| Morph Cut | Optical Flow Dissolve | `morph_cut` |
| Enable Mercury Transmit | Enable External Video Output | `playback.enableTransmit` |

These names describe the existing implementations: a chroma keyer, a 2-D motion stabilizer, and an optical-flow dissolve without face tracking. The external video output setting currently only stores a preference. The rename does not implement external hardware output or add capability claims.

## Complement to the existing color-name PR

[Issue #1868](https://github.com/storytold/filmcraft/issues/1868) is already addressed by [PR #1880](https://github.com/storytold/filmcraft/pull/1880). This draft does not duplicate its Lumetri migration or modify its contributor's branch. Before considering the overall naming audit complete, coordinate these suite-wide names there:

- Lumetri Color → **Color Grading**.
- Lumetri Scopes → **Video Scopes**.
- Lumetri Presets → **Color Grading Presets**.

PR #1880 currently says Color Grade. Align the wording with the EffectCraft migration before release, and extend its locale coverage to all currently supported catalogs. Preserve `lumetri`, panel enum serialization, command ids, shortcut panel aliases, preset ids and existing automation folder keys. Add case-insensitive old-label lookup tests and saved-project/layout round trips.

## Changes in this draft

- Rename the four labels above, related source documentation, the README and testing guide.
- Translate all four new labels in every one of the 16 shipped language catalogs, including Ukrainian entries that were previously absent.
- Accept the old effect/transition names in the name-based lookup API. The new display names and original ids resolve to the same definitions.
- Keep effect instance schemas, defaults, renderer selection, serialized ids, commands and preference keys unchanged. Add lookup and serialization regressions plus a locale coverage test.

## Remaining release gates

1. Review translation quality and the displayed controls in each locale, especially longer labels.
2. Run `cargo xtask ci` on the final head, including the new project and UI tests. Focused checks do not replace the aggregate gate.
3. Complete and coordinate #1880 rather than opening a duplicate color-name implementation.
4. Recapture affected UI screenshots from the updated app. `docs/images/filmcraft-color-panel.png` has been visually inspected and still shows the previous color-panel name. Inspect all other README/UI screenshots and update real pixels, captions, sidecars and the attribution index together. This draft does not pretend an unchanged screenshot has new text.
5. Keep historical and explicit competitor-comparison documents as such; review current user guides for stale first-party wording once both PRs are integrated. Keep provenance, required license notices and format references.

The app name, package names, file formats, iconography and existing first-party trademark license are not changed by this feature-label PR.
