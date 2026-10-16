# Avid import audit and validation

This record covers the native AVB reader, catalog/sequence adapters, component media model,
MXF changes, engine transaction boundaries and desktop import/relink UI. It describes tested
coverage, not complete reverse engineering or certification for arbitrary Avid projects.
See [the workflow and limits](avid-import-plan.md) for the current product contract.

## Review focus

- Normalize full identities while preserving raw byte order and source evidence. Reject
  conflicting identities, duplicate live objects and ambiguous copies.
- Bound file reads, counts, allocations and graph traversal. Truncation, cycles, unsupported
  structures and hostile command parameters must produce errors rather than panic.
- Keep current-bin metadata, source metadata, source-component timing and timeline placement
  distinct. Validate derived catalog entries against decoded bin objects.
- Keep every logical audio slot, including missing A1 with present A2. Do not substitute the
  first available file for a complete source.
- Validate replacements using playback's identity/profile/range checks. Treat incomplete scans
  and stale project snapshots explicitly; never auto-select an ambiguous candidate.
- Apply successful import and relink operations atomically with undo/redo and save/reopen coverage.
  Failed preparation, cancellation or changed source bytes must leave the project untouched.
- Keep parsing off the UI thread and action controls accessible with expanded reports and many
  folder rows. Search evidence and applied links must be visibly different states.

## Reproduced defects fixed during development

The relevant regressions failed before these corrections and pass afterward.

| Defect | Correction |
| --- | --- |
| Catalog completeness checked only against its own count | Cross-check canonical identities and placement against decoded masters, so deleting a clip and decreasing the count cannot silently omit it. |
| Duplicate detection used raw bytes plus byte-order labels | Compare canonical material-number fields while retaining original evidence. |
| Derived catalog fields could disagree with bin objects | Validate names, metadata, rates/lengths, attributes and ordered source-track structure. |
| Media binding trusted derived references alone | Compare full IDs, source tracks, offsets and extents against the actual bin source clip before binding. |
| Zero-length source padding was discarded before validation | Validate unsupported/truncated flags, kind and rate on every child before removing permitted filler. |
| Composition/timecode completeness checks were incomplete | Validate primary timing evidence while retaining untranslated auxiliary tracks separately. |
| Ordinary Avid OP-Atom notices disabled automatic matching | Recognize only the exact validated companion-reference and opaque-packet notices; preserve all text and continue blocking on other parser warnings. |
| Repeated native references could amplify copied payloads | Charge variable payloads and normalized values to graph byte budgets in addition to node/depth limits. |
| UI search assertions could read stale labels | Require a completed search report and let the previous-frame automation registry catch up before checking labels. |
| Optional adapter clock evidence made a missing/legacy full source ID fatal to the bin | Omit unusable clock proof and leave edit diagnostics to ordinary conversion; preserve other valid evidence. |
| Clock evidence rejected more than 64 tracks despite conversion accepting 128 | Share the 128-total-track limit and test acceptance at the bound and rejection above it. |

The last UI case reproduced in a parallel release run while passing in isolation. All twelve
media UI cases subsequently passed in ten consecutive parallel runs. The correction changes
the test's synchronization, not relinking behavior or its expected results.

The separate research reader also received short-ID collision, truncated-leaf and malformed
helper-envelope fixes. Its Python boundary and PMR/MDB behavior are not part of the FilmCraft
product implementation in this PR.

## Public regressions

Synthetic fixtures cover AVB truncation/mutation, object bounds, dangling/cyclic references,
Unicode/MacRoman text, endian conversion, unsupported objects and graph budgets. Engine tests
cover invalid parameters, metadata cross-checking, bin naming collisions, independent channel
signals, partial media, wrong identities, save/reopen, cancellation and atomic undo/redo.

Relink tests include multiple roots, overlapping roots in either order, renamed Unicode paths,
duplicate copies, stale results, symlink/incomplete searches and metadata-read limits. UI tests
exercise search-versus-link guidance, explicit duplicate selection, leaving one channel offline,
preview-before-apply, cancellation and 800×600 layouts with expanded reports and 32 folder rows.

Run the repository gate from the checkout with the required Rust toolchain installed:

```sh
cargo xtask ci
```

Focused tests can be run independently:

```sh
cargo test -p filmcraft-interchange avid
cargo test -p filmcraft-engine avid
cargo test -p filmcraft-engine component
cargo test -p filmcraft-ui-egui --test media_ui
```

