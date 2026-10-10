# Roadmap detail

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-10 · **Change:** major (new file: milestones moved from ROADMAP.md, Current focus rewritten from the 2026-10-10 re-measure, beta milestones added) · **Target:** Adobe Premiere Pro 2026 (26.5.2)

Forward-looking plan: milestones with status, the current focus and what's next, with estimates in
Opus 5.5 agent-hours (calibration in [target-app-parity.md](target-app-parity.md#calibration)).
The one-page summary is [ROADMAP.md](../ROADMAP.md); the ranked work list is [gaps.md](gaps.md).
There is no `cargo xtask scorecard` or `cargo xtask parity` yet ([G7](gaps.md#g7-no-measured-parity-tool-in-ci)).

## Current focus

In order. Each item links to its gap entry, which says what "done" means.

1. **Stop launch crashes and add pull-request CI** ([G4](gaps.md#g4-crashes-at-start-up-and-no-test-ci-on-pull-requests)): `cargo xtask ci` on macOS, Windows and Linux for every PR; old-macOS and Intel-GPU start-up. 40–80 h.
2. **Open `.prproj`** ([G2](gaps.md#g2-premieres-prproj-cannot-be-opened)): the beta blocker. 60–120 h.
3. **10-bit / 4:2:2 camera media in software** ([G3](gaps.md#g3-10-bit-and-422-camera-media-needs-hardware)). 30–50 h.
4. **Fidelity harness against Premiere** ([G1](gaps.md#g1-nothing-compares-our-output-with-premieres)) and **`cargo xtask parity`** ([G7](gaps.md#g7-no-measured-parity-tool-in-ci)): turns estimates into measurements. 50–90 h.
5. **Real-world media corpus and workflow tests** ([G6](gaps.md#g6-no-real-world-media-corpus-or-workflow-acceptance-tests)). 40–70 h.
6. **Hardware acceleration, continued** (#30, [G5](gaps.md#g5-hardware-acceleration-is-partial)): zero-copy upload, Lumetri curves / wheels / HSL and keys on the GPU, VideoToolbox Main 10, Quick Sync / AMF encoders. 120–200 h.
7. **UI fidelity issues** ([G8](gaps.md#g8-ui-fidelity-monitor-handles-effect-controls-docking)): Program monitor transform handles, Effect Controls, docking. 120–200 h.

## Alpha gate

Premiere's core daily workflows, checked end to end on the main platform (macOS, Apple silicon),
including saving the work as `.fcproj` and reopening it (atomic saves, schema migrations,
auto-save and crash recovery; migration tests). Strict reading: a "partial" that stops the
workflow would make FilmCraft pre-alpha.

| Workflow | Works end to end? | Evidence | Hours to pass |
|---|---|---|---|
| Ingest and organize media (import camera / phone / screen files, bins, metadata, proxies) | partial, not blocking | H.264 / HEVC / ProRes / DNx / MXF / AVCHD / MKV import with bit-exact decoders, Media Browser, bins, proxies, Link Media. Gaps: H.264 10-bit / 4:2:2 (Sony XAVC S-I, Canon XF-AVC) opens nowhere and HEVC 4:2:2 only through VideoToolbox (#626, [G3](gaps.md#g3-10-bit-and-422-camera-media-needs-hardware)); E-AC-3 (#647), AVI (#598), camera RAW | 0 (G3 is a beta item, 30–50 h) |
| Assemble and trim a sequence (insert / overwrite, ripple / roll / slip / slide, razor, J/K/L, multicam) | yes | Edit algebra with undo tests, Trim Monitor, Premiere keyboard preset, multicam with audio sync; UX issues (#683, #648, #668) slow it down but don't stop it | 0 |
| Colour correct and grade (Lumetri, LUTs, scopes, colour management) | yes | Lumetri complete with CPU / WGSL parity tests, all Lumetri Scopes, LUT import, HDR export signalling checked with ffprobe | 0 |
| Mix audio (levels, automation, Essential Sound, loudness) | yes on macOS | Mixer graph, automation, 53 effects, BS.1770 loudness matching ffmpeg; device reports are Windows / Linux (#605, #617) | 0 |
| Titles and captions | yes | Text engine, graphic clips, templates, SRT / VTT / SCC / MCC / STL / TTML import-export, burn-in; Arabic titles broken (#395) | 0 |
| Deliver (export H.264 / HEVC / ProRes / DNxHR / MXF with presets, queue) | yes | Own H.264 + AAC encoder, ProRes, DNxHR, MXF, hardware HEVC on macOS, ffprobe-checked exports; bitrate target miss (#371) | 0 |

**Passes:** every core workflow runs end to end on macOS and the project saves and reopens, so
FilmCraft stays **alpha**; the one partial (10-bit / 4:2:2 camera codecs) blocks some cameras'
originals, not the workflow, and is on the beta list. Interchange with Premiere's own `.prproj`
is a beta requirement, not an alpha one (sequences come over as FCP7 XML, AAF or OTIO).

## Beta milestones

Beta = ready for real work ≥ ~75% and `.prproj` opens reliably ([target-app-parity.md](target-app-parity.md#stage)).

| # | Milestone | Status | Est. |
|---|---|---|---|
| B1 | PR CI on three OSs + launch-crash fixes | ⬜ | 40–80 |
| B2 | `.prproj` import (sequences, clips, transitions, markers, bins, common effect parameters) | ⬜ | 60–120 |
| B3 | Camera media: H.264 High 10 / 4:2:2, HEVC 4:2:2, E-AC-3, AVI, multi-track audio confirmed | 🟡 (multi-stream audio #361) | 50–90 |
| B4 | Fidelity harness + `cargo xtask parity` + media corpus + workflow tests | ⬜ | 90–160 |
| B5 | Hardware: zero-copy, more GPU effects, Windows vendor encoders, VideoToolbox Main 10 | 🟡 | 60–100 |
| B6 | UI fidelity top issues and Windows / Linux runtime | 🟡 | 90–160 |
| B7 | AAF / OMF validated in Avid and Pro Tools; XML import speed | ⬜ | 20–40 |

## Milestones (history and remaining)

Status: ✅ done · 🟡 in progress · ⬜ not started. Estimates are remaining agent-hours.

| # | Milestone | Status | Done | Remaining | Est. |
|---|---|---|---|---|---|
| M0 | Skeleton + visual shell | ✅ | Workspace, 20 crates, dock/workspaces, Premiere 26 look, native menus, control channel, MCP, xtask gates (layers, wasm) | — | — |
| M1 | Media I/O | ✅ | MP4/MOV demux+mux, WAV, stills, MJPEG, symphonia audio (MP3/FLAC/ALAC/Vorbis), GOP seek + frame cache, import | Media Browser polish | 2 |
| M2 | H.264 decoder | ✅ | Own decoder, bit-exact on 37+ streams, 500–600 fps 1080p | — | — |
| M3 | Editing core | 🟡 | Edit algebra (insert/overwrite/razor/lift/extract/ripple/roll/slip/slide/rate-stretch/nest/paste), tools, markers, trim mode + Trim Monitor + dynamic J/K/L trimming, Keyboard Shortcuts editor with FilmCraft/Premiere/FCP/Avid presets; Edit/Clip/File menu commands (Label colours and Select Label Group, Paste/Remove Attributes, Select All Matching, Remove Unused, Consolidate Duplicates, Sequence From Clip, Bin From Selection, Offline File, Close Project, Make/Edit Subclip, Modify Audio Channels/Timecode, Frame Hold Options/Add Frame Hold/Insert Frame Hold Segment, Time Interpolation with frame blending, Fit/Fill frame, Breakout to Mono, Extract Audio, Replace With Clip; M3.11: Scene Edit Detection (pure-Rust cut detection, background job), Normalize Mix Track, Simplify Sequence, Transcribe Sequence, Find/Find Next and search bins, Automate to Sequence, Edit Original, Edit Offline, Source Settings, Update Metadata (XMP), Generate Audio Waveform, Project Settings General/Scratch Disks, Get Media File Properties, Save as Template, Selection as FilmCraft Project, Avid Log Exchange export, Flash Cue markers, Dynamic Audio Waveforms, Reveal Log Files, System Compatibility Report); M3.12 keyboard parity: Premiere's default keyboard (~115 keyboard-only commands: edit-point navigation on targeted/any track, select clip at playhead/next/previous, extend edit to playhead, nudge/slip/slide selection, target and source-patch toggles, clip volume ±1 dB/many, frame maximize/full screen/panel cycling, monitor zoom, track heights, Project and Text panel keyboard navigation, text size/leading/alignment, Export Frame, poster frames; see docs/keyboard.md); multicam (Create Multi-Camera Source Sequence, Multi-Camera view with live switching on 1–9, angle switching, Enable/Flatten, Edit Cameras) and sync (Synchronize, Merge Clips; In/Out/timecode/marker/audio — GCC-PHAT, sample-accurate) | Multicam paging >16 angles and grid thumbnails, optical flow (renders as frame blending), 39 Premiere default shortcuts skipped with reasons in docs/keyboard.md (Productions, AI/cloud tools, work area bar, Production/Search panels) | 6–10 |
| M4 | Playback | 🟡 | Audio-clock master, prefetch with cancellation, J/K/L, correct dropped-frame stats, playback resolution, render bar + content-hashed render previews, App Nap opt-out; 1080p H.264 and 3 stacked 1080p streams play with 0 dropped frames | 4K under load, 8K, frame-threaded AV1 decode, reduced-resolution decode for multicam grids | 6–10 |
| M5 | Effects, keyframes, GPU | 🟡 | All 93 Premiere 26 video effects and 84 transitions (CPU), blend modes and 33 common effects on the GPU (#32, GPU2, GPU4, GPU5), keyframes + value/velocity graphs, wgpu compositor, effect + opacity masks (ellipse/polygon/pen, feather, expansion, modes; CPU/WGSL parity), mask tracking (Lucas–Kanade + RANSAC), adjustment layers, effect presets (built-in + user, JSON import/export) | More effects on the GPU (34 ids done incl. Lumetri basic / creative / vignette; GPU export landed #421), GPU masks in the live path, exact Warp Stabilizer (3-D) and Morph Cut | 40–70 |
| M6 | Export | ✅ | Own H.264 encoder (High/Main/Baseline, B-frames, VBR/CBR/2-pass) → MP4 or QuickTime + own AAC; ProRes, DNxHR, MJPEG, PNG/TIFF/BMP sequences (Premiere-style numbering), GIF, WAV, AIFF; background jobs; M6.5: Export mode parity (frame size / rate / scaling / pixel aspect, profile / level, CBR / VBR 1- and 2-pass, keyframe distance, audio codec / rate / channels / bitrate / sample size, multiplexer, burn-in or sidecar captions, image / name / timecode overlays, video limiter, loudness normalization with true-peak limiter, metadata, ranges, estimated size, summary), 24 built-in presets + user presets with favourites and import/export (Preset Manager), export queue (reorder, cancel, retry, several sequences / ranges), Quick Export; `export.*` commands and `filmcraft-cli export --preset` | Interlaced encoding, 5.1 audio, smart render, publishing destinations | 6–8 |
| M7 | Audio | 🟡 | Mixer graph (tracks → submixes → Mix, pre/post-fader inserts and sends, latency-compensated, sample-accurate, ~6× realtime for 24 tracks × 3 effects on one core), Audio Track Mixer + Audio Clip Mixer panels, track automation (Off/Read/Latch/Touch/Write, recorded live while playing, thinned to keyframes, timeline lanes with pen editing), solo/solo-safe, channel mapping basics, peak + BS.1770 loudness meters (match ffmpeg), DSP crate with 16 clip/track effects, Audio Gain (set/adjust/normalize), Constant Power / Constant Gain / Exponential Fade, Essential Sound (types, Loudness auto-match, Repair incl. DeEss/DeReverb, Clarity, Creative, Ducking keyframes, presets; full Dialogue chain 22× realtime) | Music duration remix, ML speech enhancement, 5.1 panner and multichannel buses, voice-over record, effect editor windows (EQ curve), remaining effects (multiband, convolution reverb), clip-mixer automation recording | 6–9 |
| M8 | Colour | 🟡 | Lumetri: basic, creative + looks, RGB & hue curves, wheels, HSL secondary, vignette, section bypass; Input LUT / Look LUT (.cube 1D/3D/shaper, .3dl; tetrahedral CPU + WGSL; project LUT library; built-in camera conversions); Colour Match (Oklab tonal-range statistics, skin protection, solved in Lumetri wheels); colour management: Rec. 709 / Rec. 2100 PQ / HLG working spaces + wide gamut, Interpret Footage colour space (S-Log3, V-Log, Canon Log 2/3, LogC3/4, Apple Log, D-Log from published specs), metadata auto-detect, BT.2390 tone mapping, gamut mapping, HDR export signalling (VUI/colr/mdcv/clli/SEI, ffprobe-verified); Lumetri Scopes: Vectorscope YUV (75/100 % targets, skin-tone line) and HLS, Histogram, Parade (RGB/YUV/RGB-White), Waveform (RGB/Luma/YC/YC no Chroma), presets, Rec. 601/709/2100, 8-bit/float/HDR (cd/m²) scales, multi-scope grid, `scopes.read` | HDR-aware Lumetri maths, macOS EDR monitors, mastering metadata → tone-map peak, D-Log M (no published formula), HSL Secondary refine | 3–5 |
| M9 | More codecs | 🟡 | ProRes decode+encode, AAC decode+encode, HEVC Main/Main 10 decoder (bit-exact on 41 fixtures, ~225 fps 1080p), VP9 decoder (profiles 0–3, 8/10/12-bit, bit-exact on 50+ fixtures; WebM/MKV `V_VP9` and MP4 `vp09` import with key-frame-checked seeking), Matroska/WebM import (H.264/HEVC/VP9/ProRes/MJPEG + AAC/Opus/FLAC/MP3/Vorbis/PCM), Opus decoder (SILK/CELT/hybrid, 5.1/7.1 multistream; all RFC 8251 vectors range-exact; WebM/MKV/MP4), DNxHD/DNxHR decoder (all SMPTE ST 2019-1 CIDs, 8/10/12-bit, 4:2:2/4:4:4, interlaced, alpha; within IDCT precision of ffmpeg on 22 fixtures) + DNxHR LB/SQ/HQ/HQX/444 encoder and MOV `AVdh` export, AV1 decoder (Main profile, 8/10-bit, all intra/inter tools, loop filters, superres, film grain, intra BC, spatial layers; bit-exact vs libdav1d on 19 SVT fixtures + 22 libaom vectors; MP4 `av01` / WebM `V_AV1` import), MXF import (OP1a/OP-Atom; AVC with POC-ordered B pictures bit-exact, DNxHD/DNxHR, ProRes, PCM/AES3; MPEG-2 reported unsupported; index-table seeking, material package timecode), Ogg Opus/Vorbis (granule seeking, pre-skip, end trimming), image sequences, Broadcast WAV timecode | VP9 frame threading, AV1 threading/SIMD (~4 fps 1080p today), AV1 High/Professional profiles, H.264 High 10 / 4:2:2 and HEVC 4:2:2 in software (G3), E-AC-3, AVI (MPEG-2 decoder and hardware decode have landed) | 30–50 |
| M10 | Graphics & captions | 🟡 | Caption tracks (Subtitle/CEA-608/708/Teletext formats, track style), SRT/WebVTT/SCC import+export (frame-exact, property-tested), caption editing (add/split/merge/trim/move, sync-locked insert/extract), Text panel Captions tab, burn-in in Program monitor and export; text engine (`crates/text`: bundled + system fonts, harfrust shaping, bidi, line breaking, paragraph layout, glyph cache; 3-line 1080p title ≈ 0.15 ms warm); graphic clips with text + shape layers (fill, 2 strokes, background, shadow, keyframable transform), Type tool with on-monitor editing, shape/pen tools, Properties/Essential Graphics editor, align/distribute; graphics templates (own `.fcgt` format, 8 original built-ins, Browse tab with engine-rendered thumbnails, export / install / apply / editable properties; never reads .mogrt), rolls/crawls with ease and pre/postroll, responsive pins and intro/outro, per-character styles, Upgrade Caption to Graphic, source graphics, Replace Fonts; MCC (608+708), EBU STL, TTML/IMSC1, DFXP (frame-exact, property-tested) | 608/708 embedding in video streams, speech-to-text | 12–18 |
| M11 | Interchange & project management | ✅ | `.fcproj` schema versions + migrations, atomic saves, Save a Copy/Revert, auto-save ring + crash-recovery journal (Preferences ▸ Auto Save, recovery prompt), FCP7 XML, FCPXML, EDL, OTIO; offline media (own slate) + Link Media (fingerprint-checked relink, folder remap, search, Align Timecode, Make Offline); proxies (ProRes Proxy/LT, H.264 ¼/½ background jobs, attach/detach/reconnect, monitor toggle, export full-res) + ingest (copy/transcode/proxies); Project Manager (collect, consolidate + transcode with handles, size estimate) | Rename media to clip names, image-sequence conversion, Media Browser-driven relink, smart (cross-drive) path tracking | 1–2 |
| M12–M16 | Multicam, web (WASM), platform, long tail | 🟡 | Multicam: audio sync (GCC-PHAT, sub-sample), Merge Clips, multi-camera source sequences, Multi-Camera view with live switching (1–9); L0–L4 crates compile to wasm32 | Web app shell (file access, WebCodecs, audio), scene detection, auto reframe, ~800 remaining commands and dialogs, performance hardening | 35–55 |
| HW | Hardware acceleration (#30) | 🟡 | Blend modes and 34 effect ids on the GPU incl. Lumetri basic / creative / vignette (#32, GPU2–GPU7); GPU export rendering (#421); decode: VideoToolbox (#33, #41), Media Foundation / D3D11 H.264 / HEVC / VP9 / AV1 (HW2, HW3), VA-API H.264 / HEVC (#358), NVDEC H.264 / HEVC (#549); encode: VideoToolbox H.264 / HEVC, NVENC H.264 (Windows, Linux) and HEVC incl. Main 10 HDR (#328, #360) | Zero-copy, remaining GPU effects, VideoToolbox Main 10, Quick Sync / AMF, VA-API VP9 / AV1 and encode | 120–200 |
| L10N | Localization | 🟡 | Spanish, Japanese, Simplified Chinese whole interface; Portuguese (Brazil), Ukrainian menus | Hindi, Arabic (RTL), French, Indonesian, German, Korean, Vietnamese, Italian, Russian; native review | 150–230 |

## After beta

Plugin hosting (VST3 / AU, then OpenFX; owner decision, [G11](gaps.md#g11-no-plugin-hosting)),
AI features with openly licensed models ([G12](gaps.md#g12-ai-features)), delivery formats
([G13](gaps.md#g13-delivery-formats)), localization ([G14](gaps.md#g14-localization-8-of-the-12-key-languages-missing)),
the web app shell ([G20](gaps.md#g20-the-web-app-shell)).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-10 | minor | Alpha gate table added (craftrules core-workflow gate): six core workflows, all pass on macOS; stage stays alpha |
| 2026-10-10 | major | Created: milestone table moved from ROADMAP.md (M5 and M9 remaining work updated), HW and L10N rows, beta milestones B1–B7, Current focus from the re-measure |
