# Performance

`cargo xtask bench` measures the whole app headlessly and writes `target/bench/bench-<label>.json`
and `.md` (options and sections: [testing.md](testing.md) §5). Agents read live counters with
`perf.stats` ([control-protocol.md](control-protocol.md)).

**Read the numbers with the load average.** These results come from a shared Apple M4 Pro
(14 cores, 48 GB, macOS 15 / Darwin 24.6) with parallel agent builds running: load average
**75–200** during every run below. Wall-clock columns (fps, shown/dropped frames, seek latency)
swing by 2–5× between back-to-back runs at that load and are only comparable within one run. CPU
time per frame, samples decoded and skipped, and peak RSS are much less sensitive, and are what
the before/after comparison relies on. Each milestone below alternated base / after runs (2 rounds
each); the most load-independent figure is the decoder's own cycle count (`proc_pid_rusage`
instructions and cycles of a process, all threads summed).

## Memory of clips on a timeline (MEM1–MEM3)

Measured on the desktop release build with `footprint`, `heap` and `malloc_history`
(`MallocStackLogging=lite`), 2026-10-05: twelve H.264 clips dropped on a timeline took the process
to a 12 GB footprint, 10.8 GB of it live heap in three piles.

1. **Media files read whole (2.9 GB), MEM1.** The desktop host had no `Services::reader`, so
   `MediaPool::open_file` fell back to `read_file` and every clip's bytes stayed in memory for as
   long as its source was open. `FsServices::reader` now returns a
   `filmcraft_media::reader::FileReader`: the file stays open and containers read their index and
   then single samples with positional reads, as the web host already did through `BlobReader`
   ([web.md](web.md)). Formats decoded in one go (stills, WAV, MPEG elementary streams) are still
   read whole through the reader.
2. **A decoder per clip, for good (3.5 GB), MEM2.** Every source kept its decoder once it had
   decoded a frame, with its reference pictures and the pictures its frame threads have in flight:
   about 290 MB per clip here (`h264` `PicState`, `Frame::make_row`). The GOP caches now share a
   pool (`crates/codecs/src/gop.rs`): beyond `MAX_LIVE_DECODERS` (4) the least recently used
   caches that have been idle for 3 s drop their decoder and make a new one when asked again,
   which costs what a seek costs. A cache in use (a layer of the frame being composited, the next
   clip being prefetched) always keeps its decoder, so a composite of more than four clips does
   not restart decoders on every frame. `perf.stats` reports `decode.liveDecoders`.
3. **A frame budget per clip, none overall (3.8 GB), MEM3.** Each GOP cache keeps up to 384 MB
   of decoded frames (at least 64 frames), which every clip on the timeline filled and kept. The
   pool now counts the frames of all caches against `FRAME_BUDGET` (1 GiB): over it, caches idle
   for 3 s give up frames, least recently used cache first, earliest frames first. Caches in use
   keep their own budget, because a frame evicted before it is shown costs a re-decode from the
   keyframe. `perf.stats` reports `decode.cacheMB` and `decode.cacheBudgetMB`; before, this
   memory appeared in no counter (`frames.cache` covers the frame server's rendered images and
   GPU plans only).

4. **Freed planes the allocator keeps (2.5 GB), MEM4.** With MEM1–MEM3 the same twelve clips took
   5.1 GB, only 2.5 GB of it live: every decoded picture allocated three planes and every
   eviction freed them, and the system allocator kept the freed pages (`footprint`: 1.7 GB
   reclaimable). `filmcraft_frame::pool` now keeps the planes of evicted frames (up to 192 MiB per
   sample type; a frame something else still holds is left alone) and the H.264 decoder and the
   VideoToolbox path decode into them, so steady-state decoding allocates no planes.
   `perf.stats` reports `decode.planePoolMB` and `decode.planesReused`.

The same session showed that the desktop app never used hardware decoding: `register()` was an
argument of a `log::info!` that no logger evaluated (fixed under HW1; `decode.hardware.sessions`
was 0 with every frame decoded in software).

What remains per live decoder is unchanged: the H.264 decoder publishes each macroblock row of a
picture as its own allocations (`Frame::make_row`, five per row), which is where the 290 MB per
decoder comes from.

## Results (GPU2: standard effects on the GPU compositor, #30, before → after)

Before = this change with clips that carry standard effects sent back to the CPU layer path in
`render::plan` (same binary otherwise); after = GPU2. New `stack3-fx` scenario: `stack3` with
Brightness & Contrast + Gaussian Blur (blurriness 8) on V2 and Tint (70 %) on V3. `bench_playback
--scenario stack3-fx,stack3 --res full|half --gpu` (release), 8 s plays, two rounds alternating
before / after on 2026-10-05 at load average 107–170 (1-minute; 5-minute 141–152). `stack3` in
the same runs is the control; it shows how much the load alone moved the numbers.

