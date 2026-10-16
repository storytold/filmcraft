# Avid bin import: workflow, contract and remaining work

FilmCraft reads an AVB once and transfers supported contents into native project data. It does
not embed the AVB, synchronize with it or write back to it. Media remains in its original
locations; save the imported project as `.fcproj`.

This is a supported-subset importer. It does not implement persistent media databasing,
modular project storage or a complete Avid timeline renderer. Those boundaries are described
in [the issue/PR breakdown](avid-issue-breakdown.md).

## Desktop workflow

1. Choose **File > Import Avid Bin…** and select one `.avb`.
2. Optionally choose managed MXF folders. **+ Add folder** adds another location, including
   another drive. Selecting a parent includes its numbered subfolders. Leave the list empty
   to import supported sources offline.
3. Choose **Read bin**, then review the clip/sequence counts and conversion problems.
4. Choose **Import** to apply the reviewed snapshot in one undoable edit.

There is one import action for all contents. Recoverable clips and sequences are retained even
when other items fail. A report with conversion problems remains open after import. Technical
reader notices are shown separately from errors; a notice alone does not imply missing media.
Reports and folder lists scroll while the action buttons remain accessible.

Each AVB creates a bin named from its filename. Name collisions use the first free sibling
suffix: `Sound Effects`, `Sound Effects_2`, `Sound Effects_3`. Existing bins are never merged or
overwritten implicitly. Multiple-AVB selection is not yet available in this dialog.

Avid cut bins routinely hold a sequence whose master clips live in other bins. The AVB still
contains those masters, unplaced, so a sequence transfer imports them into a **Referenced
Sources** sub-bin of the new bin and reports how many. The bin itself keeps listing only what
Avid placed in it. FilmCraft binds media to an item rather than to a timeline clip, so without
those items a converted timeline could neither play nor be relinked. Metadata and media
transfers are unchanged: they import placed bin contents only.

A rendered motion effect transfers as the baked clip it is. Rendering an effect produces media of
its own, and the effect points at that precompute, so the bin shows the effect at its retimed
length bound to its own render and playing at normal speed: the retime is already in the media, and
none is applied on top. The master it was made from is a separate source, at its own length. An
effect that has not been rendered has no media to show and is reported rather than imported.

Only timelines become sequences. A bin's placed compositions also include subclips, saved effect
templates, group clips and motion effects; each is reported by kind and count rather than
converted into a sequence item that is not a sequence. Saved effect templates and group clips
are reported as unsupported even when the bin contains no timelines. Motion effects transfer
only when their render can be claimed as described above; otherwise their omission is reported.

Subclips transfer as subclips. A placed Avid subclip becomes a FilmCraft subclip restricting the
master it points at, and a sequence that cuts from one keeps referencing it. Avid records a
subclip's range in two forms and both are read; a range reaching past the end of the master it
restricts is shortened to that master and reported. A subclip binds media through its master, not
on its own. A timeline may also reference an unplaced subclip: these dependencies are retained
in `Referenced Sources`, with separate `referenced_subclips` report counts. Unused unplaced
subclips are not added. Sequence conversion resolves each subclip track through its recorded
parent-track mapping; a missing or ambiguous mapping produces a reported placeholder. The
range and routing evidence survive save/reopen. This routing applies to imported timeline edits;
FilmCraft's general subclip source-viewer/channel-selection model has not been extended here.

A plain range requires a direct source clip or single-child sequence wrappers that preserve rate,
kind and duration. Repeated intervals, unknown effects, truncated components, incompatible
track offsets/rates and missing or ambiguous track maps cannot establish one linear subclip.
They are reported by name instead of silently flattened. Nested subclip range composition and
multicam conversion remain outside this correction.
Ranges must stay within the component's exposed interval. Sequence conversion also rechecks
saved subclip ranges for negative starts, overflow and parent bounds before using them.

Imported sources use FilmCraft's **Link Media…** dialog when components need reconnecting.
Search only finds candidates. Choose **Link N files** to apply the selected replacement and,
when enabled, other unique matches. Duplicate copies require an explicit choice. Leaving A1
offline preserves A2 in its original channel slot.