The gate performs formatting, release Clippy with warnings denied, release workspace tests,
dependency-layer checks, asset attribution checks and WASM compilation. Private/environmental
checks marked ignored are not silently counted as covered by this gate.

## Private corpus evidence

An explicit Phases 1–12 manifest contains 25 AVBs. Native decoding was compared with an external
pyavb 1.4.0 oracle for 95,990 core objects and 14,269 supported location/descriptor/effect objects,
checking the exported supported properties. Native metadata tests transfer 1,999 placed masters,
including 1,953 in the large sound-effects bin. Third-party decoder implementation source was
not read or adapted for the native modules.

- Phases 4/5 check current-bin names and user columns independently of media metadata.
- Phases 9–12 check managed source identities, mono/stereo routing, source offsets, gaps,
  sequence lengths and offline reconstruction.
- Phase 12's native import links six components and agrees with the earlier catalog workflow's
  media/timeline data. The sequence retains 152 frames and five clips.
- Selected Phase 12 picture decodes differ from the external FFmpeg oracle by at most one
  8-bit YUV code value; decoded PCM, seeking and timeline audio checks pass.
- Native save/reopen and one-step undo/redo pass. The user confirmed playback, the File-menu
  import and multiple-folder UI in the test application.
- SHA-256 checks retain all 25 AVBs and the ten Phase 12 source files unchanged.

These tests are opt-in. Use the explicit private manifests and report paths expected by the
ignored tests; do not scan unrelated directories or redistribute source media. The relevant
inputs include `FILMCRAFT_AVB_CORPUS_MANIFEST`, `FILMCRAFT_AVB_SEMANTIC_ORACLE` and
`FILMCRAFT_AVID_PHASE12_REPORT`. Corpus files, generated reports, screenshots, launchers and
converted projects are excluded from the repository.

Real-file evidence is currently from macOS/APFS. Synthetic big-endian cases do not establish
coverage of every Windows AVB variant. Complex feature-film bins remain deferred.

## Acceptance boundaries

The standalone research catalog's Phase 6 counts are 1,953 masters, 6,934 live MDB matches,
3,043 verified MXF identities, 1,761 complete clips and 192 partial clips. They describe
identity/availability evidence, not native playback acceptance.

Native import preserves all 1,953 masters. Without media folders it binds five offline sources;
the other 1,948 retain metadata and diagnostics. With the source media supplied, identity indexing
now precedes binding and supports a narrow full-source audio clock interpretation corroborated
by declared MXF sample ranges. This links 1,280 masters through 2,163 verified components.
The remaining 673 retain explicit diagnostics: timing (381), insufficient managed-source evidence
(192), sparse track slots (84), and decoded range overruns (16). This remains partial coverage.

The tested automatic frame-rate/spatial adapter mappings are not proof of Avid rendering fidelity.
AMA binding, general retimes/effects, complex sequences, color presentation and broad decoder
profiles remain outside this acceptance result. Nonzero adapter phases are approximated only
in best-effort mode with retained warnings; they do not authorize offline clock inference.

## Expanded import and sequence audit — 2026-10-11

The follow-up audit covers native locator extension 3, identity indexing before binding and
final replacement validation, full-source audio clock reconciliation, sequence formats/timecode,
mixed-rate video/audio adapters and best-effort recovery. Phase 13 was not inspected.
The two optional-clock defects listed above were reproduced before their fixes.

Targeted validation passed: 599 engine, 48 interchange and 31 MXF unit tests; eight opt-in native
import/relink tests; and four decoder/metadata comparison tests across the existing 25-bin oracle
corpus. The Phase 6 linked import reopens all 2,163 components, compares selected PCM samples with
external FFmpeg, and checks project roundtrip and undo/redo. Generated fixtures cover 44.1/48 kHz,
quarter-frame source units, stored padding, invalid declared ranges, ambiguous/incomplete scans,
stale final validation and source-evidence retention.

Phase 14 adds three Avid-authored bins: 1920×1080 at 24000/1001, 1920×1080 at 30000/1001 DF,
and 3840×2160 at 25/1. Each links all six managed components and retains its expected timing,
source offsets and channel routing through save/reopen and undo/redo. The user reported no
playback issues; this is not measured frame/sample equivalence to Avid. Synthetic cases cover
additional rasters/rates, long gaps, DF/NDF, unsupported flags and phase-approximation warnings.

