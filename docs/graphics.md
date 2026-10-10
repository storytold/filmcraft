# Graphics

FilmCraft's graphics work like Premiere's Essential Graphics: a **graphic clip** on a video track
holds **text** and **shape** layers. Text is set by the text engine in `crates/text`
([README](../crates/text/README.md)): bundled OFL fonts plus system fonts, OpenType shaping with
kerning and ligatures, bidi, line breaking, all drawn as vectors in linear light.

## Model

- A graphic clip's project item is an `ItemKind::Graphic { width, height, rate }`: a transparent
  canvas the size of the sequence frame, with unlimited duration. These items are internal and do
  not appear in the Project panel.
- Its **layers** are effect instances `graphic_text` / `graphic_shape` in the clip's effect list,
  in paint order (first = back). They are hidden from the Effects panel. Because they are effect
  instances they save with the project, take keyframes on every animatable property (Source Text
  keyframes hold), appear in Effect Controls and copy with the clip. Standard effects on a graphic
  clip apply to the composed graphic; Motion and Opacity apply last.
- `filmcraft_project::graphic::eval_layer` evaluates a layer at a time into a `LayerSpec`.
- Projects with graphics need no schema migration: everything is additive (new item kind, new
  effect ids). Builds older than this one refuse such files with a parse error.

### Layer properties

| Group | Properties (parameter ids) |
|---|---|
| Text | `text` (Source Text), `font`, `font_style`, `size` (px), `align` (Left/Center/Right/Justify), `tracking` (1/1000 em), `kerning`, `ligatures`, `leading` (px added to the natural line height), `baseline_shift`, `faux_bold`, `faux_italic`, `caps` (Normal/All Caps/Small Caps), `underline`, `box_width` (0 = point text, else paragraph text wrapped at that width), `box_height` (paragraph text: lines that do not fit are not shown; 0 = as tall as the text) |
| Shape | `shape` (Rectangle/Ellipse/Polygon/Path), `size` [w, h], `sides`, `corner_radius` (one radius for all four corners, at most half the shorter side; on the Program monitor, drag one of the four small circles just inside a selected rectangle's corners), `points` (path vertices relative to the layer origin) |
| Appearance | `fill`, `fill_color`; `stroke`, `stroke_color`, `stroke_width`, `stroke_type` (Outer/Center/Inner) and the same for `stroke2`; `background`, `background_color`, `background_opacity`, `background_size` (padding), `background_radius`; `shadow`, `shadow_color`, `shadow_opacity`, `shadow_angle` (135° = down-right), `shadow_distance`, `shadow_size`, `shadow_blur` |
| Transform | `position` (graphic canvas px), `anchor` (layer px), `scale`, `scale_width`, `uniform_scale`, `rotation`, `opacity` |

Point text's origin is its alignment point on the first baseline (the click point of the Type
tool); paragraph text's origin is the top-left corner of its box; a shape's origin is its centre.
Colours are `#rrggbb` (sRGB).

### Point text and paragraph text

A text layer is one of two types, as in Premiere Pro (behaviour observed in Premiere Pro 26):

| | Point text | Paragraph text |
|---|---|---|
| Made with the Type tool by | a click | a drag: the dragged rectangle is the box |
| Lines | only where Return was typed | wrap at the box's width; lines below the box's bottom are not shown, and the bottom-right handle turns into a red plus |
| Selection box | fits the text | the box, whatever it holds |
| New layer's anchor point | the origin (left end of the first baseline) | the box's top-left corner |
| Dragging a handle | scales the layer about its anchor point: every handle scales both axes alike (Uniform Scale stays on), the font size is untouched | moves that side or corner of the box to the pointer, the opposite side stays; the text re-wraps at the same size and Scale is untouched |

- **Point-text scale.** Only the pointer's travel along one side of the box counts: along its
  width for the two side handles, along its height for the corners and the top and bottom handles.
  Travel `d` away from the anchor scales by `1 + d / r`, where `r` is the handle's distance from
  the anchor along that side, plus 60 points when the handle is nearer than 60 points (so a handle
  on the anchor does not scale without bound). A handle exactly on the anchor grows the text when
  dragged into the box.
- **Paragraph box.** A side dragged past its opposite stops at half the font size. When the top or
  left side moves, the layer's origin moves with the corner, so the anchor point is renumbered to
  stay on the same spot of the picture and Position does not change.