## Supported data and playback

- Placed master names, current-bin user metadata, full identities, source descriptors and raw
  provenance are retained separately from media availability.
- Native decoding covers the observed membership, attributes, source references, descriptors,
  locators and structural timeline wrappers. Unknown payloads remain inspectable; unknown
  semantics are not silently treated as supported effects.
- A logical source may contain independently identified picture and mono PCM MXF components.
  Missing channels retain their positions and render silence. Missing picture displays an
  offline slate while usable audio can still play.
- Mono and paired-stereo channel grouping are preserved in the tested managed-media profiles.
  Source-component track numbers, logical audio slots and timeline track identities remain
  distinct.
- Save/reopen does not require access to the original AVB. Raw evidence and conversion diagnostics
  remain in project metadata; undo/redo restores the complete import.

Filesystem presence does not establish Avid's own online status. FilmCraft distinguishes an
identity match, a supported decoder/profile and a usable component binding.

### Sequence conversion

The validated subset includes cuts, gaps/filler, source offsets, mono/stereo audio and the
observed channel-combiner wrappers. The importer attempts every composition:

- Supported source intervals retain their timeline positions and durations.
- An unsupported single-input effect can be bypassed only when its duration and rate agree
  with its input; the omission is reported.
- Known but unsupported intervals become labeled offline placeholders. A composition with a
  trustworthy clock and extent can be retained as a placeholder sequence.
- Unknown or inconsistent timing is reported per composition without blocking other contents.

The tested automatic mixed-rate adapter maps to FilmCraft Frame Sampling at 100% playback
speed. The tested spatial adapter maps to Scale to Frame Size. Avid's cadence, interpolation,
crop/reformat treatment and effect rendering may differ. This is not a fidelity claim.

Original color attributes and auxiliary tracks are retained as evidence, but visual color
mapping, markers, mixer automation, creative retimes, transitions, nesting and complex effects
are outside the validated subset. Sequence audio output currently uses FilmCraft's 48 kHz
stereo defaults.

## Identity and timing rules

Managed MXF matching uses complete 32-byte file-package identities, normalized according to
encoded byte order. Filenames, display URNs, shortened IDs and search result order do not select
replacements. Candidates are reopened and verified against the expected component before apply.

Stored decimal AVB rates remain preserved as decoded evidence. Playback uses a verified rational
clock where supported. For the observed Phase 12 adapter, output rate `24/1` and speed ratio
`1001/1250` independently establish source rate `30000/1001`, corroborating its MXF. That permits
the same source clock while offline without guessing from the decimal `29.97` alone.

Avid's [published rate convention](https://developer.avid.com/ctms/api/aa/resources/keyframes.html)
maps 29.97, 23.976 and 59.94 to their standard rational clocks. That documentation does not
specify every AVB field's encoding. Original counts, component rates and source/timeline extents
are kept separate; unexplained rates do not receive a blanket NTSC substitution.

## Bounds, cancellation and atomicity

Native AVBs are capped at 512 MiB and two million indexed objects; object payloads remain borrowed.
The native import pipeline shares one parsed snapshot, normalizes each master once and avoids
whole-document JSON serialization/reparsing. External JSON reports retain their 256 MiB limit
and independent catalog/mob cross-checks.

Native graph expansion has a 512 MiB accounting budget, with separate limits of 200,000 component
expansions, one million attributes and 128 MiB of attribute accounting. These are work/allocation
estimates, not a process RSS ceiling; the budgets are cumulative across the bin. Depth/cycle checks
remain enforced. Malformed references and truncated payloads produce errors. Parsing runs in a
guarded background job and makes no project edits. Native media/sequence preparation checks
cancellation during indexing and graph/attribute expansion.

Up to 32 media roots share a limit of 32 directory levels, 1,024 directories and 20,000 entries.
Canonical directory keys deduplicate repeated, aliased and overlapping selections. Distinct file
copies retain their ambiguity. MXF metadata reads and diagnostic output are bounded.

