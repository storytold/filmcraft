# filmcraft-interchange

Timeline interchange for FilmCraft: CMX 3600 EDL, Final Cut Pro 7 XML (xmeml), FCPXML,
OpenTimelineIO, AAF and OMF import and export, plus Avid ALE. Layer L2: no file I/O; media
references are strings and audio essence is supplied by the caller (the engine).

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
| FCPXML | Apple [FCPXML Document Type Definition](https://developer.apple.com/documentation/professional-video-applications/document-type-definition) | 1.10 (2021) | standalone `video` asset references, source selection and clip timing |

The other formats' editions are listed in the module docs (`edl`, `fcp7`, `fcpxml`, `otio`, `ale`).

**Invariant — Unicode media URLs:** file URL prefix probes must not panic when their byte boundary
falls inside a UTF-8 character. Timeline importers use the shared converter; `tests/it/file_urls.rs`
checks expected Unicode paths through the public converter, resolver and EDL importer.

## Absurd frame rates and start timecodes

Importers accept frame rates up to 1 000 000 per second (above video rates and PCM audio sample
rates such as 192 000 Hz, which OTIO uses as a rate). An FCP7 `timebase` is clamped to it and an OTIO rate beyond it
uses the default, each with a warning (DSD audio at 2.8 or 5.6 MHz is beyond it, so an OTIO DSD clip is retimed at
the default rate and reported). Also, a sequence
or media rate beyond it (an FCPXML `frameDuration` of `1/9223372036854775807s`) is reset to the
default with a warning: such a rate overflowed the frame arithmetic of every export. Sequence and
media start timecodes beyond 360 000 000 000 frames are reset the same way. Covered by
`tests/it/rates.rs` (including the warnings), `src/common.rs` (`rates`) and `tests/it/fcp7.rs` (`absurd_start_timecode_is_bounded`).

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

## FCPXML standalone video

Asset-backed `video` elements import as picture-only clips, including Final Cut slideshows whose
still-image assets have zero duration. Clip durations, source times, media paths, connected lanes,
adjustments, markers and transitions use the existing timeline import mapping. The engine links
available images and exposes missing paths through Link Media; saving and reopening preserves them.
Effect-backed generators and video sources other than the default `srcID="1"` are skipped with a
warning.

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
- `tests/it/fcpxml.rs`: standalone still-image clips retain timing, media paths, markers and
  transitions; AV assets import picture only, including connected clips and adjustments;
  unsupported references and source selections report warnings without panicking. The engine's
  `interchange_auto_points_tests` also covers missing-image import, save/reopen, relink, undo/redo
  and rendered pixels throughout the clip, plus automatic linking when the image exists.
- `tests/it/aaf_omf.rs`: AAF round trips at 25, 29.97 DF, 23.976 and 59.94 DF (clips, gaps,
  two-sided and one-sided transitions, links, markers, start timecodes, constant and keyframed
  gain, media markers and timecode); 512-byte sectors; breakout to mono; embedded trimmed audio
  (sample data and source offsets); separate consolidated files with a video mixdown; OMF round
  trips with embedded audio, separate AIFF files and breakout; empty sequences; truncation and
  random corruption never panic. Nested sequences: AAF round trips as compositions (nests in
  nests, a nest used twice, another frame and sample rate, breakout to mono), OMF with the
  rendered sound, the audio needs of nests, a sequence inside itself.
- `src/fcpxml.rs` (unit tests): a `<format>` whose `frameDuration` has a zero or negative part (`100/0s`,
  `0/100s`, `-100/2500s`) gets the default frame rate and an `invalid frameDuration` warning in the report
  instead of a zero rate, which made the export divide by zero once the sequence had a start timecode or a marker.
- `tests/it/uncarried_clips.rs`: every format's export report names each graphic clip and adjustment
  layer once, as a warning; a sequence without them reports nothing; what a re-import gives back
  matches the report; the FCP7 XML import names a clip item whose file is not defined.
- `src/aaf/tests.rs`: written files checked against the required properties of every class
  written (Header, Identification, Mobs, slots, components, descriptors), weak references
  resolving into the dictionary, source clips resolving to mobs and slots, the Edit Protocol
  transition rules and sequence length arithmetic; every stored form round-trips through the
  container; a file rewritten into Premiere Pro's layout (video tracks as layers of one slot,
  nested compositions tagged top-level) reads back as the same edit.
- `src/common.rs` (`scaled_saturates`) and `tests/it/edl.rs`: a duration read from a file times an integral speed saturates
  instead of overflowing (an EDL with a record-out timecode of 2147483647 hours and an `M2` speed change panicked).
- `src/omf/tests.rs`: the Bento label and TOC, the required properties of every OMF class
  written, reference resolution, embedded WAVE data, sequence lengths in samples.
