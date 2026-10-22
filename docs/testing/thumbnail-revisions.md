# Sequence thumbnail picture identity

Related to #2379. Media thumbnails keep their existing media-reference identity. Sequence item
pictures now use a small memo of their own picture dependencies instead of the global project
revision. Moving an outer clip leaves an unchanged inner sequence's Project and Timeline
thumbnails reusable; changes inside the inner sequence still refresh them.

The memo retains at most eight project snapshot references (not decoded pictures or media
sources). It compares shared sequence identities and reachable media/subclip/graphic items,
including nested and merged references, plus project settings, lookup tables and source graphics.
A comparison visits at most 256 dependency nodes, 1,024 track/clip references, 4,096 effect entries
and 32 nesting levels. Item names over 4,096 bytes invalidate conservatively. Missing items, cycles and limit overflows invalidate conservatively on a new snapshot.
An unchanged snapshot/context uses the memo without another dependency walk. Large projects
may therefore retain the previous behavior rather than gain cache reuse. Once the eight memo
slots are occupied, further cards use a shared conservative identity tied to the current project
snapshot, revision and media-pool context. Their frame keys still include the individual item.
This fallback stays stable while idle; cards do not evict one another and trigger idle work.
It holds only weak references and does not retain a ninth project snapshot.

Sequence identities are opaque process-local counters, not raw pointer addresses. Retained
snapshots keep shared sequence identities alive during comparison. Clearing the memo or undo may allocate
a new identity; the required invariant is a correct picture, not a particular revision number.
The memo can retain metadata from up to eight snapshots, sharing immutable sequence/lookup-table
storage with undo and background readers; it introduces no serialized project fields.

The media pool exposes a content generation for explicit source replacement/removal/clearing,
opener registration, proxy-policy changes and changes to stored-path policy. Lazy source opens
leave this generation unchanged. This catches source refreshes that do not edit a sequence.
Changing the pool clears the memo; its counter is retained to avoid reusing old frame identities.

`thumbnail_revision_ui` checks failing-before outer-only invalidation, actual cached frame reuse,
changed child pixels, undo/redo, nested captions, transitive sequence/subclip edits, missing/recreated
items, media interpretation/offline/proxy settings, source graphics/lookup tables, pool/path changes,
bounded graph fallbacks, snapshot retention and idle stability with 24 visible sequence cards. Fixtures are procedural and stay visible.

Local camera measurements use a separately tracked decoder-capable build. Compare exact binaries
with the same footage, frame, preview/layout, thumbnail settings and workload; bracket candidates
with repeated baseline runs. Keep frame-job work separate from command-to-current-monitor time
and physical presentation. Fewer redundant jobs do not by themselves establish an end-to-end
speed gain or qualify every nested workload. No private recording or screenshot belongs in this
source contribution.


Project item names are picture inputs: Metadata & Timecode Burn-in can render a sequence's name,
which lives outside its shared sequence data. A name change therefore invalidates the picture.
Burn-in also resolves shared-sequence aliases through a project-level name lookup. To avoid an
unbounded whole-project alias scan, reachable burn-in effects retain conservative invalidation
across changed snapshots; unchanged contexts remain stable. Tests verify both actual Program
pixels and thumbnail identities after ordinary and shared-alias sequence renames.
