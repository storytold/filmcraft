# The collaboration behavior suite (FilmCraft's projections)

> The shared suite every Craft app runs before its CraftHub integration is enabled (the CraftHub
> specification §18.9): each row is one automated test with a written scenario, in the family's
> behaviour-suite tradition. This file is FilmCraft's copy of the shared catalog: the scenarios
> are identical in every app, and the *projections* column is where each app writes what the
> scenario means for its own UI.

## The scenarios

| # | Scenario | The guarantee under test | FilmCraft's projection |
|---|----------|--------------------------|------------------------|
| 1 | Join by code, join by link | One argument that recognizes itself; the file opens in its own tab, other sessions untouched | A joined project opens as its own window/tab; the Timeline and Project panels stay per-document |
| 2 | Concurrent editing converges | Snapshot-hash equality after arbitrary interleavings, drops and reconnects (per seed) | Clip properties merge last-writer-wins per property; tracks and clips merge as their own objects; a track's order is **derived from the synced `start` values**, so every replica re-sorts identically |
| 3 | Read-only, with its reason | The reason is shown (view link, role without edit, region lock), never just a badge | The mode bar shows "view link" / "viewer role" / "locked region"; the locked time span tints on its track |
| 4 | Batch refused with fixes | The server's refusal carries the fixes; the client applies them and lands on the server state | A refused clip edit snaps back with a toast explaining why (a foreign region lock is the usual reason) |
| 5 | Per-user undo | My steps are mine alone; later writes by others survive my undo | Undo after a peer's edit undoes only my edit; the peer's clip stays where they put it |
| 6 | Offline, then resume as the same client | The recovery copy reattaches; unsent changes replay or land as a "(recovered copy)" beside the file | The unsent trim replays on reconnect; a conflicting copy appears beside the project |
| 7 | Cursor chat and its bounds | ≤100 characters, ephemeral, never enters the document or the journal | The bubble renders at the peer's position in the Timeline, then is gone |
| 8 | Spotlight and follow | Follow tracks view and page; any view movement of mine stops the follow | Follow tracks the peer's **active sequence and scroll**; scrolling my own Timeline stops it |
| 9 | Presence and per-app projections | Ephemeral, throttled to change-only, bounded sizes | **The playhead is the projection** (`UiState.playhead` → `PresenceMsg.playhead`, in sequence ticks — §18.4 names this for FilmCraft): peers' playheads draw on the timeline ruler; the fps resolution is *document data* (the sequence's `settings`), never a presence field |
| 10 | Region locks | Locks sync as document properties; locked regions tint; the reason is shown | **Locks are `TimelineRange`-shaped** (§6.7): one track + a tick range + a holder, riding the `locks` document property; the schema refuses a foreign clip edit whose track and time range intersect a held region |
| 11 | Contribution, review, merge | The branch lives inside the live session; merge is one undo step; each side's changes show | The review pane shows both sides' clip property diffs, Inspect-style |
| 12 | Comments and their anchors | Anchor kinds per app; comments are entries with ids and order | Comments anchor to a clip or a time point on a track (FilmCraft's native marker form) |
| 13 | Background file notifications | Delivered when the tab is not front; bounded, dismissible | The tab badge shows "3 new" for a backgrounded shared project |
| 14 | Closing a file ends its session alone | Other sessions keep syncing behind the front tab | Closing one shared project never drops another project's peers |
| 15 | The revert guard | `file.revert` is refused while its document is in a live session (including reconnect and pre-welcome) | The toast says "leave the session before reverting"; the project and its render previews are intact |

## FilmCraft's adapter surface (what the suite drives)

* **Property tables** — the synchronized fields of `Bin`, `Track`, `TrackItem` (clips),
  `Transition`, `CaptionTrack`, `Caption`, the `ProjectItem` wrapper, and the per-kind payloads
  (`MediaClip`, `Sequence`, subclip/adjustment/graphic), each with the compile-time guard: a
  struct gaining a field that doesn't sync is a build error. The `Sequence` payload syncs
  **without its four track lists** — tracks are their own objects under the sequence, clips and
  transitions under their track, captions under their caption track.
* **The tree** — the root bin *is* the file root (its name syncs as `rootName`): bins and items
  under it (and under bins), a sequence's four track lists as its children, clips, transitions
  and captions under their tracks, ordered by fractional sibling keys kept beside the project in
  the adapter. A track's *time* order is not the sibling order: `Track::sort` derives it from
  the synced `start` values, and `apply` restores that sort on every touched sequence, so the
  timeline's order converges deterministically without a broadcast.
* **The ids** — FilmCraft's six id types draw from **one monotonic counter**, so a single 64-bit
  id names any object: they ride as space-0 `NodeId`s directly, no translation map.
* **The document level** — `name`, `settings`, `nextId`, `notes`, `rootName` and the `locks`
  region set sync as whole-value LWW scalars; the LUT library, transcripts, search bins, source
  graphics and narrations sync as keyed entries with their order properties.
* **Deferred assets** — heavy media never entered the document (they are paths on `media`
  items); the one heavy embedded payload, the **LUT library**, syncs as content-addressed
  `Value::AssetRef` entries (`LutText:<id>` — SHA-256 + byte length), never inside batches: a
  put whose bytes have not arrived is skipped, not corrupted, and replays after the asset lands
  (`FilmcraftHub::offer_asset`). Rendered frames, waveforms, peak files and proxies are derived
  data that never syncs.
* **Commands** — the unified `hub.*` catalog with the v2 `collab.*` twins
  (`filmcraft-hub::commands`): same names in every Craft app.
* **Deep links** — `filmcraft://join?<code\|token>&server=<url>` (`filmcraft-hub::deeplink`).

## The enable rule

No app is enabled before its protocol core passes its simulation tests in CI for a full week
(nightly, fixed seeds) — and enabling happens through the app's own `crafthub` feature flag,
never through the core, so a regression is one flag flip away from off (§18.4).