SHA-256 checks confirm the 28 original AVBs and six Phase 12 MXFs covered by the saved manifests
are unchanged. Private tests additionally use `FILMCRAFT_AVB_LOCATION_ORACLE`,
`FILMCRAFT_AVID_PHASE6_MEDIA_FOLDER`, `FILMCRAFT_AVID_FFMPEG`, `FILMCRAFT_AVID_RELINK_PROJECT`,
`FILMCRAFT_AVID_PHASE14_FOLDER` and `FILMCRAFT_AVID_PHASE12_MEDIA_FOLDER`. Test assets stay private.
Logs and source hashes are retained in ignored `target/avid-local-followups/audit-phase14/`.

## Checkpoint gate

The checkpoint at `f569f9f` passed `cargo xtask ci`: 2,881 tests passed, none failed and 62 were
ignored; layering, attribution for 69 assets and WASM checks across 44 crates passed. Two
pre-existing gate defects (zero-softness Extract and caption-picker automation targeting) are
kept in the separate maintenance commit `f10fda5`. The Extract tolerance was not relaxed.

Local evidence is retained in ignored `target/avid-checkpoint-ci-green.log`, the targeted
`target/avid-audit-*` / `target/avid-multiroot-*` logs and the explicit private manifests. Subsequent
changes were checked separately in the final submission gate below.


## Initial submission gate

After dialog localization, explicit preview-omission counts and documentation consolidation,
`cargo xtask ci` passes on macOS 27.0.1 arm64 with Rust 1.95.0: **2,881 passed, 0 failed,
62 ignored**, plus formatting, release Clippy, layers, 69 attributed assets and 44 WASM crates.
Workspace release and WASM artifacts were rebuilt after checking the isolated maintenance branch.
The log is retained locally as `target/avid-submission-ci-clean.log`.

All twelve media UI tests also pass with offscreen rendering enabled. The inspected renders
confirm visible action buttons at 800×600 with expanded reports, long paths and 32 folder rows,
and the orange quoted Link-button guidance. These renders remain in ignored
`target/avid-submission-ui/`; the test log is `target/avid-submission-ui.log`.

The separately submitted maintenance tree passes its own complete gate: 2,807 passed, 0 failed,
41 ignored. Git commit attribution was corrected to the contributor's GitHub no-reply identity;
the tested source trees were unchanged by that metadata correction.

## Expanded import submission gate — 2026-10-11

After the media-indexing, timing, sequence-format and audit fixes above, `cargo xtask ci` passes
on macOS 27.0.1 arm64 with Rust 1.95.0: **2,899 passed, 0 failed, 64 explicitly ignored**.
Formatting, workspace release Clippy, dependency layers, attribution for 69 assets and WASM
compilation across 44 crates also pass. The two additional ignored tests are the private Phase 6
linked-import and Phase 14 regressions; both passed separately in the opt-in audit run.

The implementation and its public regressions are committed separately from the updated
coverage documentation. Full gate logs are retained locally as `target/avid-pr-update/ci.log`
and `target/avid-pr-update/ci-docs.log`.
Private media, oracle exports, generated projects and launchers are excluded from the commits.
The scope remains the existing Avid import PR; the maintenance PR and deferred features are unchanged.

## Local subclip audit corrections — 2026-10-11

Review of the eight local commits through `103917f5` reproduced four gaps with independent
synthetic catalogs: a subclip's track number was used directly against its parent's channels;
unplaced subclips referenced by a timeline were not imported; arbitrary descendant source clips
were treated as proof of a linear range; and skipped groups/effects could lose their diagnostics
when a bin had no timeline to convert.

The local corrections resolve timeline source tracks through retained subclip evidence and verify
the parent identity. Referenced subclips are kept in `Referenced Sources`, counted separately
from visible subclips. Linear-range validation now accepts direct source clips and bounded
single-child sequence wrappers with matching timing; unsupported or incomplete structures are
reported. Omission diagnostics originate in the catalog stage, independently of timeline count.
The subclip rules live in `avid_catalog/subclips.rs` rather than further enlarging catalog assembly.

The second review reproduced and corrected three further boundary cases: an attribute range
starting before its source component, nonzero or invalid track placement treated as aligned,
and negative, overflowing or out-of-parent ranges retained in a saved project. Each new regression
failed before its correction. These cases now produce explicit omissions or timeline placeholders
instead of selecting unintended media. The no-timeline omission test also checks that its warning
names the omitted item and type.

Validation on this working tree:

- Interchange library: 59 passed, 0 failed, 4 private tests ignored.
- Public subclip integration regressions: 11 passed, 0 failed; the private fixture test runs separately.
- Engine Avid checks: 48 passed, 0 failed, 15 opt-in tests ignored. The added command-level test
  uses generated, distinguishable A1/A2 PCM, checks the routed channel's samples, and covers
  placed/referenced subclips, undo/redo and project encode/decode.