- **Anchor point.** Drag it with the Selection tool: the anchor moves, the layer stays (Position
  and Anchor Point change together). It is picked before anything under it: a shape dragged by
  its exact centre moves its anchor, not the shape, and the top-left handle of a new paragraph
  box is reached only after moving the anchor.
- **Switching type.** Properties ▸ Text ▸ wrench ▸ **Text Properties** ▸ Text Layer Type
  (`graphics.setTextType`). To point text: the wrapped lines become real lines (also those the
  box hid). To paragraph text: the box is fitted to the text. Either way the text stays where it
  is and Position is untouched; only Anchor Point is renumbered. Vertical text is always point
  text.

Known differences from Premiere: the first line of paragraph text sits one font ascent below the
box's top (Premiere places it higher, about one cap height below); Premiere's smallest box and
the box it fits when switching to paragraph text were not measured exactly; the 60 points were
measured with the Program monitor at Fit only; Text Properties has no Hindi Digits option.

## Rendering

`crates/render/src/graphic_clip.rs` rasterises each layer straight to the output transform
(sequence Motion × layer transform), so text stays sharp at any scale or rotation. Fill coverage
comes from the text engine; strokes come from a Euclidean distance transform of that coverage
(outer strokes sit under the fill, centre and inner strokes over it); the shadow is the union of
all parts, spread by Size, offset and blurred. Each layer's raster is cached while it stays the same.
The GPU plan receives one tight image per graphic clip and places it with a translation. The
Timecode and Clip Name effects also draw with the text engine.

## Commands

