# FilmCraft Roadmap

**Stage: alpha** · next: beta, ~19 points of ready-for-real-work and ~400–700 h away, plus opening Premiere's `.prproj` ([why](docs/target-app-parity.md#stage); passes the [core-workflow gate](docs/roadmap.md#alpha-gate))

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (full re-measure against Premiere Pro 2026 26.5.2; scorecard, gaps, milestones and localization moved into docs/ per the craftrules progress-docs standard) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

Progress toward parity with Adobe Premiere Pro. This page is the summary; the detail is in
[docs/target-app-parity.md](docs/target-app-parity.md) (assessment, methodology, calibration),
[docs/gaps.md](docs/gaps.md) (the ranked work list: **agents, read it before choosing work**) and
[docs/roadmap.md](docs/roadmap.md) (milestones and current focus).

## Headline numbers

| | Value | How |
|---|---|---|
| **Feature breadth** | **~86%** | Does each Premiere feature exist? Menus 92% (measured: label diff against Premiere 26.5.2's menu dump), effects / transitions / audio effects 100% (measured by tests), other areas estimated |
| **Ready for real work** | **~56%** (51–61%) | How close FilmCraft is to replacing Premiere for an editor on real projects: depth, correctness, file compatibility, speed, stability, ecosystem. Estimated depth per area, combined by a [written formula](docs/target-app-parity.md#three-readiness-numbers) |
| **Mainstream practitioner** | **~55%** | A typical professional editor's weekly work only, discounted ×0.90 interaction, ×0.92 stability, ×0.88 file exchange with Premiere users ([method](docs/target-app-parity.md#mainstream-practitioner)) |
| **Essentials user** | **~60%** | Import, cut, title, music, export at default settings, discounted ×0.88 launch / stability, ×0.90 discoverability, ×0.95 opening files people send ([method](docs/target-app-parity.md#essentials-user)) |
| **To beta** | **~400–700 Opus 5.5 agent-hours** | Estimated, calibrated from this repo's PR history |
| **To full parity** | **~1,550–2,750 agent-hours** | ~70% parallelizes; plugins, AI models, native-speaker review and some hardware need a human |
| Code (measured) | 45 crates, ~343k lines of Rust, 2,520 tests, 268 merged PRs, 69 authors | `git`, `gh`, `grep` |

Two numbers, because they answer different questions. The gap between them is the work that
matters most now: breadth is nearly done, depth is not.

## By dimension

| Dimension | Ready | Hours | Doc |
|---|---|---|---|
| Features (editing, effects, colour, audio, graphics, export settings) | ~66% | 350–620 | [target-app-parity.md](docs/target-app-parity.md#by-feature-area) |
| UI / UX fidelity | ~60% | 120–200 | [ui-parity.md](docs/ui-parity.md) |
| File formats (containers, `.prproj`, interchange, captions, stills) | ~60% | 140–240 | [file-format-parity.md](docs/file-format-parity.md) |
| Codecs | ~60% | 150–250 | [codec-parity.md](docs/codec-parity.md) |
| Hardware acceleration | ~45% | 120–200 | [hardware-parity.md](docs/hardware-parity.md) |
| Localization | ~35% | 150–230 | [localization-parity.md](docs/localization-parity.md) |
| Performance | ~55% | 60–120 | [performance.md](docs/performance.md) |
| Stability | ~45% | 60–120 | [gaps.md G4](docs/gaps.md#g4-crashes-at-start-up-and-no-test-ci-on-pull-requests) |
| Platforms (macOS, Windows, Linux, FreeBSD, web) | ~60% | 50–90 | [gaps.md G9](docs/gaps.md#g9-windows-and-linux-at-run-time) |
| Plugins and ecosystem | ~5% | 200–400 | [gaps.md G11](docs/gaps.md#g11-no-plugin-hosting) |
| AI features | ~25% | 150–300 | [gaps.md G12](docs/gaps.md#g12-ai-features) |

## Features

| Area | Breadth | Ready | Hours |
|---|---|---|---|
| Editing, timeline, trimming, multicam | ~90% | ~70% | 80–140 |
| Effects and transitions | 100% (measured) | ~60% | 80–140 |
| Media import and codecs | ~75% | ~60% | 150–250 |
| Audio | ~88% | ~65% | 50–90 |
| Panels and UI fidelity | ~88% | ~60% | 120–200 |
| Colour | ~90% | ~70% | 40–70 |
| Graphics, titles, captions | ~85% | ~65% | 50–90 |
| Export and delivery | ~80% | ~65% | 60–110 |
| Project files and interchange | ~70% | ~45% | 90–160 |
| Preferences and project management | ~80% | ~65% | 20–40 |

Evidence for every row: [docs/target-app-parity.md](docs/target-app-parity.md#by-feature-area).

## Languages

Premiere ships 10 interface languages. Measured from our catalogs (3,498 strings):

| Language | Status | Strings |
|---|---|---|
| English | full | 100% |
| Simplified Chinese | partial (whole interface; engine errors English) | 99.6% |
| Spanish | partial (whole interface) | 99.9% |
| Hindi | none (no Devanagari shaping in the interface) | 0% |
| Arabic | none (no RTL interface) | 0% |
| French | none | 0% |
| Portuguese (Brazil) | menus only | 9.9% |
| Indonesian | none | 0% |
| Japanese | partial (whole interface) | 99.7% |
| German | none | 0% |
| Korean | none | 0% |
| Vietnamese | none | 0% |

Also shipped: Ukrainian (menus only, 10%). Detail: [docs/localization-parity.md](docs/localization-parity.md).

## Upcoming

Ranked; detail and "done when" in [docs/roadmap.md](docs/roadmap.md) and [docs/gaps.md](docs/gaps.md).

1. **No launch crashes, and CI on pull requests** (macOS 12, Intel UHD, Windows 11 reports; no workflow runs the tests today). 40–80 h.
2. **Open Premiere's `.prproj`**, the beta blocker. 60–120 h.
3. **10-bit / 4:2:2 camera media without hardware** (H.264 High 10 / 4:2:2, HEVC 4:2:2; #626). 30–50 h.
4. **Measure fidelity against Premiere** and add `cargo xtask parity`. 50–90 h.
5. **Real-world media corpus and workflow acceptance tests.** 40–70 h.
6. **Hardware acceleration, continued** (#30): zero-copy upload, more effects on the GPU, Quick Sync / AMD encoders, VideoToolbox Main 10. 120–200 h.
7. **UI fidelity:** Program monitor transform handles, Effect Controls, docking. 120–200 h.
8. **Plugin hosting** (VST3 / AU, then OpenFX). Owner decision needed. 200–400 h.

## Progress log

- **2026-10-10 (re-measure):** full parity re-measure against Premiere Pro 26.5.2: menu label diff 338 / 366 (92%), installed bundle listings (10 UI languages, exporter and codec frameworks), 188 open issues. Breadth ~86% (file formats now counted), ready ~55%, stage **alpha**; progress docs reorganized per the craftrules standard ([target-app-parity](docs/target-app-parity.md), [gaps](docs/gaps.md), [roadmap](docs/roadmap.md), [codec](docs/codec-parity.md), [file-format](docs/file-format-parity.md), [hardware](docs/hardware-parity.md), [UI](docs/ui-parity.md), [localization](docs/localization-parity.md)).
- **2026-10-10:** ~75 community PRs merged: GPU export rendering (Auto) with Vignette, Video Limiter and Lumetri basic / creative / vignette on the GPU (#421); Linux NVDEC H.264 / HEVC decode (#549); text-to-speech narrations with Kokoro-82M (#278); Japanese for the whole interface (#449) and Ukrainian menus (#407); ALSA default-device fallback (#254, fixes #23); drop import into the timeline (#517); system appearance (#505); timecode scrub editing (#576); NVENC RGBA input (#486); marker CSV export (#454); many fixes (#526–#575).
- **2026-10-09:** release 0.5.0. NVENC H.265 export on Windows, Main and Main 10 HDR (#328, #360); NVENC H.264 export (#386); VA-API H.264 / HEVC decode (#358); Source monitor playback (#423); multi-stream container audio, schema v13 (#361); New Sequence and Sequence Settings dialogs (#269, #355); Spanish for the panels (#238); background decoders and memory stress tests (#385, #381).
- **2026-10-08 (Source range dragging):** implicit full-clip In/Out, video-only/audio-only/linked both controls, picture/waveform drag gestures and one-command undoable timeline placement. Shared Source/Program draggable frame-bounded handles and range translation, cancellation, Source marker/navigation routing and marked-range playback/looping.
- **2026-10-08 (Source playback):** normal forward playback for video/audio media and subclips in the Source monitor, an independent Source clock, Play/Pause and focused Space, frame stepping, device-clock audio with wall-clock fallback, seeking, and end-of-clip stopping. Windows desktop review verified a real video file and standalone WAV, including In/Out marking. Reverse/shuttle Source playback and source sequences remain unsupported; no parity percentage changed.
- **2026-10-08 (night):** HDR H.265 (HEVC Main 10, PQ and HLG, BT.2020, limited range) export on Windows through NVENC (#30): an HDR sequence now exports as real 10-bit HDR (`rgbf_to_yuv420_10` into P010 input buffers; VUI BT.2020 + PQ / HLG; the HDR10 mastering-display and content-light messages as SEI on every IDR for PQ, through the generic SEI payload array because NVENC 12.1 has no mastering-display field; `colr` / `mdcv` / `clli` in the sample entry), where the GPU reports 10-bit HEVC (new `register_hdr_probe` / `hdr_available` in the export crate; elsewhere, and on macOS, HDR sequences still tone-map to 8-bit SDR HEVC, and `settings.sdr` forces that). ffprobe reads `hevc` / `Main 10` / `hvc1` / `yuv420p10le` / `bt2020` / `smpte2084` or `arib-std-b67` / `bt2020nc` / `tv` with the mastering-display and content-light side data for PQ; ffmpeg's decode equals ours code for code; the in-process round trip is at 71.1 dB luma PSNR on the 10-bit scale with 803 distinct levels on a ramp and the peak code 940 intact (RTX 5060). 8-bit SDR H.265 is unchanged.
- **2026-10-08:** H.265 (HEVC Main, 8-bit 4:2:0, SDR) export on Windows through NVENC (#30): the NVENC session, ring and NV12 path of the H.264 encoder now serve both codecs (the H.264 export is byte-for-byte unchanged); the format is available where the GPU has an HEVC encoder, choosing it is the opt-in, and `hvcC` is built from the SPS the encoder wrote. ffprobe reads `hevc` / `Main` / `hvc1` / `yuv420p` / BT.709; our own HEVC decoder decodes it at 48.9 dB luma PSNR (RTX 5060). Main 10 and HDR remain.
- **2026-10-08 (#285):** `filmcraft-cli` no longer panics with "Broken pipe" when its stdout is closed early (`filmcraft-cli commands | head`): later output is dropped, the rest of the work (script lines, `--save`, `--save-as`) still runs, and the exit status still reports failures. Other stdout write errors are reported and make the exit status 1. No change to the checklist or estimates.

- **2026-10-06:** 21 community PRs landed (#19, #28, #38, #39, #41–#60). Highlights: hardware decoding is now actually on in the desktop app (#41: `register()` sat inside a `log::info!` that never ran, so #33's VideoToolbox path only worked in the CLI and benchmarks); decode memory on long timelines cut sharply (frame pool, shared GOP-cache budget, positional file reads); exports are byte-identical across machines (#19); crash fix for odd track names over MCP (#44); ripple trim, ripple delete and speed changes move split-edit clips once (#56–#58); Japanese interface using an installed system font (#38; no fonts bundled); MCP annotations, resources, export progress and cancellation (#28).

- **2026-10-05:** honest assessment added (checklist ~87% vs ready for real work ~50–60%). Hardware acceleration started (#30): blend modes on the GPU (#32); VideoToolbox decode + `platform` crate, the one crate allowed `unsafe` (#33). Community fixes landed: AIFF import, export start/end ranges, transitions following ripple trims, Slide moving linked audio (#5–#8).

- **2026-10-04 (later):** interchange + MXF export (M11.4/M6.6): own compound-file container (`crates/cfb`), AAF Edit Protocol import/export (embedded/linked/consolidated audio, video mixdown, handles, breakout to mono), OMF 2.0 export (Bento), MXF OP1a/OP-Atom writer (DNxHR/ProRes/H.264/PCM) as export formats. 1605 tests.

- **2026-10-04:** MPEG-2 (M9.15): own MPEG-2/MPEG-1 video decoder (4:2:2, interlaced, field pictures; IEEE 1180 IDCT), MPEG TS/PS demux (AVCHD .mts, .m2ts, .ts, .mpg/.vob/.mod), own AC-3 decoder (ATSC A/52), MP2, LATM AAC, Blu-ray/DVD LPCM, MPEG-2 in MXF/MOV/MP4/MKV. 1562 tests.

- **2026-10-03 (evening):** Project panel and Media Browser (M12.7): Freeform view (stacks, arrangements), Icon view hover scrub with In/Out, list columns from all metadata, ten view presets, multiple Project panels; Media Browser with Favorites/drives/recent, back/forward, filters, thumbnails, Edit Columns, ingest, drag to timeline; all as `project.*` / `mediaBrowser.*` commands. 1513 tests.

- **2026-10-03 (later):** graphics & captions (M10.7): `.fcgt` graphics templates (export/install/apply/edit, 8 original built-ins; Adobe .mogrt refused), Essential Graphics Browse/Edit, rolls/crawls, responsive design pins and time, per-character styles, Upgrade Caption to Graphic / to Source Graphic, MCC/EBU STL/TTML/DFXP, Replace Fonts; schema v12. 1479 tests.
- **2026-10-03:** 4K H.264 playback (M4.9): vectorised deblocking, branch-free CABAC bins and column-parallel inverse transforms cut H.264 decoding by 23 % (cycles per frame, bit-exact; 4K cold seeks −27 % CPU); opt-in draft decoding (Settings ▸ Playback) skips deblocking of non-reference pictures and uploads decimated planes while playing at 1/2–1/4, never for paused frames or exports; conformance runs 1, 3 and all threads. 4K plays 192/0 at load 100–140 (before: only at load ~90).
- **2026-10-03:** 4K VP9 / HEVC / AV1 playback (M4.10): VP9 frame threading (references published per superblock row, loop filter overlapping the next frames) plus a vectorised loop filter, column-parallel transforms and 16-bit MC cut VP9 work by 25 % single-threaded and 44 % with all cores (4K decode 8 → 27–34 fps at load ~200); HEVC −28 % (vectorised SAO, MC without per-block zeroing, batched bypass bins), AV1 −20 % (fixed-width MC, recycled planes); opt-in draft decoding for all three; bit-exact at 1, 3 and all threads. 4K VP9 and HEVC now play 192/0 at load ~200 (before: 0–58 of 192 shown).

- **2026-10-03:** keyboard parity (M3.12: 143 new commands on Premiere's default keys, table validated against the registry), audio (M7.8: 5.1 tracks/submixes/Mix, 5.1 panner, BS.775 downmix, 5.1 WAV/MOV/AAC export, voice-over record with punch-in, Remix, Clip Mixer Latch/Touch/Write). 1432 tests.

- **2026-10-02 (late):** export parity (M6.5): built-in preset library + user presets, full Export-mode settings (loudness normalization, image/name/timecode overlays, captions, range), export queue, Quick Export, Premiere-style image-sequence export, `filmcraft-cli export --preset`; media import (M9.14): MXF OP1a/OP-Atom (new `crates/mxf`, H.264/DNx/ProRes/PCM, timecode), Ogg Opus (new `crates/ogg`), image sequences as clips, BWF timecode. 1373 tests.

- **2026-10-02 (panels):** complete Lumetri Scopes (new `filmcraft-scopes` crate: Vectorscope YUV/HLS, Histogram, Parade RGB/YUV/RGB-White, Waveform RGB/Luma/YC/YC no Chroma; wrench menu with presets, colour space, brightness, scale; < 1 ms per scope at 1080p; `scopes.read` returns the numbers); Metadata panel (editable log fields, undoable `metadata.set`), Timecode panel, Events panel (event log of failed commands, jobs, auto-save), Progress panel (jobs with Cancel), Reference Monitor (parked or ganged, picture or scopes).
- **2026-10-02 (evening):** performance (M4.8): `cargo xtask bench` (decode, seek, playback, scrub, timeline UI, export, project I/O, memory; results in docs/performance.md) and `perf.stats` for agents; HEVC transform/SAO 2.5× cheaper; late frames skip non-reference pictures (4K CPU per displayed frame halved, cold seeks 20–45% faster); decoders stay bit-exact. 1270 tests.

- **2026-10-02 (later):** all 93 video effects (+Legacy/Obsolete bins), all 53 audio effects with Parametric/Graphic EQ, Multiband Compressor and Dynamics editor windows, all 84 video transitions (+21 Legacy), Settings dialog (16 categories, most wired), remaining menu items (Scene Edit Detection, Find, Normalize Mix Track, Simplify Sequence, Automate to Sequence, search bins, templates, Project Settings with scratch disks, ALE and selection-as-project export, system report); project schema v11. 1258 tests.

- **2026-10-02:** Premiere menu long tail: Sequence/Markers (gaps, split edits, through edits, subsequence, Delete Tracks, range/chapter markers, ripple sequence markers), Clip/Edit/File (Paste/Remove Attributes, subclips, Frame Hold Options, Time Interpolation with frame blending, Audio Channels, Breakout to Mono, Extract Audio, Replace With Clip, Remove Unused, Consolidate Duplicates), View/monitors (paused resolution, alpha/RGB display modes, comparison view, magnification, rulers, guides + templates, snapping), Graphics and Titles (vertical text, shape layers, align/distribute/arrange). Transcripts and text-based editing (Text panel, optional local Whisper). AV1 tile/frame/post-filter threading. Agent-friendly CLI (`exec`, `inspect`, `describe`, `import`, `export`, `run -`, `--save`, `--bridge`). Project schema v10.

- **2026-10-01 (night):** new README hero (Apollo 11 documentary edit, NASA public domain); agents never steal keyboard focus; masks + tracking, adjustment layers, effect presets; multicam + audio sync; DNxHD/DNxHR decode/encode; AV1 decoder bit-exact against libdav1d (all stages, film grain, superres, SVC).

- **2026-10-01 (M12 multicam):** Synchronize / Merge Clips / Create Multi-Camera Source Sequence (sync by In, Out, timecode ± hours, clip markers, or audio: GCC-PHAT cross-correlation, offsets recovered to the sample on noisy multi-mic recordings), multi-camera clips (angle render, Switch Audio), Program monitor Multi-Camera view with live switching (keys 1–9, one undo step per pass), Ctrl+1–9 cuts, Enable / Flatten / Edit Cameras, project schema v7. Fixed: nested sequence audio was silent.
- **2026-10-01 (evening):** M11 done: offline media with our own slate and the Link Media dialog (fingerprint-checked relink, folder remap, search), proxies (create in background / attach / toggle; export stays full-res) with ingest settings, Project Manager (collect, consolidate + transcode). Fixed: GOP cache deadlock when a rayon decoder inside a parallel export re-entered the same source.
- **2026-10-01 (M7.4):** Essential Sound panel: Dialogue/Music/SFX/Ambience types, Loudness auto-match (BS.1770, exact to the target), Repair (noise, rumble, hum, new DeEsser and spectral DeReverb), Clarity (dynamics, EQ presets, Enhance Speech DSP chain), Creative reverb / stereo width, ducking that writes Volume keyframes, presets; all `essentialSound.*` commands, effects visible and keyframable in Effect Controls.
- **2026-10-01 (night):** M8 colour: camera log curves and gamuts from published specs, colour-managed pipeline (PQ/HLG working spaces, Interpret Footage, BT.2390 tone mapping), HDR export signalling verified with ffprobe, LUTs (.cube/.3dl, tetrahedral CPU + WGSL, project library, Lumetri Input LUT / Look), Colour Match, Lumetri section bypass, HDR scopes.

- **2026-10-01 (night):** DNxHD/DNxHR decoder + encoder (SMPTE ST 2019-1) with MOV export; AV1 decoder (spec v1.0.0 + Errata 1, all decoding tools incl. film grain) bit-exact against libdav1d, wired into MP4/WebM/MKV import with key-frame-checked seeking.
- **2026-10-01 (later):** text engine (shaping, bidi, line breaking) + Type/Shape/Pen tools + graphic clips + graphics panel; Trim Monitor + dynamic trimming; Keyboard Shortcuts editor; audio mixer with automation. Fixed: MP4 muxer wrote unreadable all-empty sample tables; system font scan race.

- **2026-10-01:** Audio mixing (M7.2/M7.5/M7.6 basics): mixer graph with submixes, sends, inserts and latency compensation; Audio Track / Clip Mixer panels; Latch/Touch/Write automation recorded live; timeline track keyframes; Audio Gain dialog.
- **2026-10-01:** recovered from a machine crash with no lost work. Merged: VP9 decoder (profiles 0–3, bit-exact, WebM/MKV/MP4), Opus decoder (RFC 8251 range-exact; WebM/MKV/MP4), captions (SRT/VTT/SCC, burn-in), render bar + render previews, project schema versioning + atomic saves + auto-save + crash-recovery journal, test infrastructure (golden images, loudness oracle vs ffmpeg, headless scripted UI tests; fixed a GPU stale-texture bug), public contributor docs and licence files.

- **2026-09-30 (evening):** Opus decoder (RFC 6716/8251, range-exact on every conformance vector, ~80–110× realtime 48 kHz stereo) wired into WebM/MKV and MP4 import.
- **2026-09-30 (afternoon):** Matroska/WebM import; LUFS meters; clip audio effects on the DSP crate.
- **2026-09-30 (midday):** HEVC decoder, Matroska/WebM demuxer and audio DSP merged; HEVC import wired; asset rules (AGENTS.md, ATTRIBUTION.md, `cargo xtask assets`); README with hero screenshot and the Craft family.
- **2026-09-30 (late morning):** keyframe value/velocity graphs; xtask gates; H.264 MP4 export; Lumetri curves, wheels, looks, HSL secondary.
- **2026-09-30 (early morning):** H.264 decoder, ProRes, AAC; GPU compositor; MCP; Premiere 26 visual fidelity pass.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | minor | Headline adds mainstream practitioner (~55%) and essentials user (~60%); ready for real work ~55% -> ~56% (formula written out, same inputs); beta distance ~19 points |
| 2026-10-10 | minor | Stage banner links the core-workflow gate (all six workflows pass on macOS; stays alpha) |
| 2026-10-10 | major | Restructured to the craftrules progress-docs shape: stage banner, headline numbers, dimension / feature / language tables, upcoming; re-measured against Premiere 26.5.2 (breadth ~87 → ~86%, ready ~50–60% → ~55%); honest assessment and scorecard moved to docs/target-app-parity.md, "Where we are lacking" to docs/gaps.md, milestones to docs/roadmap.md, interface localization to docs/localization-parity.md |
| 2026-10-08 | minor | Log entries for HEVC export, Source playback and range dragging |
| 2026-10-05 | major | Honest assessment added: checklist ~87% vs ready for real work ~50–60% |