| case | shown/dropped before (2 runs) | after (2 runs) | CPU ms/frame before → after | worker svc p50 before → after |
|---|---|---|---|---|
| stack3-fx Full | 0/192, 0/192 | **192/0, 132/60** | 84, 92 → 94, 89 | 245, 220 → **1.8, 93** ms |
| stack3-fx 1/2 | 9/183, 15/177 | **181/11, 166/26** | 111, 110 → 92, 92 | 295, 307 → **6.5, 28** ms |
| stack3 Full (control) | 180/12, 189/3 | 172/19, 80/112 | 92, 92 → 89, 80 | 7.2, 5.1 → 17.8, 185 ms |
| stack3 1/2 (control) | 138/54, 181/11 | 116/76, 134/53 | 91, 92 → 83, 91 | 61, 15 → 123, 106 ms |

Before, every frame of the effect stack was rendered on the CPU (Gaussian Blur and the two colour
effects on 1080p layers): ≈ 250 ms of worker time per frame, so playback showed nothing at Full
and 5–8 % of frames at ½ (CPU ms/frame stays near 90 only because most jobs were cancelled
before finishing). After, the workers only decode, as for `stack3`: the effect stage runs on the
GPU at present time (UI present p50 11–24 ms vs 11–39 ms for the control in the same runs). The
second "after Full" run coincided with a load spike (load 155 → 170; the control dropped 112
frames in the same minute).

## Results (HW1: VideoToolbox hardware decoding, Off → Auto)

Same commit, Settings ▸ Playback ▸ Hardware decoding switched with the bench flag `--hw off` /
`--hw auto` (`cargo xtask bench --sections decode --only dec_h --repeat 3 --hw …`, then
`--sections playback --only h264` and `--only hevc-2160`), 2026-10-05, Apple M4 Pro, load average
**150–190** throughout (shared with other agent builds). The decoded pictures are identical either
way (bit-exact parity tests, `crates/platform/tests/videotoolbox.rs`); only who decodes changes.

### Decode (every frame through the media stack)

| codec | size | CPU ms/frame Off → **Auto** | fps Off → **Auto** (best of 3) |
|---|---|---|---|
| H.264 | 1080p | 31.8 → **1.2** | 153 → **389** |
| H.264 | 2160p | 119.3 → **4.2** | 35 → **107** |
| HEVC | 1080p | 22.2 → **1.1** | 185 → **708** |
| HEVC | 2160p | 85.7 → **3.7** | 49 → **217** |

CPU time per frame is what is left on the CPU: the sample copy into a `CMSampleBuffer`, copying
the decoded biplanar picture out into planar Y'CbCr (chroma deinterleaved; 10-bit shifted down),
and the GOP cache. An earlier synchronous version (one picture decoding at a time) reached only
155 / 51 / 449 / 149 fps; asynchronous decompression with two access units in flight overlaps
decoding with the copy-out.

### Program-monitor playback (8 s, GPU path)

| case | shown/dropped Off → **Auto** | CPU ms/frame Off → **Auto** |
|---|---|---|
| H.264 1080p Full | 192/0 → **192/0** | 38.1 → **3.8** |
| H.264 2160p Full | 104/88 → **192/0** | 119.8 → **9.2** |
| H.264 2160p 1/2 | 1/191 → **192/0** | 95.5 → **9.0** |
| H.264 2160p 1/4 | 0/192 → **192/0** | 94.3 → **8.8** |
| HEVC 2160p Full | 175/17 → **192/0** | 105.6 → **9.1** |
| HEVC 2160p 1/2 | 190/2 → **192/0** | 97.4 → **8.8** |
| HEVC 2160p 1/2 draft | 190/2 → **191/1** | 107.0 → **22.3** |

