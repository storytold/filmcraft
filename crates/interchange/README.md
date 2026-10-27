# filmcraft-interchange

Timeline interchange for FilmCraft: CMX 3600 EDL, Final Cut Pro 7 XML (xmeml), FCPXML,
OpenTimelineIO, AAF and OMF import and export, plus Avid ALE. Layer L2: no file I/O; media
references are strings and audio essence is supplied by the caller (the engine).

## Premiere projects (import only)

`prproj::import`, `import_with(..., Format::Premiere, ...)`, and the existing engine command
`file.import {"paths":["Edit.prproj"]}` read gzip-compressed or plain UTF-8
`<PremiereData Version="3">` documents. The desktop Import picker includes `.prproj`.
The native menu **File > Import From > Premiere Pro Project…** uses this parser directly.
The in-app report shows separate video/audio clip counts, missing media and conversion warnings;
its Save As button writes a native project through the normal atomic-save workflow.
No separate converter executable is needed. Import and all media links are one undo step.


There is no published complete `.prproj` schema. This is a clean-room, explicitly partial mapping
from contributor-supplied project documents, observed on 2026-10-17. No application code, bundled
artwork, presets, or third-party importer/decoder implementations were consulted. Gzip follows
[RFC 1952 (May 1996)](https://www.rfc-editor.org/rfc/rfc1952) using flate2's pure-Rust backend;
XML follows XML 1.0 via quick-xml and roxmltree. Project format exports remain unsupported;
`Format::ALL` lists the six formats supported in both directions.

Supported: all sequences and their ordered video/audio tracks (including empty tracks), file media
with stream descriptions and Unicode/relative paths, exact integer start/end/source-in times,
positive constant playback speeds, clip links, nested sequence references, work areas, static
Motion (position, scale, width, uniform scale, rotation, anchor, anti-flicker), static Volume and
stereo Channel Volume. Premiere frame/sample durations and edit times use the same
254016000000 ticks/second as FilmCraft; no frame/second float conversion is used for placement.
Transition alignment is an integer offset from the transition's start, also in ticks.

Recognized standard transitions use FilmCraft effects. Third-party transitions are explicitly
reported and substituted with Cross Dissolve (video) or Constant Power (audio), retaining the
transition interval and neighbouring clips. Their appearance, easing and private settings differ.
Animated supported parameters retain their initial value and report missing animation. Unknown
clip effects are named in the report. Unsupported source types fail conversion with an actionable
error rather than silently removing the clip.

Not transferred: original bin layout, labels, markers, captions/data tracks, generated graphics,
colour-management metadata, mixer automation, source/master effects, export settings, transition
parameter animation, reverse playback, speed ramps and third-party plugin settings. A summary
warning is always returned: this is not a promise of visual/audio equivalence. Negative/zero
speeds and unsupported source objects are refused. Compare playback against the original.
Native import probes linked media, but full codec and playback compatibility still requires
checking playback in the editor.

Hostile inputs: 64 MiB compressed/plain input, 128 MiB decompressed XML, bounded node/attribute
capacity, a streaming XML depth check **before** the recursive DOM parser (128 levels), 4096
sequences, 1024 tracks per group, no DTD/entities, checked frame sizes, finite parameters and
bounded integer times. Object definitions are indexed separately by ObjectID/ObjectUID;
duplicates and dangling references fail. Sequence cycles and nesting beyond 64 levels fail via
an iterative graph walk. No recursive object-reference resolution is used.
Global budgets additionally cap object-reference visits (100000), constructed model objects
(50000), imported text (32 MiB, charged conservatively), and report entries (4096). Text fields,
paths, identifiers and channel-layout JSON have individual limits. Shared object references cannot
amplify a small XML document into an unbounded import; exceeding a budget returns an error.

Validation: synthetic tests cover gzip/plain equivalence, 29.97 sequence vs 23.976 source timing,
48 kHz audio, source trims, 150% speed, links, Motion, transition substitution and exact alignment,
import-only detection/export refusal, saved timeline selection across conversion/native reopening,
ordered/stale/hostile saved tabs, corrupt numbers/references, nested cycles, 5000-level XML,
and every truncation and bit flip of a small project and its gzip representation under
`catch_unwind`. User projects stay outside the repository; no real project/media fixture ships.

## Specifications

Clean-room: written from public specifications only. The AAF SDK, pyaaf2, the OMF Toolkit and
other implementations were not consulted. Where a specification leaves a layout open (how a
nested sequence is written, how several video tracks share a slot), what Premiere Pro does was
read off files it exports, never from the application itself.

| Format | Document | Edition | Used for |
|---|---|---|---|
| AAF | AMWA *AAF Object Specification* | v1.1 (2005) | classes, properties (ids shared with SMPTE ST 377-1 local tags), mob / slot / segment model, sequences and transitions, operation groups, parameters, definitions, descriptors, locators, essence data |
| AAF | AMWA *AAF Low-Level Container Specification* | v1.0.1 | objects as structured storages, `properties` streams, stored forms, strong / weak reference collections and their index streams, `referenced properties` |
| AAF | AMWA *AAF Edit Protocol* | v1.0 (AMWA AS-01) | top-level composition, mob chains (composition → master → file → physical source), operation definitions (dissolve, fade to black, audio gain / dissolve), usage codes |
| AAF | SMPTE RP 224 / RP 210 registers | — | data definition, class and type labels |
| AAF | Microsoft [MS-CFB] | rev. 10.0 | the container, see [`filmcraft-cfb`](../cfb/README.md) |
| OMF | Avid *OMF Interchange Specification* | Version 2.0 (1997) | classes (`HEAD`, `CMOB`, `MMOB`, `SMOB`, `MSLT`, `TRKD`, `SEQU`, `SCLP`, `FILL`, `TRAN`, `EFFE`, `ESLT`, `CVAL`, `VVAL`, `TCCP`, `WAVD`, `AIFD`, `WAVE`, `AIFC`, locators), property and type names |
| OMF | Apple *Bento Specification* | revision 1.0d5 | container label, TOC encoding, objects / properties / types, references |
| WAVE / AIFF | Microsoft RIFF WAVE; Apple AIFF 1.3 | — | embedded and separate audio files |

The other formats' editions are listed in the module docs (`edl`, `fcp7`, `fcpxml`, `otio`, `ale`).

**Invariant — Unicode media URLs:** file URL prefix probes must not panic when their byte boundary
falls inside a UTF-8 character. Timeline importers use the shared converter; `tests/it/file_urls.rs`
checks expected Unicode paths through the public converter, resolver and EDL importer.

## AAF and OMF

`comp` holds a format-neutral mob-style model (compositions of slots whose sequences of fillers,
source clips and overlapping transitions reference media through master and file source mobs).
`aaf` and `omf` serialise it; both import back into the same model and from there into FilmCraft
sequences and media. Mapping details (what each FilmCraft feature becomes) are in the `aaf` and
`omf` module docs. Times are kept in ticks in the model; compositions use the sequence frame rate
for picture and the sample rate for sound slots, so audio edits are sample-exact.

Embedded / consolidated audio (`essence`): `audio_needs` lists the media ranges an export
references (per media item or per clip, with handles); the engine decodes or renders them and
passes `AudioEssence` (embedded PCM or a written file) back in `MediaOptions`.

Not represented: speed changes and frame holds (exported at 100 % with a report entry),
graphics and synthetic media (gaps), video effects other than transitions, clip
markers (written to the master mob, so they come back as media markers), OMF video and markers.

## Nested sequences

For the formats Premiere Pro exports (FCP7 XML, OTIO, EDL, AAF, OMF) a nested sequence is written
the way Premiere Pro 26.5.2 writes it, as seen in the files it exports for a sequence with a nest.
Premiere Pro does not write FCPXML.

| Format | Written as | On import into FilmCraft |
|---|---|---|
| FCP7 XML | a clip item holding the nested `<sequence>` (by id after the first use) | a sequence the clips use |
| FCPXML | a `ref-clip` to a `media` resource holding the sequence | a sequence the clips use |
| OTIO | a `Stack` inside the track | a sequence the clips use |
| EDL | one event: reel `AX` in every reel mode, the nested sequence's name as the clip name, its own time as source timecode (the report says the nest's edit is not in the EDL) | an ordinary clip of that name |
| AAF | a composition mob of its own (not tagged top-level); the nest's clips are source clips that point at it, at its first track of their kind. With embedded or consolidated audio the media inside the nest is prepared too (`NestNeeds::Inside`) | a sequence the clips use, read once however many clips use it |
| OMF | the nested sequence's sound, mixed by the engine with what is on the clip (`NestNeeds::Render`), as one clip named after the sequence. Without that essence: a gap, and the report names the sequence | media in the document |

