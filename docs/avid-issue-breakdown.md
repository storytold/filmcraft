# Proposed issue and PR boundaries

Submission scopes: [#170](https://github.com/storytold/filmcraft/issues/170) covers the managed-media catalog; [#1049](https://github.com/storytold/filmcraft/issues/1049) covers optional modular project storage. The initial Avid importer implements the supported subset documented in [the import guide](avid-import-plan.md).

## Assessment of #170

[Issue #170](https://github.com/storytold/filmcraft/issues/170) requests optional managed media folders, background indexing, identity-based reconnection, shared-storage support and background transcoding. Its rationale also discusses monolithic project storage. These require separate acceptance criteria. Avid interoperability helps establish identity semantics but does not itself deliver a FilmCraft-managed database.

Correct the rationale before submission: AVB bins hold binary clip/sequence objects; sequences are not simply text instructions in the project file. Separate media-location indexes, bin/sequence metadata, project storage and codec work. Do not promise instant recovery or universal performance advantages without measured workloads.

The current FilmCraft checkout already has bounded media fingerprints, relink candidate scoring and folder search (`engine/relink.rs`), media-change watching, cached probing, MXF support and Project Manager consolidation/transcoding. Reuse and extend those components. A persistent media catalog, robust multi-volume identity resolution and modular saves are separate additions.

## Suggested submissions

Keep #170 focused on **Optional persistent managed-media catalog and identity-based relinking**, linking the other issues. This preserves its original central request. Alternatively use it as an umbrella only if the maintainer prefers that structure. Do not close it with the initial importer PR.

### 1. Avid bin import into native FilmCraft bins

Problem: transfer existing Avid clip organization and metadata without retaining an AVB as a working document.

Acceptance:

- Select AVBs and optional Avid MediaFiles roots; preview results and omissions.
- Create a new bin for each AVB, named from its filename; choose the first unused sibling suffix `_2`, `_3`, etc.
- Transfer master identities and authoritative current-bin metadata; preserve source metadata separately.
- Represent split MXF components as one logical source; retain missing channel positions and distinguish found media from supported playback.
- Save/reopen independently of the AVB; one undo action; source files unchanged.
- Report sequences/subclips/groups/effects outside supported coverage, rather than silently flattening them.

The initial importer combines catalog transfer, the component source/playback model, native decoding and the import UI because they form one usable import path. It does not close the persistent catalog request. Broader media binding remains a follow-up.

### 2. Avid sequence/timeline conversion

Problem: translating a bin's edited sequence requires additional semantics beyond importing master clips.

Acceptance for an initial cuts-only subset:

- Sequence identity/name, frame rate, start timecode and track identities/order.
- Preserve raw item/sequence color attributes; visual mapping may follow later if palettes differ.
- Source references, source in, edit duration, timeline placement, gaps/filler and disabled tracks.
- Correct audio channels, including paired stereo backed by separate MXFs and stereo timeline tracks, sample-accurate offsets where represented, and mixed-rate conversion rules. Structural channel-combiner wrappers cannot be discarded with optional creative effects.
- Preserve bin master references without duplicating media; unresolved sources stay explicitly offline.
- Explicit diagnostics for unsupported transitions, nesting, retimes, effects and group/multicam structures.
- Compare against a saved Avid-authored sequence plus AAF export; EDL can cross-check cuts but cannot establish full fidelity.

The initial importer may omit unsupported effects and speed changes with diagnostics rather than claiming fidelity. The observed automatic mixed-rate and spatial wrappers are translated to FilmCraft equivalents; general retimes, effect fidelity and complex sequences remain follow-ups.

Later slices: transitions and handles; markers; nested/subclip/group references; speed changes; effect translation with fidelity reports. Keep unsupported effects distinguishable from successfully translated effects.

### 3. Optional persistent managed-media catalog (narrow #170)

Problem: reusable media identity/location knowledge should survive application sessions and differing storage mount paths.

Acceptance:

- Opt-in managed roots and documented native catalog schema; ordinary loose-file workflows remain available.
- Incremental, cancellable indexing with restart/rebuild safety, bounded memory and persistent diagnostics.
- Separate clip/package/component identity from file fingerprints and paths; duplicate/ambiguous identities never auto-select an arbitrary file.
- Drive disconnect/reconnect and root remapping reconcile components without shifting track numbers.
- Measure initial scan, warm lookup, reconnect and peak memory at stated corpus sizes.
- Shared-storage readers and catalog writer ownership have explicit consistency rules. Media catalog sharing does not grant concurrent project editing.

PMR/MDB import is an interoperability input, not a requirement to write Avid databases. Queue managed ingest/transcode integration separately after reusing FilmCraft's existing jobs and codec capabilities. Keep distributed synchronization and cloud conflict handling outside the first catalog PR.

### 4. Optional modular project storage and conversion

Problem: users should be able to store/load/save independently addressable project modules while preserving native project semantics.

Acceptance:

- `File > Convert Project to Modular…` creates a validated modular destination from an existing project, keeping the original available.
- Stable item IDs and cross-module references; documented ownership, paths and versioning.
- Transactional saves, interrupted-save recovery and clean diagnostics for missing/corrupt modules.
- Lazy loading with measured open/save/memory behavior; save/reopen equivalence.
- Conversion back to a single file, plus explicit handling of external module changes.

Collaboration locks/merges are a subsequent feature, not an automatic consequence of using several files. This issue has no dependency on Avid binaries. Keep it last in our implementation queue, in its own PR.

## Relationship and submission order

Import and the media catalog can share an identity resolver. Modular storage can be designed independently and consume the same native project objects. Sequence conversion depends on verified timeline semantics, not on a media database or modular storage.

Propose the narrowed scopes together when submitting; make each implementation PR independently reviewable with its own tests and limitations. Keep persistent indexing and modular project storage out of the initial importer PR; reader research and private fixtures also remain outside the product change.
