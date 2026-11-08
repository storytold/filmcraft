# Premiere import

`premiere.import` reads gzip or plain XML based on the LLM Video Editor Python parser and
structural observations from user-owned project XML. No Adobe application code, proprietary
documentation or assets were consulted or copied. The committed fixtures are hand-written
with placeholder names and paths; private projects and media are not included.
New code is MIT OR Apache-2.0.

```sh
# Substitute your project and NEW destination. A rendered preview is not required.
filmcraft-cli --save-as /path/to/episode.fcproj exec premiere.import \
  '{"path":"/path/to/source.prproj","destination":"/path/to/episode-media","mediaMode":"link"}'
```

The same JSON command works through MCP `command_run` or the desktop control channel.
File → Import and `filmcraft-cli import source.prproj` also dispatch to this importer. Their
default is linked media and a new `source.filmcraft-import-N` directory beside the source project.
Use the explicit command for a custom destination, copied media or frame rate. Existing destination
directories are refused. `sequence` optionally chooses the active sequence by ObjectUID or unique
name; an ambiguous requested name requires its ObjectUID. All sequences
are imported. Without it, the sequence with the most video items wins; “Sequence 01” is only
a tie-break, followed by the latest video end time.

The destination contains the unchanged `source.prproj`, `provenance.json`, and (copy mode) a
`media/` directory with full SHA-256 named files. Link mode uses original absolute paths without
writing to those files. Linked imports probe media and retain the existing size/head/tail relink
fingerprint in both the media item and immutable provenance (at most 2 MiB read by the native
host), but defer full-file SHA-256. A matching fingerprint is not proof that every byte is unchanged.
Their provenance
`sha256` is null, even when the media is online; `bytes` records its size when available.
The source project is still hashed and snapshotted immediately. Copied media is hashed and verified
synchronously. The project stores the original source paths, available import-time hashes, source
tree and warnings independently of timeline edits. Undo does not delete the saved source snapshot.

For an explicit, potentially expensive inspection of current media, use:

```sh
filmcraft-cli --project /path/to/episode.fcproj exec premiere.hashMedia '{"import":0}'
```

`import` is the zero-based provenance record index. This read-only command repeats document-path,
extension and opener checks before hashing each current imported path. It returns SHA-256/size
or a per-file error; `matchesImport` compares against an existing import-time hash, or is null
when none was recorded. A later hash cannot prove what bytes existed at import time. The command
does not backfill provenance, rewrite `provenance.json`, save the project or add an undo step.
It runs synchronously only when explicitly requested; it is not a background job or a promise
that the remaining probe/copy work cannot block the UI.

Files with workflow provenance use the distinct `filmcraft.project.workflow` format tag and
workflow version 1. Once saved with provenance, a file requires a build that understands this
tag. Older FilmCraft versions reject it, preventing them from silently discarding provenance
on save. Ordinary projects still use the upstream tag and schema. Undoing the import can remove
its provenance record; save/reopen retains records that remain in the project.
Copying just the `.fcproj` does not include its linked media or source snapshot. They must remain
available at their recorded locations; automatic portable-project conversion is outside this importer.