Incomplete searches, unreadable paths, skipped symlinks or exhausted limits prevent automatic
linking. An explicit, independently verified replacement can still be chosen through Link Media.
Cancellation leaves the project untouched. Preparation rereads and compares AVB bytes after
conversion, including same-size changes, and rejects changes to the destination project before
applying the snapshot.

## Commands

The desktop action uses these engine commands; automation uses the same transaction path:

| Command | Contract |
| --- | --- |
| `file.prepareAvidBin` | `{path,parent?,mediaFolders?:[string],wait?}` starts preparation. |
| `file.avidBinPreparation` | `{job}` returns completion state and the prepared report. |
| `file.importAvidBin` | `{job}` commits the reviewed snapshot once. |
| `file.inspectAvidBin` | `{path}` inspects container and membership without importing. |
| `media.searchComponents` | Searches managed components by identity. |
| `media.componentSearchResult` | `{job}` retrieves search evidence without applying it. |
| `media.applyComponentSearch` | Applies an explicit choice and optional unique companions. |
| `media.relinkComponents` | Validates and applies explicit component paths atomically. |

Folder paths must be existing absolute directories. The legacy `mediaFolder` string remains
accepted, but cannot be combined with `mediaFolders`. Editing a source or folder field invalidates
the preview. The native default attempts all clips and sequences; restricted modes remain for
research tests rather than separate UI tabs.

`file.previewAvidCatalog` and `file.importAvidCatalog` are development interfaces for the standalone
reader's schema-2 JSON. Their explicit mode is `metadata`, `media` or `sequences`; their stricter
binding/conversion behavior is separate from native best-effort import. Python decoders are
external comparison tools and are not invoked or distributed by the FilmCraft product.

## Project compatibility

The project schema advances from 15 to 18: audio components, combined picture/audio references,
and pending picture-format verification. Migrations read the intermediate audio-only form.
Older FilmCraft builds reject these newer project schemas rather than silently discard bindings.
Proxies, generic Make Offline, collection/consolidation and reference-based interchange export
have explicit limitations for component sources; unsupported operations must not fall back to
an arbitrary component as though it were the whole source.

## Validation and limits