- Private Phase 15/16 check: one test passed, preserving eight expected subclips per bin and
  verifying both AVBs remain byte-identical after reading.
- Release Clippy for engine/interchange (all targets, warnings denied), formatting and
  `git diff --check` pass.

The original failing counterexamples and subsequent logs are retained under
`target/handoff-review/`; the second review uses `second-review-*.log`. This is targeted validation,
not a new complete repository CI gate or
Phase 13 corpus replay. At that review the changes were still local; the integration review below
records the later submission checks. General source-viewer
and manually edited subclip channel-selection behavior, nested subclip range composition, and
multicam conversion remain separate work; the routing correction covers imported timeline edits.

## Large-bin resource audit — 2026-10-11

The Phase 13 assessment's item 5 identified 20 bins rejected by resource limits: five files above
256 MiB, three above one million objects and twelve above the 128 MiB graph accounting budget.
Native preparation now shares an opaque parsed `Document` across catalog and sequence planning,
enriches masters in place, and moves the completed graph into its container. This removes the
native mob/catalog duplicate and whole-document JSON serialization/reparsing. The expanded
schema-2 export keeps its original separation of decoded mobs and derived catalog fields.
External reports retain their 256 MiB limit, schema checks and independent catalog/mob identity,
membership and metadata checks; a JSON field cannot opt into the private native representation.

After those changes, native file/object limits are 512 MiB/two million and graph accounting is
512 MiB. Attribute accounting remains 128 MiB/one million entries, component expansion remains
200,000, and recursion/cycle checks remain. These are bounded-work/allocation estimates, not an
RSS ceiling. Media/sequence preparation checks cancellation every 4,096 indexed objects; native
member, component and attribute traversal also check it. A checkpoint error escapes track recovery even if the callback
reports it only once. Preparation still compares the original bytes after decoding and publishes
only a completed transaction.

Measurements use release builds on the same machine and an external `/usr/bin/time -l` process
per input. A 20 MiB SFX control's peak RSS fell from 1,448,804,352 bytes (1.35 GiB) to
622,673,920 bytes (594 MiB), about 57 percent. Its measured structural-test duration was 1.90 s
before and 0.73 s after; these single-run timings include cache variability. All 20 formerly
resource-rejected bins pass native decoding, catalog application and sequence planning, including
the live reel and both full-length-cut bins. Their highest measured RSS is 2,226,601,984 bytes
(2.07 GiB). This harness does not measure media scanning, playback or the desktop's total memory.
Each of those source files retained its SHA-256 across the measurement.

A fresh structural corpus run accounts for all 1,903 AVBs: **four bin-level failures**, down from
24, with **zero caught panics**. The four failures remain two unsupported field markers and two
unsupported picture extensions. Produced timelines increase from 1,017 to 1,199; two timelines
remain rejected. Among bins that previously decoded, placed-master, timeline and rejected-timeline
counts do not change. These are structural results with explicit approximation/placeholder
warnings, not playback equivalence. The earlier subclip audit's stricter linear-range validation
remains in force; this corpus run reports 1,429 placed subclips, so the older 6,109 subclip figure
must not be presented as current supported coverage.

Synthetic regressions cover an actual index above one million objects, the two-million cap,
truncation, cancellation, rejection before reading an oversized file, native/external snapshot
parity and preservation of external-report validation. Existing malformed-input and graph-budget
regressions remain enabled. Private corpus paths, manifests, hashes and profiling logs stay under
ignored `target/avid-large-bins/`; no private media are added to the repository.

The completed large-bin snapshot passed `cargo xtask ci`: formatting, release workspace Clippy
and tests, dependency layers, all 69 asset attributions and WASM checking for 44 crates. The
private engine test also imported three large bins (full-length cut, live reel and Builds),
verified preview/report equality, native save/load and undo/redo. Their outputs contained
4, 15 and 11 sequences respectively, alongside thousands of reported unsupported/offline
conditions; this does not claim complete sequence fidelity. The existing Phase 14 engine test
passed all three format/rate/timecode profiles. SHA-256 verification found no changed files
among all 1,903 corpus inputs.

## Group-selector foundation — 2026-10-11

Following the full large-bin gate, native normalization gained SLCT evidence preservation:
ordered alternatives, track properties, source offsets and uninterpreted selection fields.
They use the existing component depth, node, byte and cancellation checks. If alternative
expansion fails after the selector itself decodes, top-level recovery retains its media kind.
Alternatives are deliberately separate from
effect inputs; the sequence recovery path also refuses to bypass SLCT when an external report
uses `input_tracks`. A single choice does not prove selection semantics.

