//! Outer transforms must reuse the clipped inner picture while retaining reference pixels.
use filmcraft_engine::Session;
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_geom::Vec2;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, Generator, MediaInfo, MediaSource};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::frames::{FrameKey, GpuPlan, Target};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counted {
    source: GeneratorSource,
    reads: Arc<AtomicUsize>,
}
impl MediaSource for Counted {
    fn info(&self) -> &MediaInfo {
        self.source.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.source.video_frame(req)
    }
    fn audio(&self, start: i64, frames: usize, rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.source.audio(start, frames, rate)
    }
}
fn fixture() -> (FilmcraftApp, ItemId, ItemId, Arc<AtomicUsize>) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("clipped inner reuse");
    let source = GeneratorSource::new(Generator::BarsAndTone, 64, 36, rate, rate.tick_of(24));
    let media = p.add_item(
        "bars",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(Generator::BarsAndTone),
            info: source.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    let settings = SequenceSettings { width: 64, height: 36, frame_rate: rate, ..Default::default() };
    let inner = p.new_sequence("inner", settings.clone(), 1, 0, None);
    let clip = p.make_track_item(media, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    p.sequence_mut(inner).unwrap().video_tracks[0].items.push(clip);
    let outer = p.new_sequence("outer", settings, 1, 0, None);
    let mut nest = p.make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    nest.effect_mut("motion").unwrap().param_mut("position").unwrap().value = ParamValue::Vec2(Vec2::new(34.0, 18.0));
    p.sequence_mut(outer).unwrap().video_tracks[0].items.push(nest);
    let mut session = Session::default();
    session.project = Arc::new(p);
    session.state.active_sequence = Some(outer);
    let reads = Arc::new(AtomicUsize::new(0));
    session.media.insert(media, Arc::new(Counted { source, reads: reads.clone() }));
    (FilmcraftApp::new(session), inner, outer, reads)
}
fn plan(app: &FilmcraftApp, outer: ItemId) -> Arc<GpuPlan> {
    plan_at(app, outer, 0, 1.0, false)
}
fn plan_at(app: &FilmcraftApp, outer: ItemId, frame: i64, scale: f32, draft: bool) -> Arc<GpuPlan> {
    let key = FrameKey { target: Target::SequencePlan(outer), frame, size: (scale * 1000.0) as u32, revision: app.session.revision, draft };
    app.frames.request(key, FrameRate::FPS_24.tick_of(frame), scale, &app.session.project, 0);
    for _ in 0..2000 {
        if let Some(plan) = app.frames.get_plan(&key) {
            return plan;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("program plan did not finish");
}
fn pixels_match_reference(app: &FilmcraftApp, outer: ItemId, plan: &GpuPlan) -> filmcraft_render::Image {
    let source = Arc::new(GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(24)));
    let provider = move |_| Some(source.clone() as filmcraft_media::SharedSource);
    let reference = filmcraft_render::render_sequence(&app.session.project, outer, Tick::ZERO, filmcraft_render::RenderOptions::default(), &provider);
    let actual = filmcraft_render::plan::execute_cpu(&plan.plan);
    assert_eq!((actual.w, actual.h), (reference.w, reference.h));
    assert!(actual.px.iter().zip(&reference.px).all(|(a, b)| (a - b).abs() < 0.02), "cached plan differs from fresh reference");
    actual
}
#[test]
fn outer_position_reuses_the_inner_raster_and_keeps_reference_pixels() {
    let (mut app, _, outer, reads) = fixture();
    let first = plan(&app, outer);
    let first = pixels_match_reference(&app, outer, &first);
    let before = reads.load(Ordering::SeqCst);
    assert!(before > 0, "counted source must actually be used");
    assert!(first.px.as_chunks::<4>().0.windows(2).any(|v| v[0] != v[1]), "fixture must contain visible spatial detail");
    app.session
        .edit("outer position", |p, _| {
            p.sequence_mut(outer).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("position").unwrap().value =
                ParamValue::Vec2(Vec2::new(38.0, 18.0));
            Ok(())
        })
        .unwrap();
    let after = plan(&app, outer);
    let after = pixels_match_reference(&app, outer, &after);
    assert_ne!(first.px, after.px, "outer transform must visibly change its picture");
    assert_eq!(reads.load(Ordering::SeqCst), before, "outer-only position change re-rasterized the unchanged inner picture");
}

fn move_outer(app: &mut FilmcraftApp, outer: ItemId, x: f64) {
    app.session
        .edit("outer position", |p, _| {
            p.sequence_mut(outer).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("position").unwrap().value =
                ParamValue::Vec2(Vec2::new(x, 18.0));
            Ok(())
        })
        .unwrap();
}
#[test]
fn changed_inner_pixels_and_undo_redo_never_reuse_the_wrong_raster() {
    let (mut app, inner, outer, reads) = fixture();
    let before = pixels_match_reference(&app, outer, &plan(&app, outer));
    let n = reads.load(Ordering::SeqCst);
    app.session
        .edit("inner move", |p, _| {
            p.sequence_mut(inner).unwrap().video_tracks[0].items[0].effect_mut("motion").unwrap().param_mut("position").unwrap().value =
                ParamValue::Vec2(Vec2::new(28.0, 18.0));
            Ok(())
        })
        .unwrap();
    let changed = pixels_match_reference(&app, outer, &plan(&app, outer));
    assert_ne!(changed.px, before.px);
    assert!(reads.load(Ordering::SeqCst) > n, "inner edit must rasterize new content");
    app.session.undo().unwrap();
    assert_eq!(pixels_match_reference(&app, outer, &plan(&app, outer)).px, before.px);
    app.session.redo().unwrap();
    assert_eq!(pixels_match_reference(&app, outer, &plan(&app, outer)).px, changed.px);
}
#[test]
fn time_resolution_and_draft_are_separate_picture_contexts() {
    let (mut app, _, outer, reads) = fixture();
    let _ = plan_at(&app, outer, 0, 1.0, true);
    let draft_reads = reads.load(Ordering::SeqCst);
    let _ = plan(&app, outer);
    let full_reads = reads.load(Ordering::SeqCst);
    assert!(full_reads > draft_reads, "draft work must not seed full-quality reuse");
    move_outer(&mut app, outer, 38.0);
    let _ = plan(&app, outer);
    assert_eq!(reads.load(Ordering::SeqCst), full_reads);
    let _ = plan_at(&app, outer, 1, 1.0, false);
    let next = reads.load(Ordering::SeqCst);
    assert!(next > full_reads, "different time must not reuse prior picture");
    let half = plan_at(&app, outer, 1, 0.5, false);
    assert!(reads.load(Ordering::SeqCst) > next, "different raster size must not alias");
    let half = filmcraft_render::plan::execute_cpu(&half.plan);
    assert_eq!((half.w, half.h), (32, 18));
    let source = Arc::new(GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(24)));
    let provider = move |_| Some(source.clone() as filmcraft_media::SharedSource);
    let reference = filmcraft_render::render_sequence(
        &app.session.project,
        outer,
        FrameRate::FPS_24.tick_of(1),
        filmcraft_render::RenderOptions { scale: 0.5, ..Default::default() },
        &provider,
    );
    assert!(half.px.iter().zip(&reference.px).all(|(a, b)| (a - b).abs() < 0.02));
}
#[test]
fn proxy_policy_and_explicit_source_refresh_invalidate_inner_reuse() {
    let (mut app, inner, outer, reads) = fixture();
    let first = pixels_match_reference(&app, outer, &plan(&app, outer));
    let n = reads.load(Ordering::SeqCst);
    app.session.media.set_use_proxies(true);
    move_outer(&mut app, outer, 38.0);
    let _ = pixels_match_reference(&app, outer, &plan(&app, outer));
    let proxy_reads = reads.load(Ordering::SeqCst);
    assert!(proxy_reads > n, "policy change must not reuse old source picture");
    let media = app.session.project.sequence(inner).unwrap().video_tracks[0].items[0].item;
    let generator = Generator::ColorMatte { color: [0.0, 1.0, 0.0, 1.0] };
    app.session.media.insert(
        media,
        Arc::new(Counted { source: GeneratorSource::new(generator.clone(), 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(24)), reads: reads.clone() }),
    );
    move_outer(&mut app, outer, 40.0);
    let result = filmcraft_render::plan::execute_cpu(&plan(&app, outer).plan);
    assert!(reads.load(Ordering::SeqCst) > proxy_reads);
    assert_ne!(result.px, first.px);
    assert!(result.px.as_chunks::<4>().0.iter().any(|p| p[1] > 0.9 && p[0] < 0.01));
    let source = Arc::new(GeneratorSource::new(generator, 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(24)));
    let provider = move |_| Some(source.clone() as filmcraft_media::SharedSource);
    let reference = filmcraft_render::render_sequence(&app.session.project, outer, Tick::ZERO, filmcraft_render::RenderOptions::default(), &provider);
    assert!(result.px.iter().zip(&reference.px).all(|(a, b)| (a - b).abs() < 0.02));
}
#[test]
fn adding_nested_captions_invalidates_the_cached_picture() {
    use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, TrackId};
    let (mut app, inner, outer, reads) = fixture();
    let before = pixels_match_reference(&app, outer, &plan(&app, outer));
    let n = reads.load(Ordering::SeqCst);
    app.session
        .edit("nested caption", |p, _| {
            let mut track = CaptionTrack::new(TrackId(9000), "subtitle".into(), CaptionFormat::Subtitle);
            track.captions.push(Caption {
                id: ClipId(9001),
                start: Tick::ZERO,
                duration: FrameRate::FPS_24.tick_of(24),
                text: "TEXT".into(),
                speaker: None,
                cue_id: None,
                settings: String::new(),
            });
            p.sequence_mut(inner).unwrap().caption_tracks.push(track);
            Ok(())
        })
        .unwrap();
    let after = pixels_match_reference(&app, outer, &plan(&app, outer));
    assert!(reads.load(Ordering::SeqCst) > n);
    assert_ne!(after.px, before.px, "caption fixture must actually change visible pixels");
}

