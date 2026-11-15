//! A cancelled frame job (playback moved past its frame) stops at the next layer, effect or row of
//! a CPU effect instead of running to the end, and what it leaves behind is incomplete: the
//! caller (`frames.rs`) discards it. The cancel flag is polled once per layer, per effect and per
//! row, so these tests flip it at a known point instead of timing anything.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::plan::{DeviceCaps, FramePlan, plan_frame};
use filmcraft_frame::AudioBuffer;
use filmcraft_media::cancel::with_cancel;
use filmcraft_media::{MediaInfo, MediaKind, MediaSource, VideoStreamInfo};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const FPS: FrameRate = FrameRate::FPS_24;

/// A green still that counts its decodes and, once decoded, marks the job cancelled: the way a
/// slow decode sees playback move on while it works.
struct Spy {
    info: MediaInfo,
    frame: Arc<VideoFrame>,
    decodes: AtomicUsize,
    cancel_after_decode: Option<Arc<AtomicBool>>,
}

impl MediaSource for Spy {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.decodes.fetch_add(1, Ordering::SeqCst);
        if let Some(f) = &self.cancel_after_decode {
            f.store(true, Ordering::Relaxed);
        }
        Ok(self.frame.clone())
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn spy(cancel_after_decode: Option<Arc<AtomicBool>>) -> Arc<Spy> {
    let (w, h) = (64u32, 36u32);
    let px: Vec<u8> = (0..w * h).flat_map(|_| [0, 200, 50, 255]).collect();
    let info = MediaInfo {
        name: "green".into(),
        kind: MediaKind::Movie,
        duration: Tick(10 * TICKS_PER_SECOND),
        video: Some(VideoStreamInfo {
            width: w,
            height: h,
            frame_rate: FPS,
            par: (1, 1),
            codec: "test".into(),
            pixel_format: "RGBA 8-bit".into(),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            has_alpha: false,
            bitrate: None,
            hdr: None,
            color_assumed: None,
        }),
        audio_streams: Vec::new(),
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    };
    Arc::new(Spy { info, frame: Arc::new(VideoFrame::rgba8(w, h, px)), decodes: AtomicUsize::new(0), cancel_after_decode })
}

/// A sequence with one clip of `source` carrying an Ultra Key (a CPU effect: the plan renders the
/// layer on the CPU, on the frame worker).
fn keyed_clip(source: &Arc<Spy>) -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("cancel");
    let clip = MediaClip {
        media: MediaRef::File { path: "green.mov".into() },
        info: source.info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("green", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let settings = SequenceSettings { width: 64, height: 36, par: (1, 1), frame_rate: FPS, ..Default::default() };
    let sid = p.new_sequence("s", settings, 1, 0, None);
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(24)), FPS).unwrap();
    ti.effects.push(find_effect("ultra_key").unwrap().instance());
    p.sequence_mut(sid).unwrap().video_tracks[0].items.push(ti);
    let mut map = SourceMap::default();
    map.0.insert(item, source.clone() as SharedSource);
    (p, sid, map)
}

fn layer_count(plan: &FramePlan) -> usize {
    match plan {
        FramePlan::Layers { layers, .. } => layers.len(),
        FramePlan::Composite { steps, .. } => steps.len(),
        FramePlan::Image(_) => usize::MAX,
    }
}