Supported: video/audio tracks (muted through `IsMuted` or the track fader's Mute, read by its
parameter index because its name is localised; mute automation is reported and its starting value used),
track names (`MZ.TrackName`; unnamed tracks keep generated names) and locks (`IsLocked`),
the sequence frame size from the video track group's `FrameRect` (the preview frame size is only
the fallback; Motion positions are converted against that frame),
sequence markers and master-clip markers on their media (type, colour, duration, name and comment
from each `DVAMarker` record; FilmCraft markers have no URL field, so a web link's URL is reported
and left in the original snapshot; unreadable records are skipped with a warning; past 10,000 on one
sequence or master clip the rest are skipped with a warning; a master clip's markers are read once,
however many timeline clips cut its media),
disabled clips (`ClipTrackItem/IsMuted`), generated media (black video, colour matte, bars and tone,
transparent video, counting leader) by the four-character code Premiere stores as its file path,
as FilmCraft generators that never pass the document-path policy or probe (a colour matte's colour
is not read: it imports black, with a warning; unknown codes stay offline placeholders and are reported), adjustment
layers (a video clip whose `SubClip/MasterClip` has `IsAdjustmentLayer`, over generated black video) as
one FilmCraft adjustment layer per master clip, exact tick ranges, constant positive speed, nested
sequences, still media, caption text, plain text graphics, editable Motion scale/position/rotation,
intrinsic audio controls, scalar Lumetri controls, and named native transitions. Legacy transition
records without absolute timing retain clip-edge fades. Unknown effects remain
in the original snapshot and are reported once per distinct effect name with an occurrence count,
rather than silently recreated with guessed parameters.
Fonts/styles and transition appearance are not guaranteed to match Premiere. Missing, ambiguous
or unreadable media remains visible as offline placeholders. A media path saved on the other
system family (POSIX against a Windows drive or share, or the reverse) is replaced by the stored
`RelativePath` resolved against the project's folder; a stored path that cannot be opened falls
back to that relative path with a warning. A `RelativePath` that names a drive (`..\..\C:\...`)
is not used. When neither path exists, a file in the same folder whose name differs only in letter
case (`clip.mov` stored, `clip.MOV` on a case-sensitive disk) is used, with a warning, if it is the
only such file. Document references, including that fallback, pass the
existing document-path policy before canonicalization. Both the reference and resolved file must have a
recognized media extension, and the registered media opener must accept the file before it is
hashed or copied. Non-media, refused paths and failed probes stay offline with only their original
reference recorded, no hash/size, and no copied file. Probing may read media headers or, for formats
without a streaming opener, the file itself; a recognized extension alone is insufficient.
A failure to open/read an accepted media file reports its path and the error and continues
the import in both link and copy modes. It does not attempt to change permissions or copy an
unreadable source. Destination write failures and failed copy-integrity checks still abort.

Each sequence reads `Sequence/TrackGroups/TrackGroup/Second@ObjectRef` →
`VideoTrackGroup/TrackGroup/FrameRate` as ticks per frame (254,016,000,000 ticks/second).
The nearest rate from FilmCraft's common sequence rates is selected using integer rational
comparisons, including exact 24000/1001, 30000/1001 and 60000/1001. Audio sample rates and
source-media rates do not set the sequence rate. `frameRate` explicitly overrides every sequence;
only a sequence without its own rate uses the warned 24 fps fallback. Invalid/nonpositive tick
values are rejected. Integer timeline ticks are preserved in every case. Media and nests follow
`SubClip/Clip` → `Clip/Source` → `VideoMediaSource` / `AudioMediaSource` or
`VideoSequenceSource` / `AudioSequenceSource` → `Media` / `Sequence`, resolving `ObjectRef` and
`ObjectURef` through the document's ID maps. Sequences are keyed by ObjectUID. Clip labels and
duplicate sequence/media display names do not affect a resolved reference. If the chain cannot
be resolved, an unambiguous title match is used with a warning; ambiguous fallbacks stay offline.
Subclips in the project panel (`Project/RootProjectItem` → `ClipProjectItem/MasterClip`, whose media
source `Content` holds `StartBoundary`/`EndBoundary` and `BoundariesAreHard`) are imported as FilmCraft
subclips named after their master clip, on media shared with the timeline when it is the same file;
their parent media is probed like timeline media. Unreadable or inverted boundaries skip the subclip
with a warning. Timeline clips keep referencing the parent media with their absolute source range.
The bin hierarchy (`BinProjectItem`, each container read once, at most 64 levels) is
reconstructed. Every clip project item (sequence, media file, generated media, subclip or
adjustment layer) is filed in its bin: the timeline's own item when it is the same source, a new
one for bin-only items, which pass the same document-path policy and probe as timeline media. Media
takes its master clip's name (a panel rename); timeline clips keep their own names. An item listed
in several bins stays in the first. An audio/video file is filed as its video item; a timeline's
separate audio item of the same file stays at the root. Smart/search bins are reported, not
imported; caption files and graphic canvases are not listed. Reverse/time-remapped clips, unmapped native effect parameters and unsupported caption
encodings need additional fixtures before claiming fidelity. No rendered-preview substitution
is performed.