struct DeferredSource {
    source: GeneratorSource,
    reads: Arc<AtomicUsize>,
    entered: Arc<std::sync::atomic::AtomicBool>,
    ready: Arc<std::sync::atomic::AtomicBool>,
    cancel_test: bool,
}
impl MediaSource for DeferredSource {
    fn info(&self) -> &MediaInfo {
        self.source.info()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.entered.store(true, Ordering::SeqCst);
        if !self.ready.load(Ordering::SeqCst) {
            if self.cancel_test {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !filmcraft_media::cancel::cancelled() && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                return Err(filmcraft_media::MediaError::Cancelled);
            }
            filmcraft_media::pending::mark();
            return Err(filmcraft_media::MediaError::Io("generated source still pending".into()));
        }
        self.source.video_frame(req)
    }
    fn audio(&self, start: i64, frames: usize, rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.source.audio(start, frames, rate)
    }
}
fn eventually(message: &str, condition: impl Fn() -> bool) {
    for _ in 0..2000 {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("{message}");
}
#[test]
fn pending_and_cancelled_jobs_do_not_poison_future_inner_pictures() {
    use std::sync::atomic::AtomicBool;
    for cancel_test in [false, true] {
        let (mut app, inner, outer, reads) = fixture();
        let entered = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let media = app.session.project.sequence(inner).unwrap().video_tracks[0].items[0].item;
        app.session.media.insert(
            media,
            Arc::new(DeferredSource {
                source: GeneratorSource::new(Generator::BarsAndTone, 64, 36, FrameRate::FPS_24, FrameRate::FPS_24.tick_of(24)),
                reads: reads.clone(),
                entered: entered.clone(),
                ready: ready.clone(),
                cancel_test,
            }),
        );
        let key = FrameKey { target: Target::SequencePlan(outer), frame: 0, size: 1000, revision: app.session.revision, draft: false };
        app.frames.request(key, Tick::ZERO, 1.0, &app.session.project, 0);
        eventually("source was not entered", || entered.load(Ordering::SeqCst));
        if cancel_test {
            app.frames.retain_queue(|_| false);
        }
        eventually("incomplete job did not finish", || app.frames.stats().jobs >= 1);
        assert!(!app.frames.is_ready(&key), "incomplete work must not be a ready Program plan");
        if cancel_test {
            assert_eq!(app.frames.stats().cancelled, 1);
        }
        ready.store(true, Ordering::SeqCst);
        move_outer(&mut app, outer, 38.0);
        let image = pixels_match_reference(&app, outer, &plan(&app, outer));
        assert!(image.px.as_chunks::<4>().0.iter().any(|p| p[0] > 0.2 || p[1] > 0.2 || p[2] > 0.2));
        let n = reads.load(Ordering::SeqCst);
        assert!(n >= 2, "recovery must read the now-complete source");
        move_outer(&mut app, outer, 40.0);
        let _ = pixels_match_reference(&app, outer, &plan(&app, outer));
        assert_eq!(reads.load(Ordering::SeqCst), n, "successful recovery should enable reuse");
    }
}

#[test]
fn dropping_a_frame_server_releases_its_retained_inner_picture_snapshot() {
    let (app, _, outer, _) = fixture();
    let project = Arc::downgrade(&app.session.project);
    let _ = plan(&app, outer);
    drop(app);
    eventually("idle frame workers retained the completed inner-picture snapshot after its owner was dropped", || project.upgrade().is_none());
}

fn metadata_fixture(alias: bool) -> (FilmcraftApp, ItemId, ItemId, ItemId) {
    let (mut app, inner, outer, _) = fixture();
    let mut rendered_inner = inner;
    app.session
        .edit("sequence-name burn-in", |p, _| {
            let mut e = filmcraft_project::effect::find_effect("metadata_burnin").unwrap().instance();
            e.param_mut("source").unwrap().value = ParamValue::Choice(5);
            e.param_mut("size").unwrap().value = ParamValue::Float(40.0);
            p.sequence_mut(inner).unwrap().video_tracks[0].items[0].effects.push(e);
            p.item_mut(inner).unwrap().name = "AAA".into();
            if alias {
                let kind = p.item(inner).unwrap().kind.clone();
                rendered_inner = p.add_item("alias", Label::Iris, kind, None);
                p.sequence_mut(outer).unwrap().video_tracks[0].items[0].item = rendered_inner;
            }
            Ok(())
        })
        .unwrap();
    (app, inner, outer, rendered_inner)
}
fn rename_metadata_source(app: &mut FilmcraftApp, inner: ItemId) {
    app.session
        .edit("rename visible metadata", |p, _| {
            p.item_mut(inner).unwrap().name = "WWW".into();
            Ok(())
        })
        .unwrap();
}
#[test]
fn metadata_sequence_name_refreshes_the_actual_cached_program_picture() {
    let (mut app, inner, outer, _) = metadata_fixture(false);
    let before = pixels_match_reference(&app, outer, &plan(&app, outer));
    rename_metadata_source(&mut app, inner);
    let after = pixels_match_reference(&app, outer, &plan(&app, outer));
    assert_ne!(before.px, after.px, "renamed metadata must visibly change the picture");
}
#[test]
fn metadata_sequence_name_refreshes_the_thumbnail_identity() {
    let (mut app, inner, _, rendered) = metadata_fixture(false);
    let old = app.item_revision(rendered);
    rename_metadata_source(&mut app, inner);
    assert_ne!(app.item_revision(rendered), old, "rendered sequence name changed outside the sequence Arc");
}
#[test]
fn metadata_sequence_name_alias_refreshes_the_actual_cached_program_picture() {
    let (mut app, inner, outer, _) = metadata_fixture(true);
    let before = pixels_match_reference(&app, outer, &plan(&app, outer));
    rename_metadata_source(&mut app, inner);
    let after = pixels_match_reference(&app, outer, &plan(&app, outer));
    assert_ne!(before.px, after.px, "shared-sequence metadata origin must visibly change");
}
#[test]
fn metadata_sequence_name_alias_refreshes_the_thumbnail_identity() {
    let (mut app, inner, _, rendered) = metadata_fixture(true);
    let old = app.item_revision(rendered);
    rename_metadata_source(&mut app, inner);
    assert_ne!(app.item_revision(rendered), old, "global first-sequence name lookup changed outside the reachable item");
}