Phase 15 and 16 checks resolve each selector through a unique live internal usage-5 composition
and the correct media-kind/track pair. Both confirm picture choices A/B and audio choices A/C
on both channels. Synthetic checks cover alternative order/offsets, raw fields, missing and
cyclic references, budgets, cancellation and preservation of timeline duration/evidence without
choosing a source. The existing private Phase 15–16 subclip regression remains green.

This is preparation for group import, not a completed multicam converter. FilmCraft's current
multicam audio modes always include audio-only cameras, which cannot yet be assumed to match
Avid's per-track choices. The paired [Phase 19 procedure](avid-group-tests.md) establishes saved
picture/audio selection and an edited switch before that mapping is implemented. Avid MultiGroup
clips with changing angle availability are separately tracked. No private files are committed.

After the selector changes, all 124 interchange unit/integration tests passed (six explicit
private/oracle tests ignored by the ordinary run), as did the separately invoked Phase 15 and
16 group fixtures, workspace release Clippy and formatting. A second full 1,903-bin structural
run produced the same 1,199 timelines and unchanged clip/subclip/reference/rejection counts,
with zero caught panics and the same four unsupported bin-level failures. All 1,903 source
hashes were rechecked afterward and remained unchanged. These checks supplement the earlier
full workspace gate; they do not turn unsupported group playback into supported conversion.

The rebuilt engine then passed all 50 focused Avid tests (16 explicitly ignored), plus the
separately run large-bin transaction and Phase 14 tests. Interchange WASM checking also passed.
Sequence warning counts changed in 52 corpus bins as selector alternatives became visible in
the graph; unchanged item/sequence counts do not imply identical conversion diagnostics or
edit contents. For example, the live reel's engine report now contains 2,819 import errors
versus 2,670 before selector preservation. Detailed selector support remains necessary, and
no visual-equivalence claim is made for those complex sequences.

## Upstream integration review — 2026-10-11–12

The submission incorporates upstream `78fe0102` without rewriting the existing branch history.
The translation conflicts contained independent appended entries; both sets are retained.
The Link Media conflict combines upstream's asynchronous native dialogs with component-aware
identity checking. Avid bin and media-folder Browse actions also use that new API, including
hosts whose only picker is the asynchronous runner. Responses update stored dialog state on a
later frame. Cancelled pickers preserve preparation; closed dialogs leave the project untouched; changed folder rows and
changed component selections discard stale answers.

Two new UI regressions exercise delayed bin/folder answers, cancelled selection, folder-row
removal, closing the dialog, wrong component identity and switching from A1 to A2 while Locate
is pending. Correct A2 selection links only A2 and supports undo. All 14 media UI tests and four
upstream native-dialog tests pass. Rendered reports at 800×600 retain their action controls;
the relink view retains the distinction between a found identity and an applied link.

Four mechanical Clippy fixes simplify upstream boolean expressions and one guarded match arm;
they preserve behavior. No private fixtures, screenshots, generated reports or local handoff
files enter the submission. Group/MultiGroup playback stays outside the supported subset.

The first merged workspace test run exposed the new localization requirements: 51 Avid dialog
strings lacked Japanese translations and the Avid import menu lacked Brazilian Portuguese.
Both catalogs now include those entries, preserving every formatting placeholder. The existing
coverage checks remain enabled.

All 16 private checks passed on the merged reader/engine binaries: eight
native engine cases, the Phase 12 relink case, four decoder comparisons, subclip fixtures and
the two group-selector fixtures. This includes the three large-bin transactions, Phase 14
formats/timecode, and Phase 6's 1,953 masters, 1,280 bound masters and 2,163 linked components,
with selected PCM checked against external FFmpeg. The comparison and feature-film corpora's
1,928 source SHA-256 hashes remained unchanged. These integration checks supplement the prior
full 1,903-bin structural replay; that replay was not repeated for the UI merge.

The final merged snapshot passed `cargo xtask ci`: **3,382 tests passed, zero failed and
70 explicitly ignored**, plus formatting, release workspace Clippy, dependency layers,
69 attributed assets and WASM checks for 49 crates. All 25 localization unit tests also
passed in a focused run. The release rebuild shared the machine with an unrelated build
for part of validation, so its elapsed time is not a reader-performance measurement.
This is the submission checkpoint; subsequent feature work waits for review of the existing PR.
