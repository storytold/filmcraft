# Local visual search

Build the desktop app with `cargo run --release -p filmcraft --features visual-search`.
Open **Text → Search → Visuals**. Download the model when prompted, choose a sampling interval,
then **Index project visuals**. Describe a shot in English and choose **Search visuals**.
Results show a thumbnail, source time range and cosine similarity. Click a result to open its
source at that time; project marks, timeline edits and undo history are unchanged.

CLIP ViT-B/32 runs locally on the CPU. The optional download is approximately 608 MB from
LAION's Hugging Face repository, pinned to `1a25a446712ba5ee05982a381eed697ef9b435cf`.
The checkpoint's pinned model card explicitly declares MIT; its README is retained in the downloaded package. The confirmation shows the size, source and MIT licence. Filmcraft sends no footage or queries
to a server. The five model-package files are SHA-256/size verified before loading. Weights are not
bundled with the app or committed. Models live in the per-user `models/clip-vit-base-patch32-laion2b`
directory; dependencies and attribution are listed in `ATTRIBUTION.md` and `crates/vision`.

## Sampling and limits

- Default interval: one frame every two seconds. The UI permits 2–10 seconds; commands permit 2–60.
- At most 1,000 project entries, 1,000 samples per source and 5,000 samples per indexing job.
- Indexing uses original media rather than proxies, includes unused source clips, and excludes
  offline/audio-only sources. Unsupported sources produce visible warnings.
- Index/model loading and query inference run as cancellable jobs. Analysis uses separate decoder caches and releases them between sources, so it does not evict playback frames or reuse stale handles when files are replaced. Indexing pauses between samples
  while desktop playback is active. A frame already decoding/inferencing may finish first.
- Search ranks sampled visual similarity, not probabilities, exact object detections or identity
  recognition. Scores are not calibrated confidence. Short events between samples can be missed.
- Model prompts are limited to 1024 bytes and 77 tokens. Results are capped at 100 (UI default 20).
  English works best; this checkpoint does not establish multilingual retrieval parity.

This first implementation does not infer shot boundaries or track motion. A returned range is
its sampled two-second (or configured interval) source window, not a detected event's exact bounds.
No face/person identity tagging is performed. As with other visual models, fine detail, counting,
negation and ambiguous descriptions may fail; inspect the source before editing it into a sequence.

## Cache and lifecycle

Embeddings are cached under the app data directory's `cache/visual-search/v1`. Keys include the
model revision, preprocessing version, source identity/reference/video format, duration, sampling
interval and native file size/modification time. Relinked, deleted, changed or offline sources
are excluded from fresh queries; stale results cannot be opened. Cached source entries are size-
and shape-validated. A partial source index resumes if a later job permits more samples.
The disk cache evicts its oldest recognized cache files above 256 MiB; each file is capped at
16 MiB. Cache write failures warn without modifying source media. Index publication is atomic;
cancellation retains the previous index. Closing a session or replacing its project cancels its
active visual job. Project changes invalidate displayed query results; re-run the query after edits.

## Commands

All commands work through CLI, MCP and the control channel. Unless specified, `wait` is false;
use `jobs.list` and `jobs.cancel {"job":id}` for progress and cancellation.

- `visualSearch.inspect {}`: model availability/download size/licence, indexed-source/sample counts,
  warnings, busy state and the current result (without UI thumbnails).
- `visualSearch.downloadModel {"wait":false}`: explicitly download the pinned model.
- `visualSearch.index {"items":[id],"intervalSeconds":2,"maxFrames":5000,"wait":false}`:
  optional source ids, otherwise all project sources within the bounds above.
- `visualSearch.search {"query":"sunset over water","limit":20,"wait":true}`: rank indexed
  source ranges. Read the result with `visualSearch.inspect`.
- `visualSearch.openResult {"result":0}`: validate current source identity, open and seek it.

Without `visual-search`, the UI reports unavailable; the default build and WASM remain usable.
Inference/download support is opt-in and is not part of the browser build.
