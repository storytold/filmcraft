use std::sync::Arc;

use filmcraft_project::{
    ClipId,
    raster_mask::{RasterFrame, RasterMask},
};
use serde_json::json;

use crate::{
    Session,
    ai_masks::{Backend, Input},
};

struct Fake;
impl Backend for Fake {
    fn infer(&self, input: &Input, _: &filmcraft_export::Progress) -> Result<RasterMask, String> {
        let pixels = input.width * input.height;
        Ok(RasterMask {
            width: input.width,
            height: input.height,
            end: input.end,
            quality: input.quality,
            frames: input.frames.iter().map(|f| RasterFrame { time: f.time, runs: Arc::new(vec![(pixels / 2, 0), (pixels - pixels / 2, 32768)]) }).collect(),
        })
    }
}

fn setup() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.set_playhead(item.start);
    s.state.selection = vec![item.id];
    s.ai_mask_backend = Some(Arc::new(Fake));
    s.execute("masks.ai.select", json!({})).unwrap();
    s.execute("masks.ai.settings", json!({"frames":3,"mode":"quality"})).unwrap();
    (s, item.id)
}

fn raster_count(s: &Session, clip: ClipId) -> usize {
    s.active_sequence().unwrap().find_item(clip).unwrap().1.effect("opacity").unwrap().masks.iter().filter(|m| m.raster.is_some()).count()
}

