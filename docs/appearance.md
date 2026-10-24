# Appearance and workspaces

> **Last reviewed:** 2026-10-19 · **Last updated:** 2026-10-19 · **Change:** major (new file: the Evolve theme and the page workspaces)

How FilmCraft looks and how its panels are arranged: the colour themes (Settings ▸ Appearance),
and, for editors coming from page-based grading suites, the Evolve theme, page workspaces and
keyboard preset.
Keyboard shortcut presets are in [keyboard.md](keyboard.md). Every option here is a preference or
a workspace file in the per-user data directory, never part of a project.

Everything in this page is FilmCraft's own work. The Evolve options reproduce no vendor
asset: the colours are original values chosen for the same feel, the layouts use FilmCraft's own
panels, and the keys are functional facts about how such suites are operated ([AGENTS.md](../AGENTS.md)
§1 and §2).

## Themes

Settings ▸ Appearance shows an Appearance Mode (Dark, Light, or Sync with system) and a theme card
for each mode. The dark card offers three themes; the light card one.

| Preference value | Name | Look |
|---|---|---|
| `darkest` (default) | Darkest | black gutters, `#1d1d1d` panels, blue highlight |
| `dark` | Dark | the same with lighter panels |
| `evolve` | Evolve Dark | cool charcoal panels (`#28282d`) on a near-black window, thin gutters, a warm orange highlight (`#be511b`, with `#f2a26a` for orange text), red playhead, near-white timecodes, a darker viewer well |
| `light` | Light | light grey panels, dark text |

Ways to choose a theme:

- Settings ▸ Appearance ▸ Dark Theme ▸ **Evolve Dark** (automation id `settings.appearance.darkTheme.evolve`), applied with OK.
- View ▸ Appearance ▸ Evolve Dark (`view.theme.evolve`), which also fixes the mode to Dark.
- The header's appearance button (right-click) when the theme is a favourite.
- Control channel / MCP: `ui.set {"theme": "evolve"}` (also `dark`, `medium`, `light`), or the
  preference `prefs.set {"key": "appearance.darkTheme", "value": "evolve"}`. `ui.inspect` reports
  the theme in use as `ui.dark` plus the preference values.

The theme is saved in `preferences.json` (`appearance.darkTheme`; the legacy `appearance.colorTheme`
follows it) and comes back at the next launch.

### Design tokens

Every widget reads its colours and sizes from `Tokens` (`crates/ui-egui/src/theme.rs`), so a
theme applies everywhere: panels, tab strips, fields, the timeline (track backgrounds, ruler,
playhead, targeting), monitors, scopes, meters, mixers and plots. The Evolve theme is its own
token set, not the Darkest palette with another accent: panel greys, text greys, the timeline
surfaces, the monitor well, the playhead and the timecode colour differ, and its chrome measures
differ too (30 pt tab strips, 3 pt gutters, 3 pt corner radii).

**Highlight colour.** Settings ▸ Appearance ▸ Highlight Color recolours selections, primary
buttons, the focus outline, the playhead, timecodes and track targeting. The default value
(`#2f6bdf`) means "the theme's own highlight": the Evolve theme keeps its orange, the others their
blue. A colour of the user's own applies to every theme.

**Contrast.** `theme::tests::evolve_text_contrast_meets_wcag_aa` checks the palette: body and
secondary text, tab labels, icons, timecodes and ruler text reach WCAG AA (4.5:1) against every
surface they are drawn on (panels, tab strips, header, fields, hover and selected rows, the
timeline, the ruler, the monitor well and the window background); faint labels reach 3:1; the
highlight, focus outline and playhead stand out from their backgrounds at 3:1 or better. The
orange highlight is a fill dark enough for white labels (primary buttons, selections, also
hovered) at 4.5:1; where the highlight is text (links, "Active" and "User" labels) the theme
uses its lighter orange (`Tokens::accent_text`), also at 4.5:1 on every surface.
Accessible color contrast (the same page) brightens secondary text and borders further.

## Workspace sets and the Evolve pages

Window ▸ Workspace Set chooses the family of built-in workspaces the window offers. It is a
feature of the docking model (`crates/ui-egui/src/dock.rs`, `WorkspaceSet`), saved in
`workspaces.json` beside the preferences, independent of the theme and the keyboard preset.

| Set | Command | Built-in workspaces | Page bar |
|---|---|---|---|
| Standard Workspaces (default) | `window.workspaceSet.standard` | Editing, Assembly, Color, Effects, Audio, Captions and Graphics, Learning, Review, All Panels (alphabetical in the menu) | no |
| Evolve Pages | `window.workspaceSet.evolve` | Media, Cut, Edit, Color, Audio, Deliver (page order) | yes |

