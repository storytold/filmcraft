# filmcraft-interchange

Timeline interchange for FilmCraft: CMX 3600 EDL, Final Cut Pro 7 XML (xmeml), FCPXML,
OpenTimelineIO, AAF and OMF import and export, plus Avid ALE. Layer L2: no file I/O; media
references are strings and audio essence is supplied by the caller (the engine).

## Specifications

The AAF/OMF codecs are written from public specifications. The AAF SDK, pyaaf2, the OMF Toolkit and
other implementations were not consulted for those codecs. Where a specification leaves a layout open (how a
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
falls inside a UTF-8 character. Timeline importers use the shared converter; `tests/file_urls.rs`
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
| FCPXML | a clip without media | not read back |
| OTIO | a clip with a `MissingReference` (effects in the `filmcraft` metadata) | offline media |
| EDL | an ordinary event with the reel the reel mode gives | an ordinary clip |
| AAF | a gap | nothing |

OMF carries no video at all.

## Tests

- `tests/aaf_omf.rs`: AAF round trips at 25, 29.97 DF, 23.976 and 59.94 DF (clips, gaps,
  two-sided and one-sided transitions, links, markers, start timecodes, constant and keyframed
  gain, media markers and timecode); 512-byte sectors; breakout to mono; embedded trimmed audio
  (sample data and source offsets); separate consolidated files with a video mixdown; OMF round
  trips with embedded audio, separate AIFF files and breakout; empty sequences; truncation and
  random corruption never panic. Nested sequences: AAF round trips as compositions (nests in
  nests, a nest used twice, another frame and sample rate, breakout to mono), OMF with the
  rendered sound, the audio needs of nests, a sequence inside itself.
- `tests/uncarried_clips.rs`: every format's export report names each graphic clip and adjustment
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

## Avid catalog transfer (development foundation)

`avid_catalog::plan_scoped` consumes the standalone research reader's **schema-2 JSON**.
The native AVB entry points below now produce that same transfer contract; PMR/MDB binary
decoding remains in the separate research reader. No Python runtime or third-party binary decoder is linked or shipped. This adapter implements
our catalog contract; it is not a claim that the decoder-derived research was clean-room.

`plan` validates a bounded report; `preview` resolves sibling-bin names; `apply` transfers the
validated plan to a transaction-local project. Clips are **offline, unbound metadata placeholders**.
Current names and custom metadata are copied into native items; the `filmcraft:avid-source`
metadata field retains decoded identities, references, timing, locations and availability evidence.
A source custom column colliding with that reserved key is rejected before any edit. Original
rational rates/counts stay intact; a nonrepresentable duration is floored by less than one tick
with a warning, never silently changed from decimal 23.976 to 24000/1001.

Master identities and bin membership must agree between the decoded mob list and derived catalog;
the catalog's own count alone is not evidence of completeness. Duplicate full identities are
checked across byte-order representations. Short identities are accepted only in the recognized
little-endian representation and are never equated to full identities by this transfer adapter.

This pure adapter provides no playback, sequence translation, Avid online-state inference, AVB
synchronization or source writes. Unsupported placed objects generate warnings. The engine has
a separate command for verified managed-audio binding; direct AVB import is described below and in the [implementation plan](../../docs/avid-import-plan.md).

`avid_sequence` adds a separate strict sequence adapter for that same JSON contract. It resolves
full master identities within an explicitly selected native bin after managed component binding.
The Avid-authored HD 1080p/24 corpus covers cuts, filler, exact offsets, primary timecode, mono
routing and two-input stereo channel combiners; complete raw composition evidence is retained.
Unsupported compositions are reported by preview and reject the import batch, including unsupported
mixed-rate adapters, unknown effects and trailing filler that would shorten native duration. The adapter does
not inspect any third-party decoder implementation or decode Avid binary files. Coverage and
sample acceptance are in the [implementation plan](../../docs/avid-import-plan.md#sequence-conversion).

The format resolver (`avid_sequence/formats.rs`) also accepts recognized progressive
720p/1080p/UHD labels and explicit progressive raster dimensions, with per-sequence clocks.
Matching nominal format labels corroborate rational NTSC edit rates. Timeline counts use that
clock while motion-adapter inputs retain their source clocks; raw evidence is unchanged.
Phase 14 now verifies Avid-authored `HD 1080p/23.976`, `HD 1080p/29.97` and `2160p/25`
(3840×2160, also confirmed by its target-format attributes). Other expanded formats have
synthetic coverage. Primary TCCP flags 0/1 map to non-drop/drop-frame display while preserving
the stored frame count; unknown flags and drop-frame on incompatible rates remain unsupported. Unknown rasters
are errors in strict mode and explicit HD fallbacks in best effort. Custom pixel aspect/color
settings and interlaced field rendering remain unsupported.
The format semantics follow Avid's [High Resolution Workflows Guide](https://resources.avid.com/SupportFiles/attach/High_Res_WorkflowsGuide.pdf)
(printed pp. 114–116) and [rational-rate documentation](https://developer.avid.com/ctms/api/aa/resources/keyframes.html);
these are not AVB layout specifications.

The observed automatic motion adapter is now mapped to native Frame Sampling at real-time speed,
checking its output/source rate ratio against the bound picture clock. The 29.97-to-30000/1001
reconciliation uses an identified MXF or validated exact automatic-adapter evidence. Raw nominal
rates remain separate from the interpreted clock. The shared bounded validator lives in
`avid_sequence/rates.rs`; conflicting evidence disables offline clock inference. A single-input spatial adapter maps to Scale
to Frame Size. Warnings identify possible Avid interpolation/cropping differences and are preserved
in native metadata. Unknown effects, creative retimes and unsupported modes remain rejected.
The observed automatic audio adapter retains the original source clock, selected channel and
100% playback speed, validating source/output duration coverage. It does not establish picture
clock evidence. Strict conversion requires zero phase offset. Best effort may use the recorded
source-frame start for a nonnegative phase offset, with a warning that Avid's phase alignment is
not reproduced; this approximation never authorizes offline picture-rate inference.

The engine catalog commands also offer `mode: "metadata" | "media" | "sequences"` (default
`metadata`). `avid_catalog::plan_scoped` defers composition-omission warnings only when sequence
conversion is requested. The engine validates all requested stages on one disposable native
project copy; preview does not publish it, and import commits once. Unsupported binding or
conversion aborts the whole operation. See the [combined workflow contract](../../docs/avid-import-plan.md#commands).

The engine's `missingMedia: "offline" | "error"` option defaults to `offline`: supported managed
source descriptors can establish component structure when files are absent. The pure catalog
adapter retains only unique live SourceMob descriptor evidence, cross-checks copied descriptors,
and associates adapter clocks by full master identity. Runtime media validation and pending
picture-format hydration belong to the engine; this crate performs no file access or relinking.

Native bin preparation uses an opaque `avid_catalog::Document` from `read_native` (or
`read_native_with_checkpoint`) across catalog and sequence planning. Masters are normalized once;
the importer does not serialize the full graph to JSON and parse it again. `read_document` keeps
the 256 MiB external-report limit and independent catalog/mob validation; external JSON cannot
select the compact native representation. `native_document` remains the expanded schema-2 export
interface and allocates a separate, bounded catalog copy.

## Native AVB structural inspection (development foundation)

`avid_bin::index` reads the document header and object table, borrowing opaque object payloads
from the input. `Bin::membership` reads the observed `ABIN` version `0x0e` membership table,
bin UID, view reference and placement flags. References are serialization-local indexes, not
persistent mob identities; a user-placed entry may be a sequence rather than a master clip.
Unknown object classes remain raw. Root settings, placement bytes and header save fields also
remain raw; successful indexing does not establish that their contents are valid or supported.

Evidence: document version 4 and `ABIN` `0x0e` observed in 25 private AVBs saved by Media Composer
25.12.1.58982 (Phases 1–12). No complete public AVB specification edition is claimed. This code
was implemented from the supplied binary layouts, with pyavb 1.4.0 run externally as a black-box
comparison oracle for object counts, bin UIDs, placement counts and every membership reference.
No pyavb source was read or adapted for this module, and it adds no runtime decoder dependency.
Earlier decoder-derived research and the JSON catalog contract retain their separate provenance.

All 25 observed files are little-endian. Big-endian framing has synthetic coverage only; that
does not establish compatibility with legacy Mac files or another platform/version. Future
document/root variants are explicit errors. The parser caps native input at 512 MiB and two million
objects, checks lengths/references before allocation or access, and rejects unindexed trailing
bytes. Tests cover both byte orders, non-ASCII raw strings, truncations, corrupt lengths/counts,
dangling references, unknown classes/versions, exhaustive single-bit mutations of a small fixture,
and arbitrary input. The opt-in corpus test uses an explicit manifest, excluding deferred Phase 13.

The engine exposes this as the read-only `file.inspectAvidBin` query with `{ "path": "bin.avb" }`.
It reports structural coverage and diagnostics without creating project items. The query intentionally reports structural coverage only. Semantic decoding and native import
are separate entry points below; root display settings remain opaque.

## Native AVB semantic transfer

`Bin::decode` independently decodes the observed versions of CMPO, ATTR, SEQU, SCLP, FILL,
TCCP, FILE, MSML, MCBR, empty URLL, MDES, PCMA, CDCI, TKFX, SPED, MASK, ECCP, TRKR and SLCT.
It preserves raw strings and identifiers while exposing typed references, decimal rate fields,
descriptors and effect parameters. Each object must be consumed exactly; unknown semantic
classes/extensions fail explicitly. Unknown objects remain borrowed raw payloads in the index.
Decoding an effect's fields does not imply that its rendering semantics are supported.

SLCT group selectors retain ordered `alternatives`, per-alternative track properties and raw
selection fields in the normalized graph. Alternatives are neither sequential edits nor
simultaneous effect inputs. They deliberately stay outside generic effect bypass: the saved
active-choice semantics still need paired Avid fixtures. This is evidence preservation, not
group-clip playback support; no angle or audio source is inferred from a selector's ordinal.

Text uses pinned `encoding_rs` 0.8.42 for MacRoman. The relevant published character mapping is
Unicode's Apple ROMAN.TXT revision c02 (2005-04-05); UTF-8 locator extensions are validated
separately. Stored decimal rates remain exact mantissa/exponent rationals. UMID material fields
are normalized by their encoded byte order; no display URN or shortened ID is used as a join.
Timecode's high/low words are interpreted independently of host byte order. Synthetic tests
cover big-endian IDs/text/timecodes; real-file evidence remains little-endian only.
FILE extension 3 is a separately retained UTF-8 location, observed as both UNC and POSIX paths
in Phase 14. Its precedence over the other locator fields is unknown; it does not authorize
automatic relinking. The same bounded UTF-8 field checks apply to all supported FILE extensions.

`avid_catalog::plan_native` transfers placed master metadata, including observed usage codes 0
and 7. `native_document` expands a bounded source/timeline graph into the existing schema-2
contract. Unique live managed-source descriptors and matching MSML identities establish offline
structure, not filesystem availability. Their evidence is labeled `managed_avb_source_descriptors`;
no PMR lookup is invented. Unscanned locations remain unresolved. AMA file locators are retained,
but this native playback entry point does not bind AMA clips. Unknown descriptor classes are
explicit omissions and cannot authorize a managed binding.

The native graph has separate recursion, node, attribute and byte-expansion budgets. Cycles,
conflicting live source IDs and malformed descriptors cannot be used to infer playable media.
The engine performs cancellable background preparation, optional identity-based MXF search,
review and one atomic import using the existing binding and sequence adapters. No Python runs
inside FilmCraft. The external pyavb 1.4.0 oracle remains a development comparison tool; none of
its implementation source was read or adapted for these modules.

The explicit Phases 1–12 manifest contains 25 AVBs. Comparison covers 95,990 core objects and
14,269 location/descriptor/effect objects, checking every exported supported property, plus
1,999 placed-master metadata transfers (1,953 in Phase 6). Phases 9–12 reconstruct the tested
managed sequences; Phase 12's linked native media/timeline data agrees with the existing catalog
import. This is supported-subset validation, not a complete AVB format claim. FXPS, parameter
curves and many other object classes remain opaque, and strict conversion rejects advanced/unknown timeline structures. The native File-menu workflow
uses `plan_best_effort`: bypass equal-duration single-input effects when possible, otherwise
insert labeled offline placeholders for known intervals. Unknown timing still produces a
per-sequence error. `apply_supported` imports the recoverable compositions and returns failures.
Original sequence evidence and diagnostics remain in project metadata. Phase 13 remains deferred.
An additional opt-in Phase 14 regression validates three sequence formats, drop-frame start,
mixed-rate video/audio routing, all six managed components per bin, source offsets, warning
retention, undo/redo, project roundtrip and unchanged AVB bytes. Set `FILMCRAFT_AVID_PHASE14_FOLDER`
and `FILMCRAFT_AVID_PHASE12_MEDIA_FOLDER` to run `native_phase14_formats_timecode_and_mixed_rate_edits`.
