# Nested thumbnail reuse and transformed sequence pictures

Sequence item thumbnails use bounded dependency identities as described in
[thumbnail-revisions.md](thumbnail-revisions.md). Reusing an unchanged inner picture removes
background jobs after an outer-only edit. That does not by itself establish faster monitor updates.

For a plain, fully opaque Normal-blend nest with matching dimensions and color settings, the
renderer can keep a transformed nest as a clipped source-sized working image and let the compositor
place it. It avoids transforming that image into a full output-sized intermediate canvas on the
processor. The existing identity-nest path remains direct layers; unsupported effects, masks,
rotation and other guarded cases keep their reference fallback.

The transformed path follows the reference mip-halving choice, then adds a transparent one-texel
border so edge samples do not clamp opaque pixels into transparent space. `PlanLayer::prefiltered`
marks that working image so the graphics processor does not add another sampling pass. Ordinary
source layers retain their existing sampling. Allocation sizes are checked and bounded; a rejected
working image falls back. No serialized project field or decoding behavior changes.

Generated tests use visible, nonuniform pictures and compare the group result with the reference,
including fractional placement, reduced previews, minification, captions and guarded fallbacks.
Thumbnail tests additionally verify cached picture reuse, changed dependencies, edit history, pool
refreshes and stable idle keys when more cards are visible than the memo can retain.

Native measurements must record actual display assignment and refresh rate. Keep the whole window
on the same display with the same size, preview, footage, frame and thumbnail settings for both
builds. Retain source/build fingerprints, exact exported-picture comparisons and source-integrity
checks. A private matched placement scaffold may control the experiment but must be identical on
both sides and disclosed separately from the production patch; no hard-coded display placement is
part of this contribution.

Use repeated baseline/candidate/baseline brackets with acceptance written before measurement. Keep
invalid runs and their rejection reason, and retain valid results that miss the target. Separate
completed-job work from command-to-current-monitor time, which includes control requests and
interface update cadence. Neither measurement establishes physical display presentation latency,
sustained playback capacity or support for every camera mode. Camera integration on a separately
tracked decoder-capable base does not establish decoding support on another source tree. Private
recordings, projects, derived pictures and captures stay outside the source contribution.


The native frame server may also retain the clipped inner working picture across outer-only
edits. This host-owned cache has two entries and a 64-mebibyte byte limit; job-local staged
pictures have the same limits. Those are additional bounded pictures, not a total process-memory
limit. Entries retain immutable images and project metadata, not open media sources. No cache
lock is held while rendering. Exact sequence time, working scale, effects, depth, source pool and
generation, catch-up margin and transitive picture dependencies determine reuse. Ordinary paused
monitor requests have a catch-up margin too; treating every such request as playback would
incorrectly disable reuse. Draft and speculative prefetch work do not populate this cache.

The renderer's optional provider hooks default to uncached behavior, preserving the reference and
export paths. Failed-source, pending or cancelled jobs do not publish staged pictures. A completed
plan must finish upload preparation before publication; a changed source generation discards its
writes. Oversized or invalid images are not retained. Existing nested clipping, caption, mip,
border and fallback rules still apply to every new outer placement.

`nested_picture_reuse` requests actual monitor plans from a counted nonuniform generated source.
Its outer-only regression fails before reuse because a second inner frame is fetched; after reuse,
the newly positioned outer picture still matches a fresh reference while the inner frame is not
fetched again. Additional cases cover inner changes, undo/redo, time, resolution, draft quality,
source/proxy changes, captions and recovery after pending/cancelled work. Internal resource checks
verify publication boundaries, pool/context isolation, malformed-image rejection and eviction.
These tests do not replace repeated uninstrumented native performance and real-camera checks.

The frame server owns the completed cache strongly. Idle worker state keeps only a weak reference;
in-flight jobs may hold it until they finish. Dropping the server therefore releases its completed
picture snapshots instead of leaving the new cache pinned by detached idle workers. The lifetime
regression observes the actual project snapshot through a weak reference after dropping the app;
it fails before this ownership correction. General worker-thread shutdown is outside this change.

Metadata burn-in may read sequence names outside the immutable sequence payload, including
shared aliases. Such pictures invalidate conservatively when the project snapshot changes; the
cache does not infer complete dependency identity from a sequence pointer alone. Item names and
bounded effect inspection participate in the same validation used by thumbnails.