Motion uses `MatchName` (`AE.ADBE Motion` / `ADBE Motion`) and ParameterID 1 (Position),
2 (Scale), and 5 (Rotation); localized display/parameter names are diagnostic text only. Values
come from `CurrentValue` or the static `StartKeyframe` record. Bounded source-time keyframes
for these controls are imported with linear/hold interpolation; original easing remains approximate.
Other Motion parameter IDs are not mapped. These fixes address the name/reference defects discussed in
[issue #978](https://github.com/storytold/filmcraft/issues/978#issuecomment-6254913542). See the fidelity sections below for audio and animation limits.

Text graphics match the text component's `MatchName` and Source Text ParameterID 1, then decode
the static base64 `StartKeyframeValue` using the bounded length-prefixed UTF-8 envelope heuristic
also used for captions. This is based on structural observations of user-owned XML, not a complete
text-format specification. `InstanceName` is a layer label and is never substituted for visible text.
Missing, unreadable or animated Source Text is reported and left unresolved; fonts, styling and
text animation are not reconstructed.

A project with bins or project items but no `Sequence` imports them without a sequence. Projects
with neither (settings only) report an empty-document error and create no imported project or
destination.

Input is bounded to 64 MiB compressed gzip and 512 MiB plain/expanded XML, 16,000,000 XML nodes,
256 sequences, 256 tracks per kind and 100,000 timeline items. The traversal budget is the greater
of 5,000,000 units and 64 times the parsed node count. Each visited subtree charges its node count
plus one unit per 64 bytes, including repeated references and caption payloads; increasing document
capacity does not exempt repeated work. Diagnostics are capped at 1,024 entries plus a truncation
notice. Shared link-group identifiers are limited to 4,096 bytes before per-clip copying.
Large XML still requires substantial memory and parses synchronously. Clips with nonpositive
timeline duration, non-finite speed or speed outside 0.001–1000, or inverted/unsupported source
ranges are skipped with a warning naming the clip. Other clips keep their original timeline
positions; skipped clips can leave gaps. A transition whose end precedes its start is skipped with
a warning, preserving its clip and any valid transition. A clip slipped before its source's first
frame (a negative in point) starts at the source start, with a warning naming the clip.
DTD/entity expansion, duplicate IDs, negative/non-integer timeline ticks, negative out points, non-integer source ticks, non-numeric speeds, cyclic nesting and nesting deeper than 64 levels
still produce errors. The referenced graph is validated before unsupported clips are removed,
so skipping a clip does not bypass the corruption or resource-limit checks. A failed import removes only
its newly created destination and leaves the editor project unchanged.

Open workflow issues: probing, copying and explicitly requested hashing are synchronous and can
block the caller on large inputs. Default linked import avoids the full-file SHA-256 pass;
formats without streaming openers may still be read in full for probing. Imports without an
explicit `destination` write beside the source project; the destination-prompt policy and
background-job dispatch remain follow-up work. These checks do
not ask for consent for each valid media reference, and do not verify rendered appearance.

## Regression and private-corpus checks

```sh
cargo test --locked -p filmcraft-engine -p filmcraft-interchange --lib premiere
# Explicit capacity checks: generated 150 MiB XML, 83 sequences, no private media.
# Run separately to measure each process's peak RSS (on macOS, /usr/bin/time -l).
cargo test --locked -p filmcraft-interchange --lib synthetic_150_mib_plain_import -- --ignored --nocapture
cargo test --locked -p filmcraft-interchange --lib synthetic_150_mib_gzip_import -- --ignored --nocapture
FILMCRAFT_PREMIERE_CORPUS=/path/to/local-manifest.json \
  cargo test --locked -p filmcraft-engine -p filmcraft-interchange --lib premiere_private_corpus -- --ignored --nocapture
```

The opt-in corpus manifest is a local JSON array of `{"source":"/path/to/project.prproj",
"maxOffline":0}` entries; set each offline ceiling to its recorded baseline. An optional
`"frameRate":{"num":25,"den":1}` checks that every imported sequence has that expected rate;
it does not override the file's rate. When omitted, rates are recorded without an assertion.
The test copies
projects under `target/premiere-corpus/`, imports with linked media through a headless engine
session without registering platform hardware, verifies source/project hashes and offline counts,
and saves/reopens each result. Media SHA-256 is explicitly inspected before and after save/reopen;
linked provenance must remain unhashed throughout. Each sequence's audio sample rate and explicit
A/V links are recorded and compared across save/reopen. A/V link groups contain at least one video
and one audio clip; the pair count sums video-count times audio-count per group. Its private result
JSON stays under `target/`. Only hand-written
synthetic XML is committed in the regression tests; never add corpus projects or media paths.


## Edit links and intrinsic audio controls

The fidelity additions are by [@cshapiro](https://github.com/cshapiro), contributed in
[fork PR #1](https://github.com/willluecke/filmcraft/pull/1) and
[fork PR #2](https://github.com/willluecke/filmcraft/pull/2), with contributor-authored project
observations and hand-written fixtures. Their original authorship is preserved in the commits.

Explicit Premiere Link groups are imported as sequence-local FilmCraft link IDs; clips are
never linked merely because their timing or filenames match. Each sequence also reads its
AudioTrackGroup ticks-per-sample rate separately from the video rate. Absent audio rate metadata
keeps the 48 kHz default; invalid rates are rejected before changing the project.

Volume Level (`Internal Volume Stereo` / `Mono`) and Channel Volume Left/Right control points are
mapped to FilmCraft's intrinsic audio effects, retaining source-media ticks and normalized
level values (0.17782794 is unity/0 dB in the observed native control; zero is silence).
Keyframes are bounded, validated, sorted and retained in provenance. Native audio interpolation
handles cannot be represented exactly by this mapping: interpolation uses linear dB segments
and the import report explicitly warns about the approximation. Mute/bypass automation is still
unsupported and reported. This is control-point preservation, not a claim of rendered-audio
parity. Audio parameters are selected by `Param@Index` (Volume: Mute 0, Level 1; Channel Volume:
Bypass 0, Left 1, Right 2), not by `Name`, which Premiere localises (`Nivel`, `Izquierda`, `Derecha`). Original source snapshots
and existing user-edited imports are never rewritten.


## Picture controls, transitions and generated graphics

The picture follow-up reviewed upstream FilmCraft PRs [#1694](https://github.com/storytold/filmcraft/pull/1694),
[#495](https://github.com/storytold/filmcraft/pull/495), and [#1652](https://github.com/storytold/filmcraft/pull/1652).
These are competing importers, not drop-in dependencies for this provenance workflow. PR #1694's
separation of ClipItems/TransitionItems and named native transitions informed this integration.
Its optional-control-type filter does not cover scalar controls without ParameterControlType in
the observed projects. The bounded scalar reader here is original work from user-owned XML,
using stable ParameterID values rather than localized or duplicated labels. No code or assets
from Adobe applications were read or copied.

Lumetri Basic Correction scalar values, Creative scalar adjustments and Vignette values map to
FilmCraft's native Lumetri effect. Section switches and component bypass are retained. Source-time
scalar keys are sorted, bounded and imported with explicitly warned linear interpolation. Native
range differences clamp with a warning; malformed/non-finite values and duplicate key times are
errors. Opaque curves, wheels, LUT and HSL data remain in provenance and are not reproduced;
the import warns. Matching control values does not promise identical grading algorithms or pixels.

Transition list entries are never treated as media clips. Head/Tail references to the same object
create one native transition with both sides, original timing and the named supported effect.
Leading transitions are clipped at sequence zero. Unrecognised effects use the existing warned
transition fallback. Older provenance without absolute transition fields retains legacy edge fades.
Transition effect selection currently uses `DisplayName` and FilmCraft's named-effect aliases;
localized or unknown names can use the warned fallback.

A generated Shape component identifies a native graphic canvas, not a filesystem image, so its
internal numeric pseudo-path no longer becomes a missing-media item. Opaque shape paths/appearance
remain unsupported and explicitly warned: they are not reconstructed or rasterized. Text retains
the existing default-style mapping. This removes false offline artwork, but does not claim complete
editable title/shape fidelity. Complex graphics still require further mapping or an explicit user render.

Single-sided Dip to Black/White transitions now fade across their full duration in both the CPU compositor and GPU frame plan; two-sided dips retain their midpoint colour.


### Editable Motion animation and plugin diagnostics

Position, Scale and Rotation animation now retain their saved source-time control points,
including graphics with a nonzero source origin. Positions are converted from normalized
coordinates to sequence pixels. Linear and hold segments are supported; native temporal
velocities and spatial Bezier tangents remain approximate, with an explicit warning. PR #495
already demonstrates the native parameter-ID/point mapping; this bounded reader uses the
current importer's source-time representation.

Unsupported effects retain their native match identifier in diagnostics, in addition to the
display name. A plugin can call itself “Gaussian Blur” while identifying as `AE.Impact_Blur_FX`;
it must not be silently treated as the standard `AE.ADBE Gaussian Blur 2` effect. Opaque shape
appearance, advanced color data and unsupported plugins remain in provenance; the project stays editable
and is not replaced with rendered media. These payloads are not recreated as native editable effects; their unsupported rendering is reported explicitly.