See [the audit record](avid-code-audit.md) and the
[interchange README](../crates/interchange/README.md#native-avb-semantic-transfer) for tests,
corpus coverage and decoder provenance. Private media, projects, screenshots and reports are
not included in the repository. Public regressions generate their own fixtures.

The large Phase 6 sound-effects bin retains all **1,953 masters**. With its media folders
supplied, **1,280 masters / 2,163 components** link to verified paths. The remaining **673**
retain metadata and explicit binding diagnostics: 381 fail remaining exact-timing requirements,
192 lack sufficient managed-source evidence, 84 have sparse audio slots, and 16 exceed decoded
media ranges. Without media folders, five bind as offline sources and 1,948 retain diagnostics.
This is partial native playback coverage. The standalone reader's 6,934 live MDB matches,
3,043 verified MXF identities and 1,761 complete / 192 partial clip counts describe catalog
availability, not this playback path.

### Managed audio clocks and media search

Selected roots are indexed once before binding. Final path validation reuses this bounded
index, requires a complete scan and one copy per identity, and reopens candidates through the
same identity/profile/range checks used by playback. Duplicates, unreadable files and cancellation
do not authorize automatic linking. Blocking diagnostics remain visible when routine reader
notices exceed the report limit. The no-folder path and standalone catalog commands retain
their existing behavior.

A narrow clock rule supports complete managed audio masters recorded at 23.976, with zero-offset
direct audio references at 23.976 or four units per frame at 95.904. Every channel must have an
identity-matched mono PCM MXF with zero origin/start and enough stored samples to back its declared
range. The declared range must equal the whole audio samples in the candidate 24000/1001 master
duration. This combines Avid's nominal-rate convention with independent file evidence; it is not
a blanket decimal-rate substitution. Master duration remains exact, while component lengths end
at declared whole samples. Raw rates/counts and per-component interpretation evidence are retained
and the preview reports the interpretation. Arbitrary offsets, overruns and essence padding are
not inferred.

The distinction matters in the supplied media: one file stores 248,443 samples but declares
248,311; another stores 88,289 but declares 88,288. Native and external FFprobe checks agree on
the declared ranges. AVB descriptor lengths, stored samples and playable ranges remain separate.
The MXF reader also recognizes the legacy AAF picture DataDefinition in this corpus and exposes
original material-track rates/durations separately from converted file-package edit units.

Generated tests cover 44.1/48 kHz endpoints, quarter-frame units, source padding, invalid ranges,
incomplete/duplicate searches, stale final validation and retained evidence. The private Phase 6
regression reopens all 2,163 components, samples 12 clips, checks undo/redo and project roundtrip,
and verifies unchanged AVB bytes. `FILMCRAFT_AVID_FFMPEG` enables exact PCM comparisons for those
clips. The first debug CLI measurement, including JSON reporting and project save, was 11.24 s
and approximately 964 MiB peak RSS; this is not a release-performance claim.

### Sequence formats and timecode

Each composition resolves its own format label and recorded edit rate. Recognized 720p, 1080p
and UHD labels establish the raster; explicit progressive dimensions distinguish DCI apertures
and custom sizes. Bare `4K` remains ambiguous. A matching fractional-rate label corroborates its
rational clock (for example 23.976 and 24000/1001), used consistently for cuts, gaps, offsets,
duration and placeholder recovery. Source clocks remain separate from timeline clocks, and
original composition evidence is retained unchanged.

Conflicting labels fail strict conversion. Best effort retains the recorded clock with a warning
and disables conflicting label evidence for offline source-rate inference. Unknown rasters use
an explicit 1920×1080 fallback warning. Pixel aspect ratio defaults to square and color settings
retain FilmCraft defaults. Custom-format, anamorphic and HDR metadata are not fully interpreted.
1080i labels identify field rates: best effort retains a frame clock and raster with a
progressive-rendering warning; strict interlaced conversion remains unsupported.

Primary timecode uses its nominal base (30 at 30000/1001). Observed TCCP flags 0/1 map to NDF/DF
without reinterpreting the stored starting frame count; 107892 is `01;00;00;00` at 29.97 DF.
Unknown flags and DF on incompatible rates are unsupported. Sequence conversion and optional
clock evidence share a 128-total-track bound, including auxiliary tracks. A source without a
valid full identity contributes no optional clock proof; ordinary conversion still diagnoses
unsupported edits without blocking usable bin contents.

The observed constant, single-source `EFF_TIMEWARP.AUDIO_TIME_WARP` adapter retains its original
sample clock, source selection and 100% playback speed. Source/output ratios, coverage, component
bounds and channel identity are validated. Audio adapters never authorize picture clock evidence.
Strict conversion requires zero phase offset. Best effort can preserve recorded source-frame
starts and output lengths for nonnegative phase offsets with an explicit approximation warning.
It does not reproduce Avid's phase alignment and cannot establish an offline source clock.

Public semantic references are Avid's [rational edit-rate convention](https://developer.avid.com/ctms/api/aa/resources/keyframes.html)
and [High Resolution Workflows Guide](https://resources.avid.com/SupportFiles/attach/High_Res_WorkflowsGuide.pdf),
tables on printed pages 114–116. These describe format semantics, not AVB serialization.

### Phase 14 acceptance

Three additional Avid-authored bins import three masters and one sequence each, with all six
components linked from existing Phase 12 media. Each sequence has two picture edits and two edits
on each of A1/A2, without offline placeholders or conversion errors:

| Bin | Raster / exact rate | Length in frames | Start timecode |
| --- | --- | --- | --- |
| 1080 2398 Bin | 1920×1080, 24000/1001 | 686 | 01:00:00:00 NDF |
| 2997 drop frame | 1920×1080, 30000/1001 | 1299 | 01;00;00;00 DF |
| UHD 25 Bin | 3840×2160, 25/1 | 1083 | 01:00:00:00 NDF |

Native decoding and the external pyavb oracle agree on composition lengths, decimal edit rates,
timecode flags/bases and start counts. FILE extension 3 retains an additional bounded UTF-8
location, observed as UNC and POSIX paths; path precedence is unknown and does not enable AMA
binding. The UHD sample independently records 3840×2160 and 25/1 in `_TGTFORMAT_ATTRLIST`.
Other supported label spellings and formats have synthetic coverage pending real-file evidence.

The private regression checks settings, every edit boundary, source starts, routing, linked
components, undo/redo, roundtrip and unchanged AVBs. Public tests cover long gaps without drift,
mixed rates, independent formats in one bin, DF/NDF, malformed inputs, channel clocks, bounds,
phase warnings and disabled offline inference. The user reported no playback issues on
2026-10-11. This is practical acceptance, not a measured frame/sample equivalence claim; the
nonzero phase offsets observed in the 23.976 sample (2, 109 and 55) retain their warnings.

## Remaining work

The current submission boundary is the existing Avid import PR: native bin import, supported
sequence/subclip transfer, component relinking, large-bin resource fixes and selector evidence
preservation. Feature development pauses after this update while maintainers review it; group
playback and the other items below remain follow-ups rather than expanding this submission.
Persistent Avid-style media databasing and modular project storage remain separate work. A shareable Avid-authored fixture project is deferred until a maintainer requests
it. No routine database rebuilds or additional screenshots are needed for this tested subset.

- Test sequences containing grouped media (group clips/multicam): selected angles and audio,
  synchronization, edit boundaries, relinking and unsupported-structure diagnostics.
  Native SLCT alternatives and raw selection fields are now retained for that work. The
  [group test procedure](avid-group-tests.md) isolates saved picture and audio choices. Avid
  MultiGroup clips with changing available angles across time remain a separate coverage item.
- Establish exact nonzero adapter phase semantics; preserve current approximation warnings.
- Examine remaining audio timing profiles, sparse channel slots and missing live-source evidence
  separately, preserving missing A1 independently from present A2.
- Validate custom raster/rate/field metadata from `_TGTFORMAT_ATTRLIST`, plus PAR, HDR/color and
  field rendering, without treating defaults as faithful transfers.
- Add AMA binding/remapping, broader codecs/descriptors, Windows AVB samples, multiple-bin
  selection, color presentation and further timeline semantics in separately validated work.
- Use Phase 13 as the robustness and resource-use corpus. Comprehensive interpretation of its
  complex effects and grouped media remains separate from large-bin structural import.
- Close the four remaining structural bin failures (unsupported field markers/picture
  extensions), with small synthetic regression cases. Large-bin resource rejections are
  resolved for the measured corpus; that does not close these format gaps.
- Extend subclip handling separately for nested ranges and AutoSync sources, and reconcile
  imported per-track mappings with the general source viewer and subsequent editing commands.
- Investigate unsupported offline picture descriptors, limit/group very large diagnostic
  reports, and narrow referenced-source imports to the dependencies actually required by
  placed content. Preserve useful error details and deterministic identity matching.
- Continue the standalone reader's native MDB migration using existing samples and comparison
  evidence. Keep disposable AVB write probes and PMR/MDB encoding research secondary; production
  writers and database generation are not requirements for bin import.
- Address review feedback on the existing PR before opening further feature PRs. Each update
  needs an audited diff, coherent commits, current coverage/limitations documentation and the
  required validation gates.

Persistent media cataloging follows importer work as its own feature. Optional modular storage
and `Convert Project to Modular` remain last: independently addressable bins/sequences, robust
saves/recovery and bounded loading are intended to improve reliability on feature-film-scale
projects. Neither project modularity nor persistent cataloging is implied by importing an AVB.

Research artifacts, generated projects and test launchers remain in ignored
`target/avid-local-followups/`. The [audit record](avid-code-audit.md) distinguishes private
sample evidence, targeted regression checks and complete repository gates.