At this load the software decoders drop most 4K H.264 frames; with hardware decoding every case
plays in real time at a tenth of the CPU. Draft playback costs more CPU than full-quality
playback with hardware decoding: the hardware ignores draft mode (`set_draft` is a no-op) and
the draft plan's box decimation of the planes, cheap next to software decoding, is now the main
CPU cost. Zero-copy upload of the decoded `CVPixelBuffer` into wgpu (no copy-out at all) is the
next step (issue #30).

## Results (HW2: Windows Media Foundation / Direct3D 11 hardware decoding, Off → Auto)

Same binary (release build of the HW2 commit), Settings ▸ Playback ▸ Hardware decoding switched
with `--hw off` / `--hw auto`, 2026-10-07: Intel Xeon E5-2680 v4 (14 cores / 28 threads, 2.4 GHz),
NVIDIA GeForce RTX 5060 8 GB (driver 617.14), Windows 11 Pro 26200, idle machine. Two alternating
rounds; the table gives the range of the two (decode: best of 3 within each). Reproduce with
`cargo xtask bench --sections decode --only dec_h264 --repeat 3 --hw off|auto` (likewise `dec_hevc`)
and `--sections playback --only "h264-2160 full" --hw off|auto`; set `FILMCRAFT_FFMPEG` if ffmpeg is not
on `PATH` for the fixture generator. Pictures are identical either way (bit-exact parity tests,
`crates/platform/tests/media_foundation.rs`). The decoder MFTs were Microsoft's H.264 decoder
(`Microsoft H264 Video Decoder MFT`) and the `HEVCVideoExtension` decoder, both decoding with DXVA
on the RTX 5060's NVDEC: `nvidia-smi dmon -s u` showed the decoder engine at 18–27 % during an
Auto run and 0 % otherwise, and every picture came back as a Direct3D 11 texture.

### Decode (every frame through the media stack)

| codec | size | CPU ms/frame Off → **Auto** | fps Off → **Auto** |
|---|---|---|---|
| H.264 | 1080p | 77–80 → **3.3** | 162–166 → **347–353** |
| H.264 | 2160p | 310–315 → **11.9** | 39–43 → **93** |
| HEVC Main | 1080p | 69–73 → **3.5** | 89–93 → **307–308** |
| HEVC Main | 2160p | 315–348 → **12–13** | 26–28 → **87–90** |
| HEVC Main 10 | 2160p | 314–336 → **17.6** | 32 → **57** |

Counters: every Auto row had `hw frames` = frames × 3 repeats (360 at 1080p, 216 at 2160p), 3
sessions, 0 fallbacks, 0 declined; every Off row had 0 hardware frames and 0 sessions.

What is left on the CPU is the readback: `examples/mfprobe.rs --time` splits a 2160p H.264 picture
into 1.1 ms DXVA decode (feeding the MFT), 2.8 ms GPU to CPU copy (staging texture + map) and 7 ms
CPU conversion (Annex B, biplanar to planar Y'CbCr); Main 10 is 1.4 / 4.5 / 10 ms (twice the bytes).
A zero-copy path removes the last two.

### Program-monitor playback (8 s, GPU path)

| case | shown/dropped Off → **Auto** | CPU ms/frame Off → **Auto** |
|---|---|---|
| H.264 1080p Full | 192/0 → **192/0** | 86–89 → **8.5–8.8** |
| H.264 2160p Full | 192/0 → **189–190/2–3** | 338–353 → **17–20** |
| H.264 2160p 1/2 | 192/0 → **190/2** | 355–361 → **22** |
| HEVC 2160p Full | 112–152/40–80 → **191–192/0–1** | 355–364 → **18** |

This machine has 28 software-decoding threads, so Off keeps up with 4K H.264 (at 340 ms of CPU per
frame, about 8 cores busy); Auto uses a twentieth of that CPU but drops 2–3 of 192 frames
(readback latency at the start of the run). 4K HEVC is where Off drops frames and Auto does not.
Draft playback (1/2 draft: 410–416 → 93–105 CPU ms/frame) is dominated by the draft plan's decimation,
as on macOS, because the hardware ignores draft mode.

## Results (HW3: Windows zero-copy decoding, readback → zero-copy)

Same binary and machine as HW2 (Xeon E5-2680 v4, RTX 5060, idle), 2026-10-07, `playback` section, two
rounds. Three modes: **Off** (software), **readback** (HW2: decode on the GPU, copy each picture to
the CPU, upload it again; the default Vulkan renderer) and **zero-copy** (HW3: DX12 renderer with DXC,
the compositor samples the decoder's memory). Reproduce with
`FILMCRAFT_DXC_DIR="C:\Program Files (x86)\Windows Kitsin.0.26100.0d" cargo xtask bench --sections playback --only "h264-2160 full" --hw auto`
(without `FILMCRAFT_DXC_DIR` the bench uses the default renderer and CPU pictures).

| case | shown/dropped Off / readback / **zero-copy** | CPU ms/frame Off / readback / **zero-copy** | decode ms per job readback → **zero-copy** |
|---|---|---|---|
| H.264 1080p Full | 192/0 / 192/0 / **192/0** | 89-91 / 7.8-8.5 / **2.9-3.3** | 17.5-19.0 → **7.1-7.2** |
| H.264 2160p Full | 192/0 / 189-190/2-3 / **192/0** | 344-350 / 22.4-23.3 / **6.5-7.2** | 30.4-31.0 → **12.2-12.5** |
| HEVC 2160p Full | 151-165/27-41 / 190/2 / **192/0** | 360-367 / 17.5-17.7 / **5.2-5.4** | 27.7-28.9 → **7.8** |

Every zero-copy row decoded all its pictures as GPU surfaces (`zeroCopyFrames` = `frames`, 206-208 of
206-208). What is left per frame is the compositor's draw and the GPU to GPU copy into the shareable
texture. The readback-path drops of HW2 (2-3 of 192) are gone.

## Results (GPU1: blend modes on the GPU compositor, #30, before → after)

Before = this change with the old whole-frame CPU fallback for non-Normal blend modes put back
in `render::plan` (same binary otherwise); after = GPU1. New `stack3-blend` scenario: `stack3`
(three 1080p23.976 H.264 clips, V2/V3 scaled, positioned, rotated, 70–85 % opacity) with V2 in
Screen and V3 in Overlay. `cargo xtask bench-playback --scenario stack3-blend,stack3 --res
full,half`, GPU path, 8 s plays, two alternating rounds each on 2026-10-05 at load average
33–63 (1-minute; 5-minute 67–81).

| case | shown/dropped before (2 runs) | after (2 runs) | CPU ms/frame before → after | worker svc p50 before → after |
|---|---|---|---|---|
| stack3-blend Full | 19/173, 138/54 | **191/1, 192/0** | 168, 214 → **86, 87** | 258, 219 → 1.2, 1.2 ms |
| stack3-blend 1/2 | 192/0, 192/0 | 191/1, 192/0 | 158, 151 → **89, 88** | 66, 99 → 1.1, 1.0 ms |
| stack3 Full (control, Normal) | 192/0, 192/0 | 191/1, 192/0 | 88, 88 → 91, 88 | 1.3, 1.6 → 0.9, 1.5 ms |
| stack3 1/2 (control, Normal) | 191/1, 192/0 | 192/0, 192/0 | 82, 90 → 90, 89 | |

A stack with blend modes now costs what the same stack in Normal does: the frame workers only
decode, and the UI-thread present (upload and draw, including the backdrop copies for the two
blended layers) stays at 9–10 ms p50, as for `stack3`. Before, every frame was composited on the
CPU (≈ 2 cores more at 23.976 fps) and Full resolution could not keep up at this load.

## Results (M4.10: 4K VP9, AV1 and HEVC, before → after)

Baseline = commit `a814ff6` (decoders unchanged since M4.8); after = M4.10. Interleaved runs on
2026-10-03 at load average 170-320 (two base and two after rounds; the second bench round of
each overlapped another bench run, so its wall-clock columns are extra pessimistic).

### Decoder work per frame (load-independent)

Codec examples (`vp9dec`, `av1dec`, `hevcdec`) on the `cargo xtask bench` decode clips, CPU
cycles counted by the kernel (`/usr/bin/time -l`, all threads summed), mean of the two rounds;
output bit-exact with ffmpeg / libdav1d at every thread count:

| stream | 1 thread before → after | 14 threads before → after | wall ms / frame, 14 threads, before → after |
|---|---|---|---|
| VP9 1080p | 68.0 → **52.0** (−24 %) | 100.7 → **57.9** (−43 %) | 95, 82 → 18, 17 |
| VP9 2160p | 273.6 → **205.8** (−25 %) | 391.7 → **219.2** (−44 %) | 146, 118 → 38, 36 |
| HEVC 1080p | 90.9 → **67.3** (−26 %) | 100.2 → **75.1** (−25 %) | 17, 10 → 5, 13 |
| HEVC 2160p | 362.6 → **262.3** (−28 %) | 398.3 → **284.8** (−28 %) | 24, 55 → 26, 25 |
| AV1 1080p | 60.5 → **48.3** (−20 %) | 63.6 → **51.9** (−18 %) | 26, 8 → 8, 7 |
| AV1 2160p | 212.5 → **172.4** (−19 %) | 220.0 → **177.1** (−20 %) | 158, 39 → 20, 32 |

Mcycles per frame. Draft mode (1 thread): HEVC 2160p 232.5 (−11 % on top), AV1 2160p 167.9
(−3 %: SVT-AV1's top-layer frames filter little); the VP9 clips have no non-reference frames.

### Decode (every frame through the media stack)

| codec | size | CPU ms/frame before (2 rounds) | after (2 rounds) | fps before → after |
|---|---|---|---|---|
| VP9 | 1080p | 33.6, 30.8 | **23.5, 21.7** | 14, 17 → 67, 56 |
| VP9 | 2160p | 120.2, 117.7 | **68.4, 67.9** | 9.1, 7.8 → **34, 27** |
| HEVC | 1080p | 31.2, 29.3 | **23.9, 22.4** | 50, 58 → 138, 145 |
| HEVC | 2160p | 117.9, 111.8 | **86.9, 82.1** | 13, 14 → **30, 41** |
| AV1 | 1080p | 18.5, 18.9 | **15.1, 15.7** | 53, 44 → 122, 69 |
| AV1 | 2160p | 64.8, 66.7 | **48.4, 51.3** | 16, 8.9 → **25, 25** |

### Program-monitor playback, 2160p (8 s, GPU path; new `vp9-2160` / `av1-2160` scenarios)

| case | before: shown/dropped (4 runs) | after (4 runs) | CPU ms/frame before → after |
|---|---|---|---|
| VP9 Full | 6/186, 53/139, 0/192, 0/192 | **192/0, 192/0, 189/3, 192/0** | 23-49 → 75-83 |
| VP9 1/2 | 6/186, 58/134, 2/190, 52/140 | **192/0, 191/1, 192/0, 192/0** | 34-70 → 78-80 |
| HEVC Full | 6/186, 1/191, 0/192, 9/183 | **192/0, 192/0, 192/0, 192/0** | 74-124 → 91-97 |
| HEVC 1/2 | 0/192, 9/183, 0/192, 19/173 | **192/0, 192/0, 192/0, 185/7** | 64-118 → 94-97 |
| HEVC 1/2 draft | 5/187, 88/104, 97/95, 138/54 | **188/4, 192/0, 187/5, 189/3** | 135-148 → 96-104 |
| AV1 Full | 4/188, 19/173, 0/192, 36/156 | 52/140, 0/192, **178/14**, 0/192 | 31-47 → 38-56 |
| AV1 1/2 | 32/160, 44/148, 1/191, 38/154 | 2/190, **150/42, 184/8**, 50/142 | 30-50 → 41-72 |
| AV1 1/2 draft | 1/191, 24/168, 1/191, 27/165 | **192/0, 187/5, 184/8, 192/0** | 25-51 → 68-70 |

CPU per *displayed* frame rises where the decoder now keeps up: before, most frames were never
decoded (dropped jobs cost nothing). At load ~180-220, 4K VP9 and HEVC now play in real time at
Full and 1/2; AV1 does at 1/2 with draft decoding and sometimes at 1/2 / Full: its frames
decode concurrently only once their references are complete (whole-frame dependencies) and the
clip has a single tile, so a 4K frame waits for its references' deblocking, CDEF and output
copy.

## What M4.10 changed

1. **VP9 frame threading** (the decoder had none: tile columns, then a loop-filter wavefront,
   then assembly and output, each a barrier — 14 threads used 40-50 % more cycles than one and
   barely ran faster). Frames are now published one superblock row at a time once loop
   filtered; each frame's post stage (band assembly from the tile strips, loop filter in raster
   order, publication, output picture) runs on its own thread while the next frames' tile
   columns decode, their motion compensation waiting per band for exactly the reference rows it
   reads. Backward-adaptive streams only wait for the previous frame's symbol counts.
2. **VP9 kernels**: the loop filter's lane masks were `bool`s with `clamp` and branchy selects,
   which kept it scalar; 0 / -1 integer masks, bitwise selects and min / max make it NEON. The
   inverse transform's column pass runs the generated butterflies on 4 / 8 columns at once;
   8-bit sub-pixel filters accumulate in 16-bit lanes (every VP9 filter sum fits modulo 2^16).
3. **HEVC**: motion compensation zeroed two 10 KB windows per block (11 % of the time in
   `memset`) and filtered with dynamic-width scalar sums; it now reuses scratch windows and
   filters taps-outer over fixed widths. SAO (20 % of the time) selects edge / band offsets per
   lane instead of a table lookup per sample. Runs of bypass bins decode with one division;
   output clipping uses min / max. Frame threading with per-CTB-row waits was already in place.
4. **AV1**: fixed-width sub-pixel filter passes; frame and tile-region planes recycled through a
   bounded pool instead of a fresh `calloc` / `munmap` per frame (~10 % of the time); palette
   mode-info arrays (40 bytes per 4x4) only with screen content tools; branch-free CDF update.
5. **Draft mode** for all three through `VideoDecoder::set_draft` (Settings ▸ Playback ▸ Draft
   decoding, reduced-resolution playback only): VP9 frames that refresh no reference slot skip
   the loop filter; HEVC sub-layer non-reference pictures of the top sub-layer skip deblocking
   and SAO; AV1 shown frames that refresh no slot skip deblocking, CDEF and loop restoration.
   Tested per codec (unflagged pictures bit-exact, single- and multi-threaded) and through the
   media stack (`crates/codecs/tests/draft_decoders.rs`: draft frames reach draft requests only;
   an export after draft playback is bit-exact with ffmpeg).

Not done: AV1 row-level frame threading (references are waited for as whole frames) and its
per-edge deblocking; VP9 and HEVC coefficient / CABAC parsing are now the largest single costs
(~27 %); HEVC reference windows are still copied per block (~11 %).

## Results (M4.9: 4K H.264, before → after)

Baseline = commit `3d14099` (M3.12, unchanged decoder); after = M4.9. Interleaved runs on
2026-10-02/03, load average 92–178 (the playback rows say which load each run saw).

### Decoder work per frame (load-independent)

`h264dec` example, one decoder, CPU cycles and instructions counted by the kernel (all threads),
output bit-exact with ffmpeg:

| stream | Mcycles / frame before | after | after, draft mode |
|---|---|---|---|
| 2160p (`a2160.mp4`, 170 Mbit/s, first 48 frames) | 505 | **390** (−23 %) | **356** (−30 %) |
| 1080p (`dec_h264_1080.mp4`, 120 frames) | 132 | **100** (−24 %) | **90** (−32 %) |

The cycle count is the same with 1, 4 or 14 frame threads (no spinning or contention overhead):
at ~4.5 GHz a 4K frame is ~85 ms of one core, so 23.976 fps needs about two cores.

### Decode (every frame through the media stack)

| codec | size | CPU ms/frame before (2 rounds) | after (2 rounds) |
|---|---|---|---|
| H.264 | 1080p | 35.8, 39.5 | **33.0, 29.4** |
| H.264 | 2160p | 138.9, 152.1 | **125.7, 112.3** |

(A run at load ~190 before any change gave 41.6 / 160.2 ms; an intermediate build 32.2 / 118.3.)

### Cold seeks on the 4K fixture (12 random targets, GOP 250, fresh source per seek)

| mode | CPU ms per seek before | after | wall p50 ms before → after |
|---|---|---|---|
| full decode from the keyframe | 7529, 7418 | **5531, 5268** (−27 %) | 2757, 1848 → 924, 739 |
| keep 2 s (scrub) | 7488, 7373 | **5442, 5397** | 2408, 1995 → 872, 795 |
| late (playback catch-up) | 5597, 5487 | **4079, 3949** | 2014, 1248 → 735, 526 |

Samples decoded per seek are identical (~48, ~34 in catch-up): the same pictures, decoded with
less work. The cold seek is a chain of reference pictures, so it shows the per-picture cost
directly.

### Program-monitor playback, 2160p (8 s, GPU path)

| case | before: shown/dropped (load) | after: shown/dropped (load) | CPU ms/frame before → after |
|---|---|---|---|
| Full | 192/0 (92), 9/183 (167) | 113/79 (171), **192/0** (139) | 159 (no skips), 133 (17 skipped) → 131, 130 |
| 1/2 | 12/180 (95), 5/187 (169) | 173/19 (172), **192/0** (124) | 143, 127 → 133, 132 |
| 1/2 draft | – | 183/9 (175), **192/0** (119) | – → 144, 134 |
| 1/4 | – | 186/6 (177), **192/0** (111) | – → 134, 125 |
| 1/4 draft | – | **192/0** (178), 190/2 (101) | – → 132, 131 |
| 1080p Full | 192/0, 192/0 | 192/0, 192/0 | 41, 48 → 39, 36 |

Before, the 4K clip kept up in one run (Full at load 92; 1/2 dropped 94 % at load 95) and dropped
95–97 % at load ~168. After, every 4K case plays 192/0 at load 100–140; at load 171–178 Full drops
41 %, 1/2 10 % (5 % with draft decoding), 1/4 3 % (none with draft decoding). CPU per
displayed frame (whole process: decode, frame workers, compositing) drops ~15 % where no frames
were skipped; draft decoding at 1/2 and 1/4 is within run-to-run noise in this column (its saving
is the ~9 % of decoder cycles above, plus a quarter / sixteenth of the texture upload), while the
frame jobs wait for the decoder less (decode ms/job 212 → 67 at 1/2, 98 → 44 at 1/4 in the busy
round). On an idle machine the 4K clip needs about two cores.

## What M4.9 changed

1. **Deblocking** (25–30 % of 4K decode time before): the edge filter runs on all 16 luma / 8
   chroma lines of an edge at once with per-lane masks and min/max clipping (no `clamp`, whose
   bounds assert blocked vectorisation), so it compiles to NEON; edges whose alpha or beta index
   is 0 are skipped. Bit-exact with the per-line filter (random-edge unit test) and ffmpeg.
2. **CABAC**: `decode_decision` / `decode_bypass` select instead of branching on the bin value
   (the MPS/LPS outcome is close to random at high bit rates), renormalisation has no branch on
   the range, and context / state indexing has no bounds checks.
3. **Inverse transforms**: the 4x4 / 8x8 column pass and the add/clip run across all columns.
4. **Frame threading** was already in place (M2): pictures decode concurrently and wait per
   macroblock row for the reference rows their motion vectors reach, with deblocking pipelined row
   by row inside each picture's job. The conformance suite now decodes every fixture with 1, 3 and
   all threads (bit-exact each time). A separate deblocking thread was not added: a picture's rows
   are published about two rows after they are reconstructed, so a reference chain advances a few
   rows behind its predecessor and the pool already has more pictures in flight than cores
   (crates/h264/README.md, "Threading model").
5. **Draft decoding** (Settings ▸ Playback ▸ Draft decoding, off by default; `perf.stats`
   `playback.draftDecode`, `decode.draftFrames`, `decode.h264Threads`): while the Program monitor
   plays at 1/2 or 1/4, H.264 non-reference pictures skip deblocking (no other picture can change),
   and draft plans hand the GPU box-decimated Y'CbCr planes at the drawn size. Draft frames carry
   their own frame-cache keys and the GOP cache serves them to draft requests only, so pausing,
   rendering and exporting always decode exact pictures (tested through the media stack against
   ffmpeg after draft playback). The CPU renderer already converted reduced-resolution frames
   straight to the decimated size (`to_linear_f32_decimated`).

Not done: CABAC residual decoding (now 41 % of the work, ~19 cycles per bin) is inherently serial;
the remaining deblocking cost is mostly the vertical-edge transposes; motion compensation and the
per-row / per-picture copies (4 %) are unchanged.

## Results (M4.8, before → after)

Baseline = commit `457aca1` (the harness on unchanged code); after = M4.8. Runs alternated base /
after (2 rounds each, 2026-10-02).

### Decode (every frame through the media stack: container, GOP cache, decoder, conversion)

CPU ms per frame (both rounds; lower is better). fps is the best of the run at that moment's load.

| codec | size | CPU ms/frame before | after | fps before → after (same-round pairs) |
|---|---|---|---|---|
| H.264 | 1080p | 43.4 / 43.7 | 48.2 / 46.1 | 101, 105 → 51, 34 (load 99 → 127) |
| H.264 | 2160p | 169 / 166 | 171 / 178 | 17, 31 → 16, 9 |
| **HEVC** | 1080p | 80.5 / 76.4 | **31.4 / 34.5** | 25, 58 → 69, 41 |
| **HEVC** | 2160p | 325 / 314 | **122 / 136** | 8.2, 12 → 17, 11 |
| VP9 | 1080p / 2160p | 39.7 / 137.6 | 36.5 / 136.8 | 18 / 12 → 18 / 7.8 |
| AV1 | 1080p / 2160p | 20.2 / 78.2 | 19.7 / 72.6 | 106 / 23 → 50 / 9.4 |
| ProRes 422 HQ | 1080p / 2160p | 23.6 / 89.0 | 23.2 / 86.7 | 59 / 28 → 34 / 15 |

On a quieter moment earlier in the session (load ~70) the same binaries decoded H.264 at 100 fps
(1080p) / 24 fps (2160p), AV1 131 / 44 fps and ProRes 202 / 49 fps.

### Seeking (cold seek through the media stack, 12 random targets, modes interleaved, load ~150)

| fixture (GOP 250) | full decode from keyframe | skip non-reference > 2 s before (scrub) | skip all late non-reference (playback catch-up) |
|---|---|---|---|
| H.264 1080p | 1405 ms, 3.3 s CPU | 1276 ms, 3.1 s CPU | **789 ms, 2.3 s CPU** |
| H.264 2160p | 3394 ms, 8.5 s CPU | 2828 ms, 8.4 s CPU | **2274 ms, 6.3 s CPU** |
| HEVC 2160p | 4091 ms, 17.8 s CPU | 3058 ms, 12.3 s CPU | **2512 ms, 9.8 s CPU** |

The target frame is identical in every mode (only pictures nothing references are left out).

### Program-monitor playback (8 s, GPU path)

| scenario | shown/dropped before | after | CPU ms/frame before → after | non-ref samples skipped |
|---|---|---|---|---|
| H.264 1080p | 192/0, 192/0 | 191/1, 191/1 | 55 → 55 | 0 (keeps up) |
| 3 × 1080p stacked | 192/0, 190/2 | 188/4, 185/7 | 120 → 128 | 0 |
| H.264 2160p Full | 22/170, 192/0 | 0/192, 0/192 | 183 → **87** | 39–52 |
| H.264 2160p Half | 180/12, 192/0 | 0/192, 0/192 | 198 → **102** | 46–49 |
| HEVC 2160p Full | 0/192, 0/192 | 0/192, 0/192 | 164 → **90** | 88 |
| HEVC 2160p Half | 0/192, 0/192 | 0/192, 0/192 | 160 → **97** | 33–92 |

Shown/dropped at 2160p is decided by the load at the moment (base round 2 happened to play 192/0
at a quiet minute; a later three-way run at load 175–195 gave 0–21 shown for every binary, and
143/49 for HEVC once). CPU per displayed second halved at 2160p: frames that are already late are
no longer fully decoded, and HEVC decodes 2.5× cheaper.

### Scrubbing (24 random seeks + 8 playhead drags through the monitor's request path)

| scenario | seek p50 / p95 ms before | after | drag-stop p50 ms before → after |
|---|---|---|---|
| H.264 1080p | 357 / 940, 582 / 1535 | 840 / 1567, 770 / 1389 | 27, 62 → 281, 103 |
| H.264 2160p | 1063 / 2377, 1499 / 3028 | 2445 / 5357, 2390 / 4562 | 288, 513 → 560, 834 |
| HEVC 2160p | 130 / 4100 (9 timeouts), 318 / 1397 | 2466 / 6441, 2476 / 7596 (0 timeouts) | 9, 3872 → 462, 1893 |

These runs coincided with load rising from 99 to 170 for "after". A later interleaved three-way
run (base / after with catch-up disabled / after) at load 80–200 gave H.264 1080p seek p50 345 /
712 / 572 ms then 379 / 389 / 461 ms, and H.264 2160p 913 / 1312 / 2105 then 1946 / 931 / 840 ms:
within the noise. The cold-seek table above is the load-independent comparison.

### Timeline UI (1000 clips on 20 tracks, 1920×1080 window, `egui_kittest`)

| view | update p50 / p95 ms before | after | tessellate p50 ms | wgpu render p50 ms (incl. readback) | vertices |
|---|---|---|---|---|---|
| fit (all 1000 clips) | 3.1 / 4.9, 3.4 / 5.7 | 2.9 / 4.5, 2.9 / 3.3 | 0.3 | 15 | 35.7 k |
| zoomed, scrolling | 2.8 / 3.8, 2.8 / 4.8 | 2.9 / 4.9, 2.6 / 6.8 | 0.2 | 15 | 29.2 k |

The timeline is not a bottleneck: the whole app frame (all panels) costs ~3 ms with 1000 clips
visible; it already culls clips outside the view. The waveform display gain no longer rescans the
whole source's peaks for every clip every frame (cached per peak list).

### Export (8 s of 1080p23.976 H.264 + AAC source)

| format | fps before → after | CPU ms/frame before → after |
|---|---|---|
| H.264 + AAC | 10.6, 5.8 → 10.2, 11.9 | 256 → 250 |
| ProRes 422 | 4.7, 3.0 → 6.3, 9.6 | 249 → 232 |

Profile (`sample`): H.264 export is ~45 % encoder, ~30 % decoding the noisy 45 Mbit/s source,
~25 % CPU compositing; ProRes export spends most in `prores::encode::slice_bits` /
`choose_quantisers` (rate search), then source decode and compositing. Not changed in M4.8.

### Project save / open (5 sequences × 1000 clips, 3.7 MB)

save 24 / 68 → 36 / 21 ms, open 26 / 82 → 21 / 19 ms (p50 of 3; noise).

### Peak RSS per section (MB)

decode 2642 / 2925 → 2506 / 2064; playback 5246 / 7903 → 3379 / 3121; scrub 4205 / 5022 →
5390 / 4183; timeline 626 / 650 → 660 / 675; export 2367 / 2286 → 2606 / 2619; project 114 / 136 →
124 / 133.

## What M4.8 changed

1. **HEVC inverse transform** (largest hotspot: >50 % of HEVC decode CPU). Sums contiguous basis
   rows scaled by non-zero coefficients, size as a const generic so loops vectorise; bit-exact
   (exact integer sums), tested against the direct matrix product on random blocks and by the
   ffmpeg conformance fixtures. **SAO edge offset** gets a check-free inner loop. HEVC CPU per
   frame −60 %.
2. **No redundant plane copies**: H.264 / HEVC / VP9 pictures are moved into `VideoFrame`s instead
   of copied (12 MB per 2160p frame; HEVC 10-bit also lost a per-sample widening pass).
3. **Catch-up decoding**: frame jobs carry how far before their frame the playhead is
   (`filmcraft_media::cancel::with_catch_up`); the GOP cache skips non-reference samples of late
   frames (`VideoDecoder::is_disposable`: H.264 `nal_ref_idc` 0, HEVC sub-layer non-reference
   pictures of the top temporal layer). Scrub requests keep the last 2 s fully decoded so dragging
   back stays cached. The wanted frame is decoded exactly as before; a later request for a skipped
   frame re-seeks. ~50 % of the fixtures' pictures are non-reference.
4. **`perf.stats`** (engine query + UI/control method): decode counters (cache hit rate, seeks,
   samples decoded / skipped, decoder ms), playback shown / dropped, frame-worker decode / render
   ms (p50 / p95), request hit rate, cache use, UI fps, process CPU.
5. **Timeline**: waveform source peak cached.

Not feasible / not done: reduced-resolution decode for H.264 / HEVC (inter prediction needs
full-resolution references, so it cannot be bit-exact; ½/¼ playback already decimates after
decode); AV1 internals untouched (measured only); export encoders not optimised.