With the Evolve set a **page bar** runs along the bottom of the window above the status bar
(`crates/ui-egui/src/pages.rs`): one button per page, the current page in the highlight colour
with an underline. The buttons take keyboard focus and register the automation ids `pages.media`,
`pages.cut`, `pages.edit`, `pages.color`, `pages.audio`, `pages.deliver` (the bar is
`pages.bar`). Window ▸ Workspaces lists the pages in page order, then the user's own workspaces
saved in this set, and the header's workspace popup does the same.

Each page places FilmCraft's existing panels where an editor expects them:

| Page | Command | Layout (FilmCraft panels) |
|---|---|---|
| Media | `window.page.media` | Media Browser and Libraries down the left; the Source monitor (Program beside it) over the Project bins, with Metadata, Info and Markers on the right. The Timeline is a tab of the bins group, shown when a sequence is opened. |
| Cut | `window.page.cut` | Project, Media Browser, Effects and Markers top left, the Program monitor top right, the Timeline across the bottom with the tools and meters. |
| Edit | `window.page.edit` | Project, Media Browser, Effects, Libraries, Markers and History left; Source and Program monitors; Effect Controls, Properties, Essential Graphics and Text as the inspector on the right; the Timeline across the bottom. |
| Color | `window.page.color` | A clip strip (the Timeline) on top; bins, the Program monitor and Lumetri Scopes (with the Reference Monitor) in the middle; Lumetri Color, Effects and Effect Controls across the bottom with Info and Metadata. Scopes in the Program monitor are off, as in the Standard Color workspace. |
| Audio | `window.page.audio` | A tall Timeline for the audio tracks; above it the Project bins with Essential Sound, the Program monitor and the Audio Track Mixer (Audio Clip Mixer, Text to Speech); the Audio Meters beside the Timeline. |
| Deliver | `window.page.deliver` | Export mode: the export settings, the preview and the export queue. Its dock layout (Progress, Events and Project beside the Program monitor and Timeline) shows when the header's Edit tab is chosen on this page. Choosing any other page returns to Edit mode. |

Every page keeps a Timeline and a Program monitor, so opening a sequence always has somewhere to
show it. Pages can be rearranged, saved (Save Changes to this Workspace) and reset like the standard
workspaces; a workspace saved while the Evolve set is active belongs to that set and is listed
only there. The page commands work from the Standard set too (they switch sets first), and a
standard workspace command such as `window.workspace.editing` switches back; the Edit page's id is
`window.page.edit` because `window.workspace.edit` is Edit Workspaces…. The set and the page in
use come back at the next launch.

Control channel / MCP: `ui.set {"workspaceSet": "evolve", "workspace": "Color"}`,
`ui.menu.invoke {"command": "window.page.audio"}`, `ui.click {"id": "pages.deliver"}`;
`ui.inspect` reports `workspaceSet` and the set's `workspaces`.

Tests: `dock::tests::evolve_pages_are_valid_and_place_the_expected_panels` (every page a valid
tree with no empty group, every expected panel reachable, serde round trip),
`dock::tests::workspace_sets_keep_their_own_workspaces` (names, layouts and saved workspaces per
set, files from before the set), and `crates/ui-egui/tests/evolve_pages_ui.rs` (the page bar,
the menus, Deliver and Export mode, a restart).

## Keyboard

Edit ▸ Keyboard Shortcuts… ▸ **Evolve** (`shortcuts.loadPreset {"name":
"Evolve"}`) binds the editing, trim, playback, marker and page keys of page-based grading
suites to FilmCraft's commands; the table and the keys with no counterpart are in
[keyboard.md](keyboard.md#evolve-preset). The three layers are independent:
the preset works with any theme and either workspace set, and its page keys (⇧2…⇧8) switch to the
Evolve set when it is not active.

## Out of scope

The Evolve options cover look, layout and keys. Node-based grading, primaries colour wheels and
a console-style audio mixer are different tools from Lumetri Color and the Audio Track Mixer and
are not reproduced; they are follow-ups ([gaps.md](gaps.md)).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-19 | major | Created: the Evolve Dark theme (`ThemeKind::Evolve`), its token set and contrast test |
| 2026-10-19 | major | Workspace sets: the Evolve pages (Media, Cut, Edit, Color, Audio, Deliver) and the page bar |
| 2026-10-19 | minor | The Evolve keyboard preset |
