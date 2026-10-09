# FilmCraft Roadmap

Progress toward parity with Adobe Premiere Pro, with estimates. Updated as milestones land.

**Last updated:** 2026-10-05 · **Feature checklist:** ~87% · **Ready for real work:** ~50–60% · **Code:** 35 crates, 1605+ tests

Two numbers, because they answer different questions. The **feature checklist** counts whether
Premiere's menu items, effects, panels and formats exist in FilmCraft. **Ready for real work** is
our honest estimate of how close FilmCraft is to replacing Premiere for an editor on real
projects: depth, correctness, robustness, speed and the pro ecosystem. The gap between the two
is the work that matters most now. Agents: read [Where we are lacking](#where-we-are-lacking)
before choosing work.

## Honest assessment (2026-10-05)

### How the checklist number is made, and what it doesn't tell you

- **Partly measured.** The menu figure comes from diffing a dump of Premiere 26.5's menus against
  `filmcraft-cli commands`. The effects, transitions and audio-effects counts are checked by unit
  tests against Premiere's Effects panel tree (`premiere_26_video_effects_catalogue`,
  `premiere_audio_set_is_complete_and_foldered`, `premiere_26_transition_tree`).
- **Partly agent-reported.** The other areas' percentages are written by the agents doing the work.
  No automated parity check runs in CI (PhotoCraft has `cargo xtask parity`; we don't yet), so
  nothing stops a number drifting upward.
- **Presence, not quality.** An item counts once it exists. An effect described as an
  "approximation" counts the same as an exact one. Nothing compares our rendering with
  Premiere's.
- **Weights understate what users hit first.** Performance carries 5%. Plugins and AI features
  aren't in the scorecard at all.

### By dimension

| Dimension | Checklist says | Honest estimate | Evidence |
|---|---|---|---|
| **Feature breadth** (menus, commands, panels) | ~86–95% | **~85%** | Menu diff against Premiere 26.5; effect, transition and audio-effect catalogues complete by test. The long tail is mostly there. |
| **Correctness in depth** | not tracked | **~60–70%** | A first-time contributor found four real bugs in core paths within hours (#5–#8: AIFF import failed, Slide left linked audio behind, transitions didn't follow ripple trims, export ignored start/end times). User reports: Color Matte always grey with no picker (#29); ALSA audio error on Linux (#23). Settings are "most wired", not all. |
| **Codecs and media** | ~90% | **~75%** | Our decoders are bit-exact on conformance streams. Real camera and phone media (variable frame rate, damaged files, unusual containers) is far less tested. No camera RAW (RED, BRAW, ARRIRAW), no E-AC-3. |
| **Export** | ~92% | **~70%** | Delivery: H.264, plus H.265 through a hardware encoder only (NVENC on Windows: Main 8-bit SDR and Main 10 HDR PQ / HLG; VideoToolbox on macOS: Main 8-bit SDR); ProRes, DNxHR and image sequences cover mastering. No AV1 export, no 10-bit / HDR HEVC on macOS. AAF and OMF have never been validated in Avid Media Composer or Pro Tools. Export renders and encodes on the CPU only. |
| **Performance and hardware** | ~62% | **~35–40%** | Premiere runs effects, decoding and encoding on the GPU and the hardware media engines. Here, hardware decode works on macOS (VideoToolbox, #33; active in the desktop app only since #41, so releases up to 0.2.1 decoded in software) and Windows (Media Foundation / Direct3D 11 DXVA, H.264, HEVC Main / Main 10, VP9 and AV1 4:2:0 8/10-bit, #30), and for H.264 and HEVC on Linux (VA-API: 4K H.264 at 11 ms of CPU per frame instead of 417 ms, 4K HEVC at 10 ms instead of 203 ms). The GPU does compositing, blend modes (#32) and 31 common effects (GPU2): colour adjustments, crop/flip/transform, blurs, sharpen. Lumetri, keys, masks, the remaining effects and export rendering are CPU; H.264 encoding can use VideoToolbox on macOS (opt-in per export, no B-frames) and NVENC on Windows and Linux with an NVIDIA GPU (opt-in per export), and H.265 (HEVC) export exists on macOS and Windows only, through the same hardware encoders. Linux decodes H.264 and HEVC in hardware (VP9 and AV1 there are software); its only hardware encoder is NVENC H.264. 8K isn't real-time (AV1 is on Windows with a hardware decoder). The effect list shows Premiere's "GPU accelerated" badge, but only 31 effect ids run on the GPU here; the rest, Lumetri included, run on the CPU. |
| **Stability** | not tracked | **improving, unproven** | The never-crash pass (no panics in product code, last-resort guards) landed on 2026-10-04. There's no field record from real users' projects yet. |
| **Plugins and pro ecosystem** | not tracked | **~0–10%** | No audio plugin hosting (VST3 / Audio Units) and no third-party video effects (OpenFX). For many professional editors this alone rules FilmCraft out. Team Projects and Productions are out of scope by design. |
| **AI features** | partial | **~25–35%** | Speech to text exists but is off in default builds (the `whisper` feature). Enhance Speech is a DSP chain, not a model. Auto Reframe is approximate. No Generative Extend, and no media-intelligence search or auto colour. |
| **Platforms** | not tracked | **macOS solid; Windows and Linux thin** | Development and testing happen on macOS. Windows and Linux are built for releases but barely exercised at runtime. The web build is a compile target; the browser app shell isn't finished. |
| **UI fidelity** | ~85% | **~80%** | Agents compare with Premiere reference screenshots; this hasn't been checked independently. |

### Where we are lacking

In priority order. Agents should prefer this work over adding more checklist items.

1. **Hardware acceleration** (#30), the most visible gap to users:
   - hardware decode on Linux (VA-API): H.264 and HEVC are in; VP9 and AV1 remain;
   - zero-copy decoded frames into wgpu;
   - the remaining effects on the GPU: Lumetri, keys, Vignette, Video Limiter, masks (31 common effects are done);
   - GPU export, then hardware encode: H.264 and H.265 on macOS are in (H.264 opt-in; H.265 has no other encoder), H.264 and H.265 (Main 8-bit, and Main 10 HDR PQ / HLG) on NVENC (Windows); Main 10 / HDR HEVC on VideoToolbox, Linux and other Windows vendors remain.
2. **A measured parity number.** Add `cargo xtask parity`, run in CI. It should cover menus,
   commands, effects, transitions, panels, preferences and formats against the Premiere snapshot in
   `plan/premiere/`. It should report presence and fidelity separately (exact, approximate, stub).
3. **Real-world media corpus.** Phone, camera and screen recordings, variable frame rate, damaged
   files, long GOPs, odd containers. Each gets an import → edit → export round-trip test with an
   ffprobe / ffmpeg oracle.
4. **Workflow acceptance tests.** Scripted MCP sessions that do real jobs end to end: assemble and
   trim a scene, colour it, mix and loudness-normalise it, caption it, deliver it, round-trip
   through XML / AAF.
5. **Windows and Linux at runtime.** CI that runs the test suite and a headless playback/export
   smoke test on both, not just release builds. Fix the Linux audio path (#23).
6. **Delivery codecs.** HEVC export is in on macOS (VideoToolbox, 8-bit, no software encoder) and on Windows (NVENC, 8-bit and Main 10 HDR); still to do: HEVC elsewhere, 10-bit / HDR HEVC on macOS, and AV1 (`rav1e`, BSD-2).
7. **Plugin hosting** (VST3 / Audio Units, then OpenFX). Needs FFI, so it falls under the isolated
   `unsafe` crate rule (AGENTS.md §0.3). **Owner decision needed** before starting.
8. **AI features** with openly licensed local models: transcription on by default, text-based
   editing, speech enhancement, reframing.
9. **The web app shell**: file access, WebCodecs, audio.

### Where we're going

**Next:** close items 1–5. Hardware acceleration fixes the gap users notice first (CPU-bound 4K playback and export). The
parity tool, media corpus and workflow tests make the progress number trustworthy. Windows and
Linux CI makes "runs everywhere" true.

**After that:** delivery codecs, plugin hosting (pending the owner's decision) and AI features,
measured with the same tools.

**Goal:** "ready for real work" at 85%+, measured rather than estimated, on all three desktop
platforms.

## Feature checklist scorecard

Measured against Premiere Pro 26.5 on this machine. Menu items: the native menu bar dump, minus
Adobe-cloud-only items (Team Projects, Productions, Firefly, Stock, Dynamic Link, account/help pages),
matched against `filmcraft-cli commands` and the UI command table. Effects: the Effects panel tree.

| Area | Weight | Measured | Coverage |
|---|---|---|---|
| Editing, timeline, trimming, multicam | 20% | core edit algebra, trim modes, sync, multicam, menu long tail, Scene Edit Detection, Automate to Sequence, Premiere default keyboard (143 new commands; 39 cloud/absent-panel keys skipped) | ~86% |
| Menus / commands | (cross-check) | ~325 of 344 in-scope menu items | ~95% |
| Effects and transitions | 12% | video effects 93/93 (+Legacy, Obsolete), audio effects 53/53, video transitions 84/84 (+21 Legacy), audio transitions 3/3; some approximations (Warp Stabilizer 2-D, Morph Cut, Auto Reframe) | ~92% |
| Media I/O and codecs | 12% | H.264, HEVC, VP9, AV1, ProRes, DNx, MJPEG, MPEG-2/MPEG-1 (4:2:2 incl. IMX/XDCAM), Opus, AAC, AC-3, MP2; MP4/MOV/MKV/WebM, MXF (incl. MPEG-2), MPEG TS/PS (AVCHD, broadcast, DVD), Ogg, image sequences, BWF; no E-AC-3, camera raw, HW decode | ~90% |
| Panels and UI fidelity | 12% | all main panels incl. Metadata, Timecode, Events, Progress, Reference Monitor, full Lumetri Scopes, audio effect editors; Project panel List/Icon/Freeform with view presets and hover scrub; Media Browser with Favorites/navigation/ingest | ~85% |
| Audio | 10% | mixer, automation (track + clip), Essential Sound, meters, all 53 effects, 5.1 tracks/buses/panner/export, voice-over record, Remix | ~88% |
| Colour | 8% | Lumetri complete, LUTs, colour management, HDR, all Lumetri Scopes (+ `scopes.read` for agents) | ~88% |
| Graphics and captions | 8% | text engine, shapes, per-character styles, our own graphics templates (.fcgt, 8 built-ins, export/install/edit), rolls/crawls, responsive pins + time, captions SRT/VTT/SCC/MCC/STL/TTML/DFXP, transcripts | ~85% |
| Export | 8% | own H.264/AAC, ProRes, DNxHR, MXF OP1a/OP-Atom, image sequences, GIF, WAV, 5.1; preset library + user presets, full Export-mode settings, queue, Quick Export; AAF (Edit Protocol) import/export, OMF 2.0 export; no AAF/OMF validation against Avid/Pro Tools yet | ~92% |
| Preferences and project management | 5% | Settings dialog with 16 categories (most settings wired), project settings, scratch disks, search bins, templates | ~80% |
| Performance | 5% | 1080p real-time, 3×1080p; 4K H.264 real-time at load ≤140 (−23% decoder cycles, opt-in draft decoding at 1/2–1/4); HEVC 2.5× cheaper, catch-up decoding, `cargo xtask bench` + `perf.stats`; blend modes on the GPU (#32); 8K and AV1 not yet real-time on a loaded machine; H.264 and HEVC decode in hardware on Linux (VA-API), and on Windows only decode and opt-in NVENC H.264 export (see the honest assessment: ~35–40% against Premiere) | ~62% |
| **Weighted total** | | | **~87%** |

## Estimate to parity

This estimate is for the **feature checklist**. Closing the honest-assessment gaps (above) is the
"Robust on real-world material" row, plus plugins and AI features, which it doesn't include.

Measured throughput in the last work block (2026-10-01 night → 10-02): five Opus 5.5 agents in
parallel for ~4.5 wall-clock hours (~18 agent-hours including integration) moved parity by ~4 points
(≈4–5 agent-hours per point), on a machine that was heavily overloaded (load 150–300 on 14 cores) and
once ran out of disk. The remaining points are a long tail (each effect, dialog and preference page is
small, but there are many), so the estimate applies a 1.3–1.5× tail factor.

| | Opus 5.5 agent-hours | Wall-clock (4–5 parallel agents + integrator) |
|---|---|---|
| Feature parity by checklist (~19 points left) | ~90–125 | **~20–32 h** |
| Robust on real-world material (codec edge cases, 4K/8K performance, pro workflows) | +120–200 | **+30–50 h** |
| **Total to "100% and better"** | **~210–325** | **~50–82 h (≈2–3.5 days, 24/7)** |

The 2026-10-02 block moved parity ~62% → ~72% with six agents in ~6 wall-clock hours, despite the
disk filling twice; the remaining work is mostly panels (scopes, Metadata, Media Browser), export
presets/queue, codecs (MXF, image sequences, hardware decode), performance and graphics templates.

Limits on speed: CPU and disk on one machine (more than ~5 agents slows everyone down: each worktree
build is 6–7 GB and a full `cargo xtask ci` takes 20–60 min under load), and a single integrator
merging, resolving conflicts (e.g. two agents bumping the project schema) and re-running CI. With one
agent and no parallelism, multiply wall-clock by ~3–4.

Not reachable clean-room and locally: **Generative Extend** (needs a large video-generation model).
**Enhance Speech** and **Auto Reframe** are feasible only with openly licensed models we can ship.

## Milestones

Status: ✅ done · 🟡 in progress · ⬜ not started. Estimates are remaining agent-hours.

| # | Milestone | Status | Done | Remaining | Est. |
|---|---|---|---|---|---|
| M0 | Skeleton + visual shell | ✅ | Workspace, 20 crates, dock/workspaces, Premiere 26 look, native menus, control channel, MCP, xtask gates (layers, wasm) | — | — |
| M1 | Media I/O | ✅ | MP4/MOV demux+mux, WAV, stills, MJPEG, symphonia audio (MP3/FLAC/ALAC/Vorbis), GOP seek + frame cache, import | Media Browser polish | 2 |
| M2 | H.264 decoder | ✅ | Own decoder, bit-exact on 37+ streams, 500–600 fps 1080p | — | — |
| M3 | Editing core | 🟡 | Edit algebra (insert/overwrite/razor/lift/extract/ripple/roll/slip/slide/rate-stretch/nest/paste), tools, markers, trim mode + Trim Monitor + dynamic J/K/L trimming, Keyboard Shortcuts editor with FilmCraft/Premiere/FCP/Avid presets; Edit/Clip/File menu commands (Label colours and Select Label Group, Paste/Remove Attributes, Select All Matching, Remove Unused, Consolidate Duplicates, Sequence From Clip, Bin From Selection, Offline File, Close Project, Make/Edit Subclip, Modify Audio Channels/Timecode, Frame Hold Options/Add Frame Hold/Insert Frame Hold Segment, Time Interpolation with frame blending, Fit/Fill frame, Breakout to Mono, Extract Audio, Replace With Clip; M3.11: Scene Edit Detection (pure-Rust cut detection, background job), Normalize Mix Track, Simplify Sequence, Transcribe Sequence, Find/Find Next and search bins, Automate to Sequence, Edit Original, Edit Offline, Source Settings, Update Metadata (XMP), Generate Audio Waveform, Project Settings General/Scratch Disks, Get Media File Properties, Save as Template, Selection as FilmCraft Project, Avid Log Exchange export, Flash Cue markers, Dynamic Audio Waveforms, Reveal Log Files, System Compatibility Report); M3.12 keyboard parity: Premiere's default keyboard (~115 keyboard-only commands: edit-point navigation on targeted/any track, select clip at playhead/next/previous, extend edit to playhead, nudge/slip/slide selection, target and source-patch toggles, clip volume ±1 dB/many, frame maximize/full screen/panel cycling, monitor zoom, track heights, Project and Text panel keyboard navigation, text size/leading/alignment, Export Frame, poster frames; see docs/keyboard.md); multicam (Create Multi-Camera Source Sequence, Multi-Camera view with live switching on 1–9, angle switching, Enable/Flatten, Edit Cameras) and sync (Synchronize, Merge Clips; In/Out/timecode/marker/audio — GCC-PHAT, sample-accurate) | Multicam paging >16 angles and grid thumbnails, optical flow (renders as frame blending), 39 Premiere default shortcuts skipped with reasons in docs/keyboard.md (Productions, AI/cloud tools, work area bar, Production/Search panels) | 6–10 |
| M4 | Playback | 🟡 | Audio-clock master, prefetch with cancellation, J/K/L, correct dropped-frame stats, playback resolution, render bar + content-hashed render previews, App Nap opt-out; 1080p H.264 and 3 stacked 1080p streams play with 0 dropped frames | 4K under load, 8K, frame-threaded AV1 decode, reduced-resolution decode for multicam grids | 6–10 |
| M5 | Effects, keyframes, GPU | 🟡 | All 93 Premiere 26 video effects and 84 transitions (CPU), blend modes and 31 common effects on the GPU (#32, GPU2), keyframes + value/velocity graphs, wgpu compositor, effect + opacity masks (ellipse/polygon/pen, feather, expansion, modes; CPU/WGSL parity), mask tracking (Lucas–Kanade + RANSAC), adjustment layers, effect presets (built-in + user, JSON import/export) | Effects on the GPU (in progress, #30), GPU masks in the live path, GPU export, exact Warp Stabilizer (3-D) and Morph Cut | 20–30 |
| M6 | Export | ✅ | Own H.264 encoder (High/Main/Baseline, B-frames, VBR/CBR/2-pass) → MP4 or QuickTime + own AAC; ProRes, DNxHR, MJPEG, PNG/TIFF/BMP sequences (Premiere-style numbering), GIF, WAV, AIFF; background jobs; M6.5: Export mode parity (frame size / rate / scaling / pixel aspect, profile / level, CBR / VBR 1- and 2-pass, keyframe distance, audio codec / rate / channels / bitrate / sample size, multiplexer, burn-in or sidecar captions, image / name / timecode overlays, video limiter, loudness normalization with true-peak limiter, metadata, ranges, estimated size, summary), 24 built-in presets + user presets with favourites and import/export (Preset Manager), export queue (reorder, cancel, retry, several sequences / ranges), Quick Export; `export.*` commands and `filmcraft-cli export --preset` | Interlaced encoding, 5.1 audio, smart render, publishing destinations | 6–8 |
| M7 | Audio | 🟡 | Mixer graph (tracks → submixes → Mix, pre/post-fader inserts and sends, latency-compensated, sample-accurate, ~6× realtime for 24 tracks × 3 effects on one core), Audio Track Mixer + Audio Clip Mixer panels, track automation (Off/Read/Latch/Touch/Write, recorded live while playing, thinned to keyframes, timeline lanes with pen editing), solo/solo-safe, channel mapping basics, peak + BS.1770 loudness meters (match ffmpeg), DSP crate with 16 clip/track effects, Audio Gain (set/adjust/normalize), Constant Power / Constant Gain / Exponential Fade, Essential Sound (types, Loudness auto-match, Repair incl. DeEss/DeReverb, Clarity, Creative, Ducking keyframes, presets; full Dialogue chain 22× realtime) | Music duration remix, ML speech enhancement, 5.1 panner and multichannel buses, voice-over record, effect editor windows (EQ curve), remaining effects (multiband, convolution reverb), clip-mixer automation recording | 6–9 |
| M8 | Colour | 🟡 | Lumetri: basic, creative + looks, RGB & hue curves, wheels, HSL secondary, vignette, section bypass; Input LUT / Look LUT (.cube 1D/3D/shaper, .3dl; tetrahedral CPU + WGSL; project LUT library; built-in camera conversions); Colour Match (Oklab tonal-range statistics, skin protection, solved in Lumetri wheels); colour management: Rec. 709 / Rec. 2100 PQ / HLG working spaces + wide gamut, Interpret Footage colour space (S-Log3, V-Log, Canon Log 2/3, LogC3/4, Apple Log, D-Log from published specs), metadata auto-detect, BT.2390 tone mapping, gamut mapping, HDR export signalling (VUI/colr/mdcv/clli/SEI, ffprobe-verified); Lumetri Scopes: Vectorscope YUV (75/100 % targets, skin-tone line) and HLS, Histogram, Parade (RGB/YUV/RGB-White), Waveform (RGB/Luma/YC/YC no Chroma), presets, Rec. 601/709/2100, 8-bit/float/HDR (cd/m²) scales, multi-scope grid, `scopes.read` | HDR-aware Lumetri maths, macOS EDR monitors, mastering metadata → tone-map peak, D-Log M (no published formula), HSL Secondary refine | 3–5 |
| M9 | More codecs | 🟡 | ProRes decode+encode, AAC decode+encode, HEVC Main/Main 10 decoder (bit-exact on 41 fixtures, ~225 fps 1080p), VP9 decoder (profiles 0–3, 8/10/12-bit, bit-exact on 50+ fixtures; WebM/MKV `V_VP9` and MP4 `vp09` import with key-frame-checked seeking), Matroska/WebM import (H.264/HEVC/VP9/ProRes/MJPEG + AAC/Opus/FLAC/MP3/Vorbis/PCM), Opus decoder (SILK/CELT/hybrid, 5.1/7.1 multistream; all RFC 8251 vectors range-exact; WebM/MKV/MP4), DNxHD/DNxHR decoder (all SMPTE ST 2019-1 CIDs, 8/10/12-bit, 4:2:2/4:4:4, interlaced, alpha; within IDCT precision of ffmpeg on 22 fixtures) + DNxHR LB/SQ/HQ/HQX/444 encoder and MOV `AVdh` export, AV1 decoder (Main profile, 8/10-bit, all intra/inter tools, loop filters, superres, film grain, intra BC, spatial layers; bit-exact vs libdav1d on 19 SVT fixtures + 22 libaom vectors; MP4 `av01` / WebM `V_AV1` import), MXF import (OP1a/OP-Atom; AVC with POC-ordered B pictures bit-exact, DNxHD/DNxHR, ProRes, PCM/AES3; MPEG-2 reported unsupported; index-table seeking, material package timecode), Ogg Opus/Vorbis (granule seeking, pre-skip, end trimming), image sequences, Broadcast WAV timecode | VP9 frame threading, AV1 threading/SIMD (~4 fps 1080p today), AV1 High/Professional profiles, MPEG-2 decoder, hardware decode | 12–20 |
| M10 | Graphics & captions | 🟡 | Caption tracks (Subtitle/CEA-608/708/Teletext formats, track style), SRT/WebVTT/SCC import+export (frame-exact, property-tested), caption editing (add/split/merge/trim/move, sync-locked insert/extract), Text panel Captions tab, burn-in in Program monitor and export; text engine (`crates/text`: bundled + system fonts, harfrust shaping, bidi, line breaking, paragraph layout, glyph cache; 3-line 1080p title ≈ 0.15 ms warm); graphic clips with text + shape layers (fill, 2 strokes, background, shadow, keyframable transform), Type tool with on-monitor editing, shape/pen tools, Properties/Essential Graphics editor, align/distribute; graphics templates (own `.fcgt` format, 8 original built-ins, Browse tab with engine-rendered thumbnails, export / install / apply / editable properties; never reads .mogrt), rolls/crawls with ease and pre/postroll, responsive pins and intro/outro, per-character styles, Upgrade Caption to Graphic, source graphics, Replace Fonts; MCC (608+708), EBU STL, TTML/IMSC1, DFXP (frame-exact, property-tested) | 608/708 embedding in video streams, speech-to-text | 12–18 |
| M11 | Interchange & project management | ✅ | `.fcproj` schema versions + migrations, atomic saves, Save a Copy/Revert, auto-save ring + crash-recovery journal (Preferences ▸ Auto Save, recovery prompt), FCP7 XML, FCPXML, EDL, OTIO; offline media (own slate) + Link Media (fingerprint-checked relink, folder remap, search, Align Timecode, Make Offline); proxies (ProRes Proxy/LT, H.264 ¼/½ background jobs, attach/detach/reconnect, monitor toggle, export full-res) + ingest (copy/transcode/proxies); Project Manager (collect, consolidate + transcode with handles, size estimate) | Rename media to clip names, image-sequence conversion, Media Browser-driven relink, smart (cross-drive) path tracking | 1–2 |
| M12–M16 | Multicam, web (WASM), platform, long tail | 🟡 | Multicam: audio sync (GCC-PHAT, sub-sample), Merge Clips, multi-camera source sequences, Multi-Camera view with live switching (1–9); L0–L4 crates compile to wasm32 | Web app shell (file access, WebCodecs, audio), scene detection, auto reframe, ~800 remaining commands and dialogs, performance hardening | 35–55 |

## Running now

- **Hardware acceleration (#30):** landed: blend modes on the GPU (#32); VideoToolbox hardware decode + the `platform` FFI crate (#33); 31 common effects on the GPU (GPU2). Windows: Media Foundation / Direct3D 11 H.264 + HEVC decode (HW2) and VP9 + AV1 (HW3), NVENC H.264 export (HW4, opt-in) and H.265 export. Linux: VA-API H.264 + HEVC decode (HW5). Next: VA-API VP9 / AV1, zero-copy upload, Lumetri on the GPU.
- **Next:** [Where we are lacking](#where-we-are-lacking) items 1–5. GPU export waits for the export-crate work (#19 and the never-crash follow-up) to land.

## Log

- **2026-10-08 (Source range dragging):** implicit full-clip In/Out, video-only/audio-only/linked both controls, picture/waveform drag gestures and one-command undoable timeline placement. Shared Source/Program draggable frame-bounded handles and range translation, cancellation, Source marker/navigation routing and marked-range playback/looping.
- **2026-10-08 (Source playback):** normal forward playback for video/audio media and subclips in the Source monitor, an independent Source clock, Play/Pause and focused Space, frame stepping, device-clock audio with wall-clock fallback, seeking, and end-of-clip stopping. Windows desktop review verified a real video file and standalone WAV, including In/Out marking. Reverse/shuttle Source playback and source sequences remain unsupported; no parity percentage changed.
- **2026-10-08 (night):** HDR H.265 (HEVC Main 10, PQ and HLG, BT.2020, limited range) export on Windows through NVENC (#30): an HDR sequence now exports as real 10-bit HDR (`rgbf_to_yuv420_10` into P010 input buffers; VUI BT.2020 + PQ / HLG; the HDR10 mastering-display and content-light messages as SEI on every IDR for PQ, through the generic SEI payload array because NVENC 12.1 has no mastering-display field; `colr` / `mdcv` / `clli` in the sample entry), where the GPU reports 10-bit HEVC (new `register_hdr_probe` / `hdr_available` in the export crate; elsewhere, and on macOS, HDR sequences still tone-map to 8-bit SDR HEVC, and `settings.sdr` forces that). ffprobe reads `hevc` / `Main 10` / `hvc1` / `yuv420p10le` / `bt2020` / `smpte2084` or `arib-std-b67` / `bt2020nc` / `tv` with the mastering-display and content-light side data for PQ; ffmpeg's decode equals ours code for code; the in-process round trip is at 71.1 dB luma PSNR on the 10-bit scale with 803 distinct levels on a ramp and the peak code 940 intact (RTX 5060). 8-bit SDR H.265 is unchanged.
- **2026-10-08:** H.265 (HEVC Main, 8-bit 4:2:0, SDR) export on Windows through NVENC (#30): the NVENC session, ring and NV12 path of the H.264 encoder now serve both codecs (the H.264 export is byte-for-byte unchanged); the format is available where the GPU has an HEVC encoder, choosing it is the opt-in, and `hvcC` is built from the SPS the encoder wrote. ffprobe reads `hevc` / `Main` / `hvc1` / `yuv420p` / BT.709; our own HEVC decoder decodes it at 48.9 dB luma PSNR (RTX 5060). Main 10 and HDR remain.
- **2026-10-08 (#285):** `filmcraft-cli` no longer panics with "Broken pipe" when its stdout is closed early (`filmcraft-cli commands | head`): later output is dropped, the rest of the work (script lines, `--save`, `--save-as`) still runs, and the exit status still reports failures. Other stdout write errors are reported and make the exit status 1. No change to the checklist or estimates.

- **2026-10-08 (M11.5):** native PremiereData v3 `.prproj` and `.prfpset` import foundation: bins, media references, sequences, exact clip ticks, links, nests, selected stock effects and transitions, preset animation/timing modes, undo/redo and explicit fidelity reports. Unsupported components are reported; proprietary plug-ins and native project export are outside this mapping. See [Premiere FX & Projects](docs/premiere-fx-projects.md).

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

### Interface localization

Edit > Language switches English/Japanese/Spanish/Portuguese (Brazilian) and persists across restarts through `general.interfaceLanguage` (also available in Settings > General). Until a language is chosen, System Language (the default) follows the operating system's or browser's preferred languages and falls back to English. Commands `app.language.english`, `app.language.japanese`, `app.language.spanish` and `app.language.portuguese` are reachable through the control channel. Translations live in one catalog per language (`crates/ui-egui/src/i18n/<code>.tsv`); UI code wraps its strings in `tl!("…")` / `tlf!("…{name}", name)`, and names that come from registries (effects and their parameters, settings, commands, panels, workspaces, presets) are translated where they are drawn. **Spanish covers the whole interface**: menus, panels, dialogs, the Settings pages, the Effects/Effect Controls registry, the Keyboard Shortcuts dialog and status messages; tests fail when a `tl!` literal, a menu label or a registry name has no Spanish entry. Searches match the displayed labels as well as English sources. Japanese and Brazilian Portuguese cover the core menus; untranslated labels use English. Engine error messages, the control channel, the CLI and MCP stay in English. Japanese text uses the Japanese fonts from [craft-fonts](https://github.com/storytold/craft-fonts) when FilmCraft is built with them (`CRAFT_FONTS_DIR`; all official releases, including the web build), otherwise a Japanese font already installed on the system; with neither, switching to Japanese is refused with a message. Existing vertical text support is preserved.
