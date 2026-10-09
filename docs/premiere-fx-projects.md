# Premiere FX & Projects

FilmCraft can import native `.prproj` projects and `.prfpset` effect presets. This is a
data-file compatibility layer, implemented in Rust from observed user-authored files and
documented preset behaviour. It reads PremiereData version 3 object graphs; it does not
load Adobe plug-in binaries. No Adobe code, preset assets or user projects are included.

## Import a project

Use **File → Import** or `file.import {"paths":["/path/project.prproj"]}`. The Media Browser
also recognises `.prproj` as a project. XML, UTF-16 XML and gzip-wrapped XML are accepted.
The imported fragment is merged through the existing interchange path, and source media
are linked through the host's streaming media reader when available.

The importer reads project bins, media paths and stream descriptions, sequences, video/audio
tracks, lock/mute/sync-lock flags, clip start/end and source in/out times, constant speed from
the source span, linked audio/video clips, nested sequences, supported effect components and
Cross Dissolve/Constant Power/Constant Gain/Exponential Fade timeline transitions. Projects
with bins and media but no sequence are accepted too.

Native timing is already in FilmCraft's 254,016,000,000 ticks per second. Cut and source times
remain integers. Rounded native media frame-duration metadata uses a bounded rational frame-rate
approximation, which is reported; sequence timing is not reconstructed from those media rates.

## Import and apply effect presets

Use **Effects → Presets → Import Presets…** and select `.prfpset`, or run
`presets.import {"path":"/path/effects.prfpset"}`. Preset-tree references determine which
items are imported. Custom folders become `Folder/Preset` names, so unreferenced objects
are not accidentally added. Imported presets persist in the normal FilmCraft library and
can be exported in FilmCraft's JSON preset format.
Import reads through the editor's host services, including hosts without a native filesystem.

Mapped native components:

| Native match name | FilmCraft component |
| --- | --- |
| `AE.ADBE Motion` | Motion |
| `AE.ADBE Geometry2` | Transform |
| `AE.ADBE Opacity` | Opacity; unsupported blend selectors are reported |
| `AE.ADBE AECrop` | Crop |
| `AE.ADBE Offset` | Offset |
| `AE.ADBE Motion Blur` | Directional Blur |
| `AE.ADBE Gaussian Blur 2` | Gaussian Blur; unmapped controls are reported |
| `Internal Volume Mono/Stereo` | Volume, converting linear gain to dB |
| `AE.ADBE Cross Dissolve New` with `TransitionDuration` | Cross Dissolve transition preset |

Static values, enabled state and supported keyframe values/times are imported using stable
parameter IDs, rather than translated display names. Preset keyframes are normalised against
their saved `AnchorInPoint`; `Type` 0/1/2 maps to Scale/Anchor to In/Anchor to Out. FilmCraft's
existing preset application then retimes them onto the selected clip. These modes follow
[Adobe's documented preset behaviour](https://helpx.adobe.com/premiere/desktop/add-video-effects/apply-video-effects/create-effect-presets.html).

Native points are frame fractions. Presets store a 1920×1080 reference basis and mark their
coordinate convention: on application, Motion position uses sequence dimensions, while its
anchor and standard-effect points use source dimensions. This preserves placement when a
landscape source is used in a portrait sequence.

A saved Cross Dissolve is a timeline transition, not a clip filter. Select one clip and apply
the preset near the desired edge, or use `presets.apply {"preset":"Name","clips":[42],"edge":"out"}`
(`"in"` selects the other edge). Duration is rounded to the nearest target-sequence frame and
the result reports any rounding. Applying it is one undo step.

## Fidelity reports and limits

Both imports return a `report`. File import carries it in `documents`; preset import returns
it directly. The UI shows the first report entries in its status bar. Unknown effects and
parameters are named; an entirely unsupported preset is not added as an empty preset.
Malformed documents are parsed completely before project or preset-library changes commit.
Preset-library save failure restores the previous in-memory library.

The current mapping is not a claim of pixel parity with Premiere. Temporal influence is read,
but temporal velocities and spatial Bézier handles are approximated by FilmCraft interpolation
and reported. Composition shutter angle is approximated as 180° when requested by Transform.
Masks, opaque effect data, Lumetri/Ultra Key/text components, most audio plug-ins, mixer
automation, native routing between multiple audio streams, multicam/merged-clip semantics,
markers, timecode origins, pixel-aspect metadata,
advanced colour-management metadata and native project export are not translated.
Unsupported generated media retain offline placeholders;
normal file-backed unsupported media remain relinkable file references. Overlapping clip
ranges on one native track and cyclic bins/sequences are rejected rather than silently changed.
Mixed transition/clip-effect presets and disabled transition presets are rejected.

Limits: 64 MiB of encoded/decompressed XML, two million XML nodes, 200,000 indexed objects,
two million graph/keyframe expansion operations, 64 nesting levels, 1,024 tracks per group,
65,536 keyframes per parameter. Imported timeline/source/keyframe spans leave arithmetic
headroom by limiting ticks to one quarter of the signed 64-bit range. No entity DTD is loaded.
Shared text fields are bounded before graph expansion: identifiers/XML field names 256 bytes,
names/descriptions/folder prefixes 4 KiB, static values/frame rectangles 1 KiB, numeric fields
128 bytes, channel-layout JSON 16 KiB and media paths 128 KiB. Opaque payloads remain subject
to the whole-document limit.

## Reproduce checks

```sh
cargo test -p filmcraft-interchange --test premiere
cargo test -p filmcraft-engine premiere
cargo run -p filmcraft-interchange --example premiere_inspect -- project.prproj presets.prfpset
```

The synthetic tests cover gzip/UTF-16, exact cuts and source offsets, links, bins, nested
sequences and cycles, foldered presets, three timing modes, reported unsupported effects,
temporal approximation, truncation/mutation, expansion limits, preset persistence/rollback,
portrait geometry, native transition application and undo/redo. The inspector reads user files
without decoding or modifying their source media. Keep private reference files outside the repo.