| Command | Menu / shortcut | Params |
|---|---|---|
| `graphics.newText` | Graphics and Titles ▸ New Layer ▸ Text (⌘T) | `text`, `position`, `box` (`[w, h]`: paragraph text in a box, `position` is its top-left corner), `clip` (add to this graphic), `vertical`, `size`, `font`, `fontStyle`, `seconds` (5), `track`, `time` |
| `graphics.newVerticalText` | New Layer ▸ Vertical Text | as `graphics.newText`; characters stack top to bottom, paragraphs are columns right to left |
| `graphics.newRectangle`, `graphics.newEllipse`, `graphics.newPolygon` | New Layer ▸ Rectangle (⌥⌘R), Ellipse (⌥⌘E), Polygon | `position` (the shape's centre), `size`, `clip`, `seconds` (5), `track`, `time`; polygon `sides` (6) |
| `graphics.newFromFile` | New Layer ▸ From file… | `path` — imports the image or video and places it above the clips at the playhead (a separate clip; graphics have no media layers yet) |
| `graphics.newShape` | (agents) | `shape` (rectangle/ellipse/polygon/path), `position` (the shape's centre), `size`, `points`, `clip` (add to this graphic), `seconds` (5), `track`, `time` |
| `graphics.setTextType` | Text Properties ▸ Text Layer Type | `clip`, `layer`, `type` (`point` / `paragraph`); the text stays where it is |
| `graphics.setText` | typing on the monitor | `clip`, `layer`, `text`, `merge` (coalesce one typing session into one undo step) |
| `graphics.set` | Properties panel | `clip`, `layer`, `props` {parameter id or camelCase alias: value; choices by index or name}, `time`, `merge` + `begin` (a drag is one undo step: send `merge: true` on every change and `begin: true` on the first change of each press) |
| `graphics.selectLayer` | layer list / monitor click | `clip`, `layers` |
| `graphics.deleteLayer`, `graphics.arrangeLayer` | layer list | `clip`, `layer`, `to` (front/back/forward/backward/index) |
| `graphics.align` | Align and Transform | `align` (left/hcenter/right/top/vcenter/bottom), `to` (frame/group/selection), `layers` |
| `graphics.alignFrame.<how>` | Align to Video Frame ▸ Left / Center Horizontally / Right / Top / Center Vertically / Bottom | each selected layer to the frame |
| `graphics.alignGroup.<how>` | Align to Video Frame as Group ▸ … | the selected layers' union to the frame (2+ layers; relative positions kept) |
| `graphics.alignSelection.<how>` | Align to Selection ▸ … | each selected layer to the selection's union (2+ layers) |
| `graphics.distribute` | Align and Transform | `axis` (horizontal/vertical), `space` (equal gaps), `layers` (3+) |
| `graphics.distributeVertically`, `…SpaceVertically`, `…Horizontally`, `…SpaceHorizontally` | Distribute ▸ … | the selected layers (3+): equal centre spacing or equal gaps |
| `graphics.bringToFront`, `graphics.bringForward`, `graphics.sendBackward`, `graphics.sendToBack` | Arrange ▸ … (⇧⌘], ⌘], ⌘[, ⇧⌘[) | `clip`, `layer` |
| `graphics.selectNextGraphic`, `graphics.selectPreviousGraphic` | Select ▸ Select Next / Previous Graphic | selects the next / previous graphic clip in timeline order (moves the playhead onto it) |
| `graphics.selectNextLayer`, `graphics.selectPreviousLayer` | Select ▸ Select Next / Previous Layer (⌥⌘], ⌥⌘[) | cycles the selected layer |
| `graphics.resetAllParameters` | Reset All Parameters | `layers` (default: the selected layers, else all) — appearance, text formatting and transform back to defaults; text, shape and geometry kept |
| `graphics.resetDuration` | Reset Duration | `seconds` (5), limited by the next clip on the track |
| `graphics.list` | (query) | `clip` — layers with names, kinds, text, `textType` (point / paragraph), `box`, `overflow` (the box hides text), position, anchor, scale and on-canvas quads |
| `fonts.list` | (query) | `system` (scan system font folders, default true) — families and styles |

Without `clip`, commands use the selected graphic clip, else the topmost graphic clip under the
playhead. Without `layer`, they use the selected layer, else the front one. New graphic clips are
placed at the playhead on the first video track above the clips there that is free for the
duration (a track is added if needed).

## Program monitor

| Tool | On the Program monitor |
|---|---|
| Type (T) | Click empty picture: new point-text layer with a caret. Drag on empty picture: new paragraph-text layer with that box. Click a text layer: caret there. The box of the text being typed into is red. Type; ←/→ (⌥ word, ⌘ line), ↑/↓, Home/End, Shift to select, ⌘A, ⌘C/⌘X/⌘V, Return = new line, Backspace/Delete, Esc = stop editing. Drag inside the edited text to select. |
| Selection (V) | Click a layer to select it (box with handles and anchor point; only selected layers have a box, and a layer whose visibility is off has none and cannot be clicked); drag to move; drag the anchor point to move it alone; drag a handle to scale point text about its anchor, resize a paragraph-text box (see *Point text and paragraph text*), or change a shape's Size: the dragged side or corner follows the pointer while the opposite one and the anchor point stay, Scale is untouched, and Shift on a corner keeps the proportions (a path, which has no Size, stretches instead); double-click a text layer to edit it. |
| Rectangle / Ellipse | Drag to draw a shape layer. |
| Pen (P) | Click to place points; click the first point (or Return) to close the path; Esc cancels. |

Moving a layer snaps its edges or centre to the frame edges, the frame centre and the guides
(View ▸ Snap in Program Monitor, on by default; hold ⌘/Ctrl to move freely). See
[monitors.md](monitors.md) for rulers and guides.

Automation ids: `program.layer.<clip>.<layer>`, `program.layer.<clip>.<layer>.handle.<n>`
(0–3 the corners from the top-left clockwise, 4–7 the top, right, bottom and left sides),
`program.layer.<clip>.<layer>.anchor`,
`program.textEdit` (while editing).

## Properties / Essential Graphics panels

With a graphic clip selected, the Properties panel (and Essential Graphics) shows: **Layers**
(front first; new text / rectangle / ellipse, bring forward / send backward, delete, visibility),
**Align and Transform** (six align buttons, two distribute buttons, position, anchor, scale,
rotation, opacity), **Text** (Source Text field, font family and style, size, paragraph alignment,
tracking, leading, baseline shift, box width and height, faux bold / faux italic / all caps /
small caps / underline, kerning and ligatures; the wrench in the section's header opens **Text
Properties**: Text Layer Type and Ligatures), **Shape**, and **Appearance** (fill, two strokes, background,
shadow). Automation ids: `graphics.layers.<n>`, `graphics.prop.<parameter id>`,
`graphics.align.<how>`, `graphics.distribute.<axis>`, `graphics.sourceText`, `graphics.NewTextLayer`,
`graphics.NewRectangle`, `graphics.NewEllipse`, `graphics.deleteLayer`, `graphics.section.<name>`,
`graphics.textProperties` (the wrench) and in its dialog `graphics.textProperties.type`,
`.type.point`, `.type.paragraph`, `.ligatures`, `.ok`, `.cancel`.

## Per-character styles

Select characters with the Type tool and change font, style, size, colour, faux bold / italic,
underline, tracking, baseline shift or caps in the Properties panel: the change applies to the
selection only (`graphics.setCharStyle`). Styles are stored on the layer as runs of characters
(`EffectInstance::layer.runs`, character offsets, sorted and non-overlapping) and follow the text
when it is edited (typed characters take the style of the character before them). The text engine
lays the runs out together (`filmcraft_text::layout_rich`): a run can change the font and size,
lines grow to fit the largest run, and each run can have its own fill colour.

| Command | Params |
|---|---|
| `graphics.setCharStyle` | `clip`, `layer`, `start`, `end` (characters; default the whole text), `style` {`font`, `fontStyle`, `size`, `color`, `bold`, `italic`, `underline`, `tracking`, `baselineShift`, `caps`}, `clear` (remove styles from the range) |

## Responsive design

**Position (pins).** A layer's edges can be pinned to another layer of the same graphic or to the
video frame (`graphics.pin {layer, to: "frame" | layer index | name | "none", edges}`). Each
pinned edge keeps its distance to the *same* edge of the target, measured when the pin is made:
a box pinned on all four edges to a text layer grows with the text; a shape pinned on two opposite
edges is resized, a text layer only moves. Pins resolve in dependency order (cycles are ignored).
Moving a pinned layer keeps the pin at its new distance. Layers get a stable `uid` for this.

**Time (intro / outro).** `graphics.setResponsiveTime {introFrames, outroFrames}` protects the
first and last part of a graphic: when the clip is trimmed or extended, keyframes inside the intro
keep their timing from the clip start, those inside the outro from the clip end, and the middle is
stretched or squeezed (`graphic_design::remap_time`). Trimming the head keeps the intro playing
from the new start.

**Rolls and crawls.** `graphics.setRoll {mode: off | roll | crawlLeft | crawlRight,
startOffScreen, endOffScreen, prerollFrames, easeInFrames, easeOutFrames, postrollFrames}` (the
Roll options of Responsive Design – Time) moves the whole graphic: up for a roll, sideways for a
crawl. *Start Off Screen* starts with the content just outside the frame (below it / right of it /
left of it), otherwise where it was laid out; *End Off Screen* lets it leave completely, otherwise
it stops when its last edge reaches the frame edge. The speed is constant between the preroll and
the postroll, with linear acceleration over Ease In and deceleration over Ease Out
(`graphic_design::roll_progress`). The offset is exact maths on the union of the layers' bounds
(with background padding), at the clip-relative time.

These settings are `TrackItem::graphic` (`GraphicMeta`, schema v12).

## Graphics templates (`.fcgt`)

FilmCraft's equivalent of motion graphics templates is its **own** format: a `.fcgt` file is
documented, versioned JSON (`filmcraft_project::gtemplate`):

| Field | Meaning |
|---|---|
| `format`, `version` | `"filmcraft.graphicsTemplate"`, `1` (newer versions are refused) |
| `id`, `name`, `category`, `description`, `author`, `license`, `tags` | library information |
| `canvas`, `duration` | frame size it was designed for, default clip length (ticks) |
| `layers` | the graphic's layers, exactly as in a project file, each with a `uid` |
| `graphic` | roll / crawl and responsive time |
| `controls` | editable properties: `{id, name, kind: text\|color\|slider\|checkbox\|font\|position, layer: uid, param, min, max}`; `param: "enabled"` is the layer's visibility |
| `resources.fonts` | optional embedded font files (base64) with their licence; registered on install |

Placing a template on a sequence of another size scales and centres its layers (pin distances
scale too).

**FilmCraft never reads or imports Adobe `.mogrt` files** (or any other application's template
packages): Install refuses `.mogrt` paths without opening them, and the reader rejects ZIP
archives. Built-in templates are original designs made for FilmCraft in code (no image, font or
template files): lower thirds (Slab, Rule, Ticker crawl), titles (Centered, Boxed), end credits
(Roll) and callouts (Pointer, Tag), with the bundled Inter font. Thumbnails are rendered by the
engine at runtime.

| Command | Menu | Params |
|---|---|---|
| `graphics.template.list` | (query) | `query`, `category`, `source` (builtin / user) |
| `graphics.template.apply` | Browse tab: drag or double-click | `template` (id, name or `.fcgt` path), `values` {control: value}, `time`, `track` |
| `graphics.template.export` | Graphics and Titles ▸ Export As Motion Graphics Template… | `clip`, `name`, `category`, `description`, `controls` [{`layer`, `param`, `name`, `kind`, `min`, `max`}], `path` (default: the user folder), `embedFonts` |
| `file.exportGraphicsTemplate` | File ▸ Export ▸ Motion Graphics Template… | as above |
| `graphics.template.install` | Graphics and Titles ▸ Install Motion Graphics Template… | `path` (a `.fcgt`) |
| `graphics.template.remove` | Browse tab | `template` (user templates) |
| `graphics.template.set` / `graphics.template.controls` | Essential Graphics ▸ Edit | `clip`, `control`, `value` / `values` |
| `graphics.template.thumbnail` | (query) | `template`, `width`, `path` (PNG) |

User templates live in `<data dir>/Graphics Templates/`.

**Essential Graphics panel.** *Browse* lists the built-in and user templates as cards with
engine-rendered thumbnails, a search field and a category filter; click selects, double-click or
Apply places the template at the playhead, dragging a card onto a video track places it there,
Install… picks a `.fcgt`, Remove deletes a user template. *Edit* is the graphic editor; for a graphic
made from a template it starts with **Template Properties** (one editor per control). With no layer
selected it shows **Responsive Design – Time** (intro / outro, Roll with its options); with a layer
selected, **Responsive Design – Position** (Pin To and the pinned edges). With characters selected
by the Type tool, the Text section's controls style just those characters. Export As Motion
Graphics Template… opens a dialog (name, category, description and a checklist of properties to
expose); Replace Fonts in Projects… lists the fonts in use (missing ones flagged) and replaces one.
Automation ids: `essentialGraphics.tab.browse|edit`, `gfxTemplates.search`, `gfxTemplates.category`,
`gfxTemplates.item.<id>`, `gfxTemplates.apply|install|remove`, `gfxTemplates.control.<control id>`,
`graphics.roll.*`, `graphics.time.intro|outro`, `graphics.pin.to|left|top|right|bottom`,
`exportTemplate.*`, `replaceFonts.*`; `ui.set {"gfxTemplates": {...}, "gfxEdit": {...}}` sets the
panel state and the Type-tool selection.

## Captions, source graphics and fonts

| Command | Menu | Params |
|---|---|---|
| `graphics.upgradeCaption` | Graphics and Titles ▸ Upgrade Caption to Graphic | `captions` (default: the selection, else the one under the playhead): each caption becomes a graphic clip on a video track with the track style (font, size, colour, background, outline, alignment, position); `<i>`/`<b>`/`<u>` become character styles; one undo step |
| `graphics.upgradeToSourceGraphic` | Graphics and Titles ▸ Upgrade to Source Graphic | `clip`: the graphic gets its own project item (shown in the Project panel); every clip of that item shares its layers — editing one updates the others in the same undo step (`project.source_graphics`) |
| `file.replaceFonts` | Graphics and Titles ▸ Replace Fonts in Projects… | `from` (family or {family, style}), `to`, `toStyle`: graphic layers, character styles, source graphics and caption tracks |
| `graphics.fonts.used` | (query) | fonts in use with counts and whether they are missing |

## Gradient fills

Appearance › Fill Type › Linear Gradient paints the shape or the text with a ramp from Gradient
Start to Gradient End. Stops are blended in linear light. The angle is degrees clockwise on
screen; 0° runs left to right across the layer's own bounds, and the ramp turns with the layer.
`graphics.set` accepts `fill_kind` (`solid` or `linear gradient`), `gradient_start`,
`gradient_end` and `gradient_angle`. A per-character fill colour still paints those characters
as a solid; the rest of the text keeps the ramp. A singular layer transform falls back to the
solid fill colour.

## Not yet

Mask-with-text, per-layer blend modes, Bézier curves in the pen tool (paths are polygons), media
layers inside graphics, and template controls grouped into folders.
