# Nested transform picture checks

Related to #2379. These are picture regressions for future preview-performance work; they do not implement or claim a latency fix. The existing export fixture discarded its created video TrackItem, so earlier comparisons could pass with no video and its transform block had no clip to change. The helper now inserts video before applying transforms, with explicit visible-picture assertions.

The processor-reference tests cover a shifted child that extends beyond the inner sequence frame, parent placement/scaling/rotation, reduced preview resolution, nested grouping and captions sampled at a held source time. The graphics comparison uses solid and textured generated media, uniform and asymmetric scaling, rotation and two preview resolutions. Every fixture must contain a visible picture; comparing two empty frames is not useful validation. No binary fixture or private recording is committed.

The graphics criterion is the existing per-pixel maximum color-channel difference on a 0–255 scale: the 99th percentile must be at most 6 and the mean below 1.5. A machine without a graphics adapter prints a skip message; that is not hardware qualification.

## Native camera observations

We separately tested an uninstalled source-image placement prototype over local Sony FX6 footage, following the approach discussed by @00xJS on #2379. Its reference-picture comparisons passed, including exact checked exports, but repeated native control measurements did not demonstrate the predeclared 20-percent monitor-revision latency improvement. The production prototype remains local and is not part of this contribution. The slowdown remains open.

For future performance qualification, use the same fixed camera frame, warm caches, bracket the candidate with baseline repeats, record source checksums and the exact binary, and include a flat-clip control. Record command-to-current-monitor timing separately from renderer work. No-edit control round trips and interface cadence can mask small rendering improvements; these observations are not instrumented screen-presentation timing. The original report is on Windows; our camera observations are macOS only. Private footage, sample data and screenshots stay local.