#[test]
fn an_uncancelled_plan_job_produces_the_keyed_layer() {
    // the control of the tests below: the same job, never cancelled
    let s = spy(None);
    let (p, sid, map) = keyed_clip(&s);
    let plan = plan_frame(&p, sid, Tick::ZERO, RenderOptions::default(), DeviceCaps::ALL, &map);
    assert_eq!(layer_count(&plan), 1);
    assert_eq!(s.decodes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_plan_job_cancelled_before_it_starts_decodes_nothing_and_makes_no_layer() {
    let s = spy(None);
    let (p, sid, map) = keyed_clip(&s);
    let flag = Arc::new(AtomicBool::new(true));
    let plan = with_cancel(&flag, || plan_frame(&p, sid, Tick::ZERO, RenderOptions::default(), DeviceCaps::ALL, &map));
    assert_eq!(s.decodes.load(Ordering::SeqCst), 0, "a cancelled job must not even decode");
    assert_eq!(layer_count(&plan), 0);
}

#[test]
fn a_plan_job_cancelled_while_decoding_skips_the_effects_and_makes_no_layer() {
    let flag = Arc::new(AtomicBool::new(false));
    let s = spy(Some(flag.clone()));
    let (p, sid, map) = keyed_clip(&s);
    let plan = with_cancel(&flag, || plan_frame(&p, sid, Tick::ZERO, RenderOptions::default(), DeviceCaps::ALL, &map));
    assert_eq!(s.decodes.load(Ordering::SeqCst), 1);
    assert_eq!(layer_count(&plan), 0, "the keyed layer of a cancelled job must not be produced");
    // the worker is free at once: the same thread, with no flag, renders the next job in full
    let again = plan_frame(&p, sid, Tick::ZERO, RenderOptions::default(), DeviceCaps::ALL, &map);
    assert_eq!(layer_count(&again), 1);
}

fn ctx() -> effects::FxCtx<'static> {
    effects::FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        mask_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "clip",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

#[test]
fn the_keyers_blur_and_per_pixel_map_stop_once_cancelled() {
    let green = || Image::filled(32, 32, [0.0, 0.6, 0.1, 1.0]);
    let flag = Arc::new(AtomicBool::new(true));

    for id in ["ultra_key", "color_key"] {
        let key = find_effect(id).unwrap().instance();
        // Not cancelled: the key works on the picture (the control).
        let mut keyed = green();
        effects::apply(&mut keyed, &key, &ctx());
        assert_ne!(keyed.px, green().px, "{id}: the key changes the picture");
        // Cancelled: no row is processed, the picture is left as it was.
        let mut stopped = green();
        with_cancel(&flag, || effects::apply(&mut stopped, &key, &ctx()));
        assert_eq!(stopped.px, green().px, "{id}");
    }

    let mut blurred = Image::new(32, 32);
    blurred.px[(16 * 32 + 16) * 4..][..4].copy_from_slice(&[1.0; 4]);
    let dot = blurred.clone();
    effects::gaussian(&mut blurred, 3.0, 3.0, false);
    assert_ne!(blurred.px, dot.px, "control: the blur spreads the dot");
    let mut stopped = dot.clone();
    with_cancel(&flag, || effects::gaussian(&mut stopped, 3.0, 3.0, false));
    assert_eq!(stopped.px, dot.px);

    let mut mapped = green();
    mapped.map_rgb(|_, _, _| [1.0, 1.0, 1.0]);
    assert!(mapped.px[0] > 0.9, "control: the map ran");
    let mut stopped = green();
    with_cancel(&flag, || stopped.map_rgb(|_, _, _| [1.0, 1.0, 1.0]));
    assert_eq!(stopped.px, green().px);
}

/// A busy-wait of about `us` microseconds that the optimiser cannot remove.
fn spin(us: u64) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_micros(us) {
        std::hint::black_box(0);
    }
}

/// Two jobs share rayon's pool, the way two frame workers do. A pool thread blocked inside job A's
/// parallel call steals job B's tasks while it waits, and keeps A's thread-local cancel flag
/// meanwhile: B's effect passes must read B's own flag (`Context::with`), or a cancelled A would
/// stop B half-way and the incomplete picture would be cached.
#[test]
fn a_cancelled_job_never_stops_another_jobs_effect_on_the_shared_pool() {
    use rayon::prelude::*;
    if rayon::current_num_threads() < 2 {
        return;
    }
    let src = {
        let mut img = Image::new(96, 96);
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            let v = ((i * 37) % 255) as f32 / 255.0;
            p.copy_from_slice(&[v, 1.0 - v, (i % 7) as f32 / 7.0, 1.0]);
        }
        img
    };
    let mut focus = find_effect("focus_blur").unwrap().instance();
    for (k, v) in [("amount", 40.0), ("size", 5.0), ("feather", 10.0)] {
        if let Some(p) = focus.params.get_mut(k) {
            p.value = filmcraft_project::ParamValue::Float(v);
        }
    }
    let run_b = |flag: &Arc<AtomicBool>| {
        let mut img = src.clone();
        with_cancel(flag, || effects::apply(&mut img, &focus, &ctx()));
        img
    };
    let reference = run_b(&Arc::new(AtomicBool::new(false)));
    assert_ne!(reference.px, src.px, "control: the effect changes the picture");

    for round in 0..200 {
        let a_flag = Arc::new(AtomicBool::new(true));
        let b_flag = Arc::new(AtomicBool::new(false));
        let got = std::thread::scope(|s| {
            // job A: many tasks, each blocked in a nested parallel call inside A's context
            s.spawn(|| {
                with_cancel(&a_flag, || {
                    let ctx = filmcraft_media::cancel::Context::current();
                    (0..64).into_par_iter().for_each(|_| {
                        ctx.with(|| {
                            (0..4).into_par_iter().for_each(|_| spin(150));
                        })
                    });
                })
            });
            s.spawn(|| run_b(&b_flag)).join().unwrap()
        });
        assert_eq!(got.px, reference.px, "round {round}: job B's picture was stopped by job A's flag");
    }
}