#[test]
fn completed_alpha_is_one_undo_step_and_survives_save_reload() {
    let (mut s, clip) = setup();
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    let before = s.history.undo.len();
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    assert_eq!(raster_count(&s, clip), 1);
    assert_eq!(s.history.undo.len(), before + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(raster_count(&s, clip), 0);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(raster_count(&s, clip), 1);
    let bytes = filmcraft_format::encode(&s.project, false);
    let saved = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(saved.project, s.project.as_ref().clone());
    s.execute("masks.ai.settings", json!({"mode":"fast"})).unwrap();
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    assert_eq!(raster_count(&s, clip), 1, "mode changes replace the previous AI mask");
    let mask = s.active_sequence().unwrap().find_item(clip).unwrap().1.effect("opacity").unwrap().masks[0].raster.as_ref().unwrap();
    assert!(!mask.quality);
}

#[test]
fn hostile_points_and_settings_do_not_mutate_project_or_draft() {
    let (mut s, _) = setup();
    let before = s.project.clone();
    for p in [json!({"x":-1,"y":4}), json!({"x":100000,"y":4}), json!({"x":"NaN","y":4}), json!({"x":4,"y":4,"kind":"unknown"})] {
        assert!(s.execute("masks.ai.point", p).is_err());
    }
    for p in [json!({"frames":0}), json!({"frames":121}), json!({"frames":-1}), json!({"band":33}), json!({"mode":"fake"})] {
        assert!(s.execute("masks.ai.settings", p).is_err());
    }
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert_eq!(s.project, before);
    assert!(s.execute("masks.ai.generate", json!({"wait":true})).is_err());
}

struct Invalid;
impl Backend for Invalid {
    fn infer(&self, _: &Input, _: &filmcraft_export::Progress) -> Result<RasterMask, String> {
        Ok(RasterMask { width: 0, height: 0, end: filmcraft_time::Tick(0), quality: false, frames: vec![] })
    }
}

#[test]
fn failed_inference_never_applies_a_partial_mask() {
    let (mut s, clip) = setup();
    s.ai_mask_backend = Some(Arc::new(Invalid));
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    let before = s.history.undo.len();
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    assert_eq!(raster_count(&s, clip), 0);
    assert_eq!(s.history.undo.len(), before);
    assert!(s.jobs.last().unwrap().result.lock().unwrap().as_ref().unwrap().is_err());
}

struct Cancelled;
impl Backend for Cancelled {
    fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> Result<RasterMask, String> {
        progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        Fake.infer(input, progress)
    }
}

#[test]
fn cancelling_a_completed_result_leaves_history_and_masks_untouched() {
    let (mut s, clip) = setup();
    s.ai_mask_backend = Some(Arc::new(Cancelled));
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(raster_count(&s, clip), 0);
}

#[test]
fn damaged_restored_draft_is_rejected_before_starting_a_job() {
    let (mut s, _) = setup();
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    s.state.ai_mask.as_mut().unwrap().frames = 0;
    assert!(s.execute("masks.ai.generate", json!({"wait":true})).is_err());
    s.state.ai_mask.as_mut().unwrap().frames = 3;
    s.state.ai_mask.as_mut().unwrap().points[0].x = f64::NAN;
    assert!(s.execute("masks.ai.generate", json!({"wait":true})).is_err());
    assert!(s.jobs.is_empty());
}

#[test]
fn first_frame_preview_is_transient_and_full_generation_replaces_it() {
    let (mut s, clip) = setup();
    // Isolate the clip from the demo's upper tracks and opening dip-to-black, so the visual
    // assertion tests the overlay rather than a transition or another clip above it.
    let seq_id = s.state.active_sequence.unwrap();
    for (index, track) in Arc::make_mut(&mut s.project).sequence_mut(seq_id).unwrap().video_tracks.iter_mut().enumerate() {
        track.enabled = index == 0;
        if index == 0 {
            track.items.retain(|it| it.id == clip);
            track.transitions.clear();
        }
    }
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    s.execute("masks.ai.preview", json!({"wait":true})).unwrap();
    assert_eq!(s.project, before, "a preview never alters the saved project");
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(raster_count(&s, clip), 0);
    let seq = s.state.active_sequence.unwrap();
    let (preview, revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert_ne!(revision, s.revision);
    let item = preview.sequence(seq).unwrap().find_item(clip).unwrap().1;
    assert!(item.effect("opacity").unwrap().masks.is_empty());
    let alpha = item.effects.last().unwrap().masks[0].raster.as_ref().unwrap();
    assert_eq!(alpha.frames.len(), 1);
    let opts = filmcraft_render::RenderOptions { scale: 0.1, ..Default::default() };
    let base_sources = s.media.provider(s.project.clone(), s.services.clone());
    let preview_sources = s.media.provider(preview.clone(), s.services.clone());
    let original = filmcraft_render::render_sequence(&s.project, seq, s.playhead(), opts, &base_sources);
    let tinted = filmcraft_render::render_sequence(&preview, seq, s.playhead(), opts, &preview_sources);
    assert!(original.px.iter().zip(&tinted.px).any(|(a, b)| (a - b).abs() > 0.05), "selection is visibly tinted");
    for (a, b) in original.px.chunks_exact(4).zip(tinted.px.chunks_exact(4)) {
        assert!((a[3] - b[3]).abs() < 0.00001, "preview retains the original frame alpha");
    }
    let untouched = original
        .px
        .chunks_exact(4)
        .zip(tinted.px.chunks_exact(4))
        .filter(|(a, b)| a[3] > 0.9 && a[..3].iter().zip(&b[..3]).all(|(x, y)| (x - y).abs() < 0.00001))
        .count();
    assert!(untouched > 0, "Include leaves the background untinted");
    let jobs = s.jobs.len();
    s.execute("masks.ai.select", json!({"kind":"background"})).unwrap();
    let (excluded, excluded_revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert_ne!(revision, excluded_revision, "Exclude invalidates the displayed frame cache");
    let excluded_sources = s.media.provider(excluded.clone(), s.services.clone());
    let excluded_image = filmcraft_render::render_sequence(&excluded, seq, s.playhead(), opts, &excluded_sources);
    assert!(
        original.px.chunks_exact(4).zip(tinted.px.chunks_exact(4)).zip(excluded_image.px.chunks_exact(4)).any(|((a, green), red)| {
            a[3] > 0.9
                && a[..3].iter().zip(&green[..3]).all(|(x, y)| (x - y).abs() < 0.00001)
                && red[..3].iter().zip(&green[..3]).any(|(x, y)| (x - y).abs() > 0.05)
        }),
        "the red background overlay appears only after selecting Exclude"
    );
    s.execute("masks.ai.select", json!({"kind":"foreground"})).unwrap();
    let (included_again, included_revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert_eq!(included_revision, revision);
    assert!(Arc::ptr_eq(&included_again, &preview), "switching back immediately restores the green-only preview");
    assert_eq!(s.jobs.len(), jobs, "display mode changes reuse the same alpha");
    assert_eq!(s.project, before);
    assert_eq!(s.history.undo.len(), undo);
    assert!(crate::ai_masks::monitor_preview(&s, seq, s.playhead() + filmcraft_time::Tick::from_seconds_f64(1.0)).is_none());
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    assert!(s.ai_mask_preview.is_none());
    assert_eq!(raster_count(&s, clip), 1);
}

#[test]
fn include_click_automatically_previews_and_clear_discards_it() {
    use std::time::Duration;
    let (mut s, _) = setup();
    s.execute("masks.ai.point", json!({"x":400,"y":300})).unwrap();
    let started = std::time::Instant::now();
    while !s.jobs.last().unwrap().progress.finished.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    crate::ai_masks::poll(&mut s);
    assert!(s.ai_mask_preview.is_some());
    s.execute("masks.ai.select", json!({"clear":true})).unwrap();
    assert!(s.ai_mask_preview.is_none());
}

#[test]
fn removing_a_point_refreshes_preview_and_preserves_other_frames_and_project() {
    use std::time::{Duration, Instant};
    let (mut s, _) = setup();
    for mut point in [json!({"x":400,"y":300}), json!({"x":600,"y":200,"kind":"background"}), json!({"x":420,"y":300,"kind":"refine"})] {
        point["preview"] = json!(false);
        s.execute("masks.ai.point", point).unwrap();
    }
    let start = s.playhead();
    s.set_playhead(start + filmcraft_time::Tick::from_seconds_f64(0.1));
    s.execute("masks.ai.point", json!({"x":400,"y":350,"kind":"foreground","preview":false})).unwrap();
    s.set_playhead(start);
    let before = s.project.clone();
    let revision = s.revision;
    let undo = s.history.undo.len();
    let mut expected = s.state.ai_mask.as_ref().unwrap().points.iter().map(|p| (p.kind, p.time, p.x, p.y)).collect::<Vec<_>>();
    expected.remove(1);
    s.execute("masks.ai.preview", json!({"wait":true})).unwrap();
    let previous = s.ai_mask_preview.as_ref().unwrap().job;
    assert_eq!(s.execute("masks.ai.removePoint", json!({"point":1})).unwrap()["points"], 3);
    let progress = s.jobs.last().unwrap().progress.clone();
    let started = Instant::now();
    while !progress.finished.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    crate::ai_masks::poll(&mut s);
    assert_ne!(s.ai_mask_preview.as_ref().unwrap().job, previous);
    assert_eq!(s.state.ai_mask.as_ref().unwrap().points.iter().map(|p| (p.kind, p.time, p.x, p.y)).collect::<Vec<_>>(), expected);
    let jobs = s.jobs.len();
    s.execute("masks.ai.removePoint", json!({"point":0})).unwrap();
    assert!(s.ai_mask_preview.is_none(), "removing the current frame's last Include point restores the original frame");
    assert_eq!(s.jobs.len(), jobs, "an Include point on another frame cannot preview this frame");
    assert_eq!(s.state.ai_mask.as_ref().unwrap().points.len(), 2);
    assert_eq!(s.project, before);
    assert_eq!(s.revision, revision);
    assert_eq!(s.history.undo.len(), undo + 2, "both deletions are undoable transient edits");
}

#[test]
fn clear_and_remove_points_undo_redo_without_dirtying_the_project() {
    let (mut s, _) = setup();
    s.ai_mask_backend = None;
    s.execute("masks.ai.point", json!({"x":400,"y":300})).unwrap();
    s.execute("masks.ai.point", json!({"x":700,"y":400,"kind":"background"})).unwrap();
    let (project, revision, dirty) = (s.project.clone(), s.revision, s.is_dirty());
    let original = s.state.ai_mask.clone();
    s.execute("masks.ai.removePoint", json!({"point":0})).unwrap();
    let removed = s.state.ai_mask.clone();
    s.execute("masks.ai.select", json!({"clear":true})).unwrap();
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.state.ai_mask, removed);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.state.ai_mask, original);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.state.ai_mask, removed);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert!(Arc::ptr_eq(&s.project, &project));
    assert_eq!((s.revision, s.is_dirty()), (revision, dirty));
}

#[test]
fn transient_history_preserves_out_of_history_project_renames() {
    let (mut s, _) = setup();
    s.ai_mask_backend = None;
    let mark = s.undo_mark();
    s.execute("masks.ai.point", json!({"x":400,"y":300})).unwrap();
    s.saved_revision = s.revision;
    let revision = s.revision;
    // Save As updates the project name this way without creating a history step.
    Arc::make_mut(&mut s.project).name = "Saved As".into();
    s.undo().unwrap();
    assert_eq!(s.project.name, "Saved As");
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert_eq!(s.revision, revision);
    assert!(!s.is_dirty());
    Arc::make_mut(&mut s.project).name = "Renamed again".into();
    s.redo().unwrap();
    assert_eq!(s.project.name, "Renamed again");
    assert_eq!(s.state.ai_mask.as_ref().unwrap().points.len(), 1);
    assert_eq!(s.revert_to(mark), 1);
    assert_eq!(s.project.name, "Renamed again");
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert_eq!(s.revision, revision);
    assert!(!s.is_dirty());
}

#[test]
fn reverting_mixed_history_uses_the_oldest_document_snapshot() {
    let (mut s, _) = setup();
    s.ai_mask_backend = None;
    let mark = s.undo_mark();
    s.execute("masks.ai.point", json!({"x":400,"y":300})).unwrap();
    Arc::make_mut(&mut s.project).name = "Saved As".into();
    s.edit("Document edit", |project, _| {
        project.name = "Edited".into();
        Ok(())
    })
    .unwrap();
    s.execute("masks.ai.point", json!({"x":500,"y":300})).unwrap();
    assert_eq!(s.revert_to(mark), 3);
    assert_eq!(s.project.name, "Saved As", "the oldest transient step must not replace the document snapshot");
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
}

#[test]
fn point_edits_share_chronological_history_with_document_edits_and_brush_gestures() {
    let (mut s, _) = setup();
    s.ai_mask_backend = None;
    let (project, mark) = (s.project.clone(), s.undo_mark());
    s.execute("masks.ai.point", json!({"x":400,"y":300})).unwrap();
    s.edit("Change project name", |project, _| {
        project.name = "Changed".into();
        Ok(())
    })
    .unwrap();
    let undo = s.history.undo.len();
    for x in [400, 402, 405] {
        s.execute("masks.ai.point", json!({"x":x,"y":300,"kind":"refine","stroke":42})).unwrap();
    }
    assert_eq!(s.history.undo.len(), undo + 1, "one brush gesture is one undo step");
    let revision = s.revision;
    assert_eq!(s.undo().as_deref(), Some("Refine AI edge"));
    assert_eq!(s.state.ai_mask.as_ref().unwrap().points.len(), 1);
    assert_eq!(s.revision, revision);
    assert_eq!(s.undo().as_deref(), Some("Change project name"));
    assert_eq!(s.project, project);
    assert_eq!(s.undo().as_deref(), Some("Add AI Include point"));
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert_eq!(s.undo_mark(), mark);
    assert!(s.redo().is_some());
    s.execute("masks.ai.point", json!({"x":500,"y":300})).unwrap();
    assert!(!s.history.can_redo(), "a new draft edit branches history");
    assert_eq!(s.revert_to(mark), 2, "gesture cancellation also restores transient drafts");
    assert!(s.state.ai_mask.as_ref().unwrap().points.is_empty());
    assert_eq!(s.project, project);
}

#[test]
fn refine_views_show_actual_soft_alpha_and_switch_without_inference() {
    use crate::ai_masks::Pick;
    let (mut s, clip) = setup();
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    s.execute("masks.ai.preview", json!({"wait":true})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let (_, green_revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    let jobs = s.jobs.len();
    s.execute("masks.ai.select", json!({"kind":"refine"})).unwrap();
    let (refined, refine_revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert_ne!(green_revision, refine_revision);
    assert_eq!(s.jobs.len(), jobs, "display changes never invoke inference");
    let item = refined.sequence(seq).unwrap().find_item(clip).unwrap().1;
    let white = item.effects.last().unwrap();
    assert_eq!(white.param("white").unwrap().value, filmcraft_project::ParamValue::Color([1.0; 4]));
    let alpha = white.masks[0].raster.as_ref().unwrap().alpha_at(item.source_time_at(s.playhead())).unwrap();
    assert!(alpha.contains(&(32768.0 / 65535.0)) && alpha.contains(&0.0), "the region shows inferred fractional alpha, never an opaque brush fill");
    s.execute("masks.ai.settings", json!({"view":"alpha"})).unwrap();
    let (alpha_project, alpha_revision) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert_ne!(alpha_revision, refine_revision);
    let alpha_item = alpha_project.sequence(seq).unwrap().find_item(clip).unwrap().1;
    let full_alpha = alpha_item.effects.last().unwrap().masks[0].raster.as_ref().unwrap().alpha_at(item.source_time_at(s.playhead())).unwrap();
    assert_eq!(*full_alpha.last().unwrap(), 32768.0 / 65535.0);
    assert_eq!(full_alpha.iter().filter(|value| **value > 0.0).count(), full_alpha.len() - full_alpha.len() / 2);
    s.execute("masks.ai.settings", json!({"view":"composite"})).unwrap();
    let (composite, _) = crate::ai_masks::monitor_preview(&s, seq, s.playhead()).unwrap();
    assert!(composite.sequence(seq).unwrap().find_item(clip).unwrap().1.effect("opacity").unwrap().masks.last().unwrap().raster.is_some());
    assert_eq!(s.jobs.len(), jobs);
    assert_eq!(s.state.ai_mask.as_ref().unwrap().pick, Pick::Refine);
    assert_eq!(raster_count(&s, clip), 0);
}

#[test]
fn refine_clicks_automatically_use_quality_and_can_refine_propagated_frames() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Capture {
        quality: Arc<AtomicBool>,
        base: Arc<AtomicBool>,
    }
    impl Backend for Capture {
        fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> Result<RasterMask, String> {
            self.quality.store(input.quality, Ordering::Relaxed);
            self.base.store(input.base_alpha.is_some(), Ordering::Relaxed);
            Fake.infer(input, progress)
        }
    }
    fn finish(s: &mut Session) {
        let progress = s.jobs.last().unwrap().progress.clone();
        let start = std::time::Instant::now();
        while !progress.finished.load(Ordering::Relaxed) {
            assert!(start.elapsed().as_secs() < 10);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        crate::ai_masks::poll(s);
    }
    let (mut s, _) = setup();
    let quality = Arc::new(AtomicBool::new(false));
    let base = Arc::new(AtomicBool::new(false));
    s.ai_mask_backend = Some(Arc::new(Capture { quality: quality.clone(), base: base.clone() }));
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    s.execute("masks.ai.settings", json!({"mode":"fast"})).unwrap();
    s.execute("masks.ai.select", json!({"kind":"refine"})).unwrap();
    finish(&mut s);
    assert!(quality.load(Ordering::Relaxed), "entering Refine runs the matting model");
    let jobs = s.jobs.len();
    s.execute("masks.ai.point", json!({"x":401,"y":301,"kind":"refine"})).unwrap();
    assert_eq!(s.jobs.len(), jobs + 1, "a completed brush click previews automatically");
    finish(&mut s);
    s.execute("masks.ai.generate", json!({"wait":true})).unwrap();
    s.execute("playhead.step", json!({"frames":1})).unwrap();
    s.execute("masks.ai.select", json!({"kind":"refine"})).unwrap();
    finish(&mut s);
    assert!(base.load(Ordering::Relaxed), "a propagated frame uses its saved matte without a new Include point");
    let jobs = s.jobs.len();
    s.execute("masks.ai.point", json!({"x":401,"y":301,"kind":"refine","stroke":12,"preview":false})).unwrap();
    assert_eq!(s.jobs.len(), jobs, "pointer movement defers matting until release");
    s.execute("masks.ai.preview", json!({"wait":true})).unwrap();
    assert!(s.jobs.last().unwrap().result.lock().unwrap().as_ref().unwrap().is_ok());
}

#[test]
fn invalid_point_removal_leaves_draft_preview_and_document_untouched() {
    let (mut s, _) = setup();
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    s.execute("masks.ai.preview", json!({"wait":true})).unwrap();
    let before = s.project.clone();
    let points = serde_json::to_value(&s.state.ai_mask.as_ref().unwrap().points).unwrap();
    let job = s.ai_mask_preview.as_ref().unwrap().job;
    let jobs = s.jobs.len();
    for params in [json!({}), json!({"point":-1}), json!({"point":1}), json!({"point":u64::MAX}), json!({"point":0.5}), json!({"point":"0"})] {
        assert!(s.execute("masks.ai.removePoint", params).is_err());
        assert_eq!(serde_json::to_value(&s.state.ai_mask.as_ref().unwrap().points).unwrap(), points);
        assert_eq!(s.ai_mask_preview.as_ref().unwrap().job, job);
        assert_eq!(s.jobs.len(), jobs);
        assert_eq!(s.project, before);
    }
}

#[test]
fn invalid_refine_view_and_stroke_parameters_leave_the_draft_unchanged() {
    let (mut s, _) = setup();
    let before = s.state.ai_mask.clone();
    let history = s.history.undo.len();
    for view in [json!(null), json!(23), json!("invalid"), json!(["alpha"])] {
        assert!(s.execute("masks.ai.settings", json!({"view":view})).is_err());
    }
    for stroke in [json!(-1), json!(0.5), json!("123"), json!(null)] {
        assert!(s.execute("masks.ai.point", json!({"x":400,"y":300,"kind":"refine","stroke":stroke})).is_err());
    }
    assert_eq!(s.state.ai_mask, before);
    assert_eq!(s.history.undo.len(), history);
    assert!(s.jobs.is_empty());
}

#[test]
fn editing_a_clip_during_inference_rejects_the_stale_result() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    struct Paused {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Backend for Paused {
        fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> Result<RasterMask, String> {
            self.entered.send(()).map_err(|e| e.to_string())?;
            self.release.lock().unwrap().recv_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
            Fake.infer(input, progress)
        }
    }
    let (mut s, clip) = setup();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    s.ai_mask_backend = Some(Arc::new(Paused { entered: entered_tx, release: Mutex::new(release_rx) }));
    s.execute("masks.ai.point", json!({"x":400,"y":300,"preview":false})).unwrap();
    s.execute("masks.ai.generate", json!({})).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(s.execute("masks.ai.removePoint", json!({"point":0})).is_err(), "full generation must keep its input points stable");
    assert_eq!(s.state.ai_mask.as_ref().unwrap().points.len(), 1);
    s.execute("masks.add", json!({"shape":"rectangle"})).unwrap();
    let before = s.project.clone();
    release_tx.send(()).unwrap();
    let started = std::time::Instant::now();
    while !s.jobs.last().unwrap().progress.finished.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    crate::ai_masks::poll(&mut s);
    assert_eq!(s.project, before);
    assert_eq!(raster_count(&s, clip), 0);
    assert!(s.jobs.last().unwrap().result.lock().unwrap().as_ref().unwrap().as_ref().unwrap_err().contains("clip changed"));
}