A sequence that claims to be inside itself (a damaged project or file) is a gap with a report
entry in both directions; nothing recurses without a bound.

The AAF reader also accepts Premiere Pro's layout of a composition: all video tracks in ONE slot
(a nested scope with a segment per track, lowest first), and "top-level" tagged on nested
compositions too (the top-level ones are those no other composition uses).

## Graphics and adjustment layers

No format here has an equivalent of a FilmCraft graphic (title) clip or an adjustment layer, and
none of the exports brings one back. Each such clip is named in the export `Report` as a warning
(`graphic clip "Title" at frame 48 …`, one line per clip), with what was written instead:

| Format | Written as | On import into FilmCraft |
|---|---|---|
| FCP7 XML | a clip item without media (its layers as FilmCraft filter ids) | not read back; the import report names the skipped clip item |
| FCPXML | a gap on V1; left out on other tracks (a gap exists only in the primary storyline) | not read back |
| OTIO | a clip with a `MissingReference` (effects in the `filmcraft` metadata) | offline media |
| EDL | an ordinary event with the reel the reel mode gives | an ordinary clip |
| AAF | a gap | nothing |

OMF carries no video at all.

## Tests

- `src/otio.rs` (unit tests): OTIO repeats a nested sequence for every use, so compound clips that each hold
  several uses of the next are exponential (a 3 KB FCPXML nested 12 deep made the export run for minutes and grow to
  gigabytes). After 25 000 clips written inside nested sequences the rest are gaps, with one warning; an ordinary
  project is far below that and unchanged.
- `tests/it/aaf_omf.rs`: AAF round trips at 25, 29.97 DF, 23.976 and 59.94 DF (clips, gaps,
  two-sided and one-sided transitions, links, markers, start timecodes, constant and keyframed
  gain, media markers and timecode); 512-byte sectors; breakout to mono; embedded trimmed audio
  (sample data and source offsets); separate consolidated files with a video mixdown; OMF round
  trips with embedded audio, separate AIFF files and breakout; empty sequences; truncation and
  random corruption never panic. Nested sequences: AAF round trips as compositions (nests in
  nests, a nest used twice, another frame and sample rate, breakout to mono), OMF with the
  rendered sound, the audio needs of nests, a sequence inside itself.
- `tests/it/uncarried_clips.rs`: every format's export report names each graphic clip and adjustment
  layer once, as a warning; a sequence without them reports nothing; what a re-import gives back
  matches the report; the FCP7 XML import names a clip item whose file is not defined.
- `src/aaf/tests.rs`: written files checked against the required properties of every class
  written (Header, Identification, Mobs, slots, components, descriptors), weak references
  resolving into the dictionary, source clips resolving to mobs and slots, the Edit Protocol
  transition rules and sequence length arithmetic; every stored form round-trips through the
  container; a file rewritten into Premiere Pro's layout (video tracks as layers of one slot,
  nested compositions tagged top-level) reads back as the same edit.
- `src/omf/tests.rs`: the Bento label and TOC, the required properties of every OMF class
  written, reference resolution, embedded WAVE data, sequence lengths in samples.
