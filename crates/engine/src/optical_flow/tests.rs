use super::*;

/// A demo project with the first V1 clip cut down to `frames` frames; returns the clip, its start
/// and its duration.
fn demo(frames: i64) -> (Session, ItemId, ClipId, Tick, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let seq_id = s.state.active_sequence.unwrap();
    let (clip, start, dur) = {
        let seq = s.project.sequence(seq_id).unwrap();
        let fd = seq.settings.frame_rate.frame_duration();
        let it = seq.video_tracks[0].items.first().unwrap();
        (it.id, it.start, Tick(fd.0 * frames))
    };
    s.execute("timeline.razor", json!({"clip": clip.0, "time": (start + dur).0})).unwrap();
    (s, seq_id, clip, start, dur)
}

fn overlay_of(s: &Session, seq_id: ItemId, clip: ClipId) -> (usize, filmcraft_project::TrackItem) {
    let seq = s.project.sequence(seq_id).unwrap();
    seq.video_tracks.iter().enumerate().find_map(|(ti, tr)| tr.item(clip).map(|it| (ti, it.clone()))).expect("overlay clip placed on a video track")
}

#[test]
fn overlay_is_aligned_above_the_clip_and_undoes_in_one_step() {
    let (mut s, seq_id, v1_clip, start, dur) = demo(4);
    let undo_before = s.history.undo.len();
    let dir = tempfile::tempdir().unwrap();
    let res = s
        .execute(
            "clip.opticalFlowOverlay",
            json!({"clips": [v1_clip.0], "mode": "hsvWheel", "maxMotion": 12.0, "dir": dir.path().to_string_lossy(), "wait": true}),
        )
        .unwrap();

    assert_eq!(res["mode"], "hsvWheel");
    assert_eq!(res["cpuFrames"], 4, "{res}");
    // no GPU overlay renderer is registered in engine tests: every frame is drawn on the CPU
    assert_eq!((res["overlayBackend"].as_str(), res["overlayCpuFrames"].as_u64(), res["overlayGpuFrames"].as_u64()), (Some("CPU"), Some(4), Some(0)), "{res}");
    let new_clip = ClipId(res["clips"][0].as_u64().unwrap());
    assert_eq!(s.history.undo.len(), undo_before + 1, "must collapse into one undo step");
    let path = res["paths"][0].as_str().unwrap().to_string();
    assert!(std::fs::metadata(&path).unwrap().len() > 0, "{path}");

    let (ti, it) = overlay_of(&s, seq_id, new_clip);
    assert!(ti > 0, "overlay clip must be placed on a track above V1");
    assert_eq!((it.start, it.duration), (start, dur));
    let op = it.effects.iter().find(|e| e.effect == "opacity").unwrap();
    assert_eq!(op.params["blend"].value, ParamValue::Choice(Blend::Screen.index()));

    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.sequence(seq_id).unwrap().find_item(new_clip).is_none());
}

#[test]
fn several_clips_are_placed_together_in_one_step_with_their_own_files() {
    let (mut s, seq_id, a, _, _) = demo(2);
    // the second piece of the razored clip, cut to two frames as well
    let (b, b_start) = {
        let seq = s.project.sequence(seq_id).unwrap();
        let it = seq.video_tracks[0].items.iter().find(|it| it.id != a).unwrap();
        (it.id, it.start)
    };
    let fd = s.project.sequence(seq_id).unwrap().settings.frame_rate.frame_duration();
    s.execute("timeline.razor", json!({"clip": b.0, "time": (b_start + Tick(fd.0 * 2)).0})).unwrap();
    let undo_before = s.history.undo.len();
    let dir = tempfile::tempdir().unwrap();
    let res = s.execute("clip.opticalFlowOverlay.motionVectors", json!({"clips": [a.0, b.0], "dir": dir.path().to_string_lossy(), "wait": true})).unwrap();
    let clips = res["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2);
    let paths: HashSet<&str> = res["paths"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(paths.len(), 2, "each clip gets its own file: {res}");
    assert_eq!(s.history.undo.len(), undo_before + 1);
    for c in clips {
        let (_, it) = overlay_of(&s, seq_id, ClipId(c.as_u64().unwrap()));
        let op = it.effects.iter().find(|e| e.effect == "opacity").unwrap();
        assert_eq!(op.params["blend"].value, ParamValue::Choice(Blend::Normal.index()));
    }
}

#[test]
fn a_selection_past_the_frame_cap_is_refused_before_anything_happens() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    // a project can store any duration: make the clip far longer than the cap allows
    let fd = s.project.sequence(seq_id).unwrap().settings.frame_rate.frame_duration();
    let pr = std::sync::Arc::make_mut(&mut s.project);
    let q = pr.sequence_mut(seq_id).unwrap();
    for t in q.video_tracks.iter_mut() {
        for it in t.items.iter_mut().filter(|it| it.id == clip) {
            it.duration = Tick(fd.0.saturating_mul(MAX_OVERLAY_FRAMES + 1));
        }
    }
    let undo_before = s.history.undo.len();
    let dir = tempfile::tempdir().unwrap();
    let r = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy(), "wait": true}));
    assert!(r.as_ref().is_err_and(|e| e.to_string().contains("longer than")), "{r:?}");
    assert_eq!(s.history.undo.len(), undo_before);
    assert!(s.jobs.is_empty() && s.flow_jobs.is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no file is written");
}

#[test]
fn hostile_parameters_are_errors_and_change_nothing() {
    let (mut s, _, clip, _, _) = demo(2);
    // with a selection, a malformed target must not fall back to rendering the selection
    s.state.selection = vec![clip];
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_string_lossy().into_owned();
    let undo_before = s.history.undo.len();
    for p in [
        json!({"clips": [clip.0], "maxMotion": 0.1}),
        json!({"clips": [clip.0], "maxMotion": 1e300}),
        json!({"clips": [clip.0], "maxMotion": -16}),
        json!({"clips": [clip.0], "opacity": -1}),
        json!({"clips": [clip.0], "opacity": 100.5}),
        json!({"clips": [clip.0], "mode": "sparkles"}),
        json!({"clips": [clip.0], "blendMode": "no such blend"}),
        json!({"clips": [u64::MAX]}),
        json!({"clips": []}),
        json!({"clips": "42"}),
        json!({"clips": [clip.0, "x"]}),
        json!({"clip": "42"}),
        json!({"clip": clip.0, "clips": [clip.0]}),
    ] {
        let mut p = p;
        p["dir"] = json!(d);
        p["wait"] = json!(true);
        assert!(s.execute("clip.opticalFlowOverlay", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.history.undo.len(), undo_before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// A rendered file for `clip`, made by running the command and undoing it (the file stays).
fn rendered(s: &mut Session, seq_id: ItemId, clip: ClipId, dir: &std::path::Path) -> Rendered {
    let res = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.to_string_lossy(), "wait": true})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let seq = s.project.sequence(seq_id).unwrap();
    let key = seq.video_tracks.iter().find_map(|t| t.item(clip).map(|it| source_key(t, it))).unwrap();
    Rendered { clip, key, path: res["paths"][0].as_str().unwrap().to_string(), bytes: None }
}

fn pending(s: &Session, seq_id: ItemId, clips: Vec<Rendered>) -> PendingFlow {
    let look = Look { mode: FlowVizMode::HsvWheel, max_motion: 16.0, opacity: 85.0, blend: Blend::Screen.index() };
    let outcome = Outcome { clips, cpu_frames: 4, ..Default::default() };
    let seq = s.project.sequence(seq_id).unwrap();
    PendingFlow {
        job: u64::MAX,
        seq: seq_id,
        look,
        epoch: s.document_epoch,
        rate: seq.settings.frame_rate,
        size: filmcraft_render::output_size(seq, 1.0),
        color: seq.settings.color,
        results: Arc::new(Mutex::new(Some(outcome))),
        hw_name: Arc::new(Mutex::new("hardware".into())),
    }
}

/// A job record as a running (or finished) worker leaves it.
fn job_record(s: &mut Session, id: u64, finished: bool) {
    let job = crate::Job { id, label: LABEL.into(), progress: Default::default(), result: Default::default() };
    job.progress.finished.store(finished, Ordering::Relaxed);
    if finished {
        *lock(&job.result) = Some(Ok(filmcraft_export::Report::default()));
    }
    s.jobs.push(job);
}

/// The worker's context for `mode` on the session's current project.
fn job_ctx(s: &Session, seq_id: ItemId, mode: FlowVizMode) -> JobCtx {
    let seq = s.project.sequence(seq_id).unwrap();
    JobCtx {
        project: s.project.clone(),
        seq: seq_id,
        size: filmcraft_render::output_size(seq, 1.0),
        rate: seq.settings.frame_rate,
        look: Look { mode, max_motion: 16.0, opacity: 100.0, blend: Blend::Normal.index() },
        provider: s.media.full_res_provider(s.project.clone(), s.services.clone()).own_sources(),
        services: s.services.clone(),
        in_memory: false,
    }
}

#[test]
fn a_clip_that_cannot_be_placed_takes_back_the_whole_job_and_its_files() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let good = rendered(&mut s, seq_id, clip, dir.path());
    assert!(std::path::Path::new(&good.path).exists());
    let missing = Rendered { path: dir.path().join("missing.mov").to_string_lossy().into_owned(), ..good.clone() };
    let (project, undo, redo) = (s.project.clone(), s.history.undo.len(), s.history.redo.len());
    // a saved project with a gesture in progress
    s.saved_revision = s.revision;
    s.history.merge_key = Some("fader".into());

    let pj = pending(&s, seq_id, vec![good.clone(), missing]);
    let r = apply(&mut s, pj, true);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(*s.project, *project, "the first clip's overlay was taken back");
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
    assert!(!s.is_dirty(), "a placement that did not happen leaves a saved project saved");
    assert_eq!(s.history.merge_key.as_deref(), Some("fader"));
    assert!(!std::path::Path::new(&good.path).exists(), "generated files are deleted");
    // the first clip was imported under the next id before the second failed: its source is not
    // left cached for an item the rollback took away
    let first_import = ItemId(project.next_id);
    assert!(s.project.item(first_import).is_none());
    assert!(s.media.cached(first_import).is_none(), "the rolled-back import is evicted from the media pool");
}

#[test]
fn a_clip_moved_while_rendering_changes_nothing() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let mut r = rendered(&mut s, seq_id, clip, dir.path());
    r.key.start += Tick(1);
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    let pj = pending(&s, seq_id, vec![r]);
    let e = apply(&mut s, pj, true).unwrap_err();
    assert!(e.to_string().contains("moved"), "{e}");
    assert_eq!(*s.project, *project);
    assert_eq!(s.history.undo.len(), undo);
}

/// Run [`poll`] until the job is applied (or a minute passes).
fn poll_until_done(s: &mut Session) {
    let t0 = std::time::Instant::now();
    while !s.flow_jobs.is_empty() && t0.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        poll(s);
    }
    assert!(s.flow_jobs.is_empty(), "the job finished");
}

#[test]
fn the_menu_path_renders_in_the_background_and_places_the_clip_when_done() {
    let (mut s, seq_id, clip, start, dur) = demo(3);
    s.state.selection = vec![clip];
    let dir = tempfile::tempdir().unwrap();
    let undo_before = s.history.undo.len();
    let before: HashSet<ClipId> = s.project.sequence(seq_id).unwrap().video_tracks.iter().flat_map(|t| t.items.iter().map(|it| it.id)).collect();
    let res = s.execute("clip.opticalFlowOverlay.kineticGlow", json!({"dir": dir.path().to_string_lossy()})).unwrap();
    assert!(res["job"].as_u64().is_some(), "{res}");
    assert_eq!(res["frames"], 3);
    // the user selects another clip while the track renders: completion must not take it away
    let other = s.project.sequence(seq_id).unwrap().video_tracks[0].items.iter().find(|it| it.id != clip).unwrap().id;
    s.state.selection = vec![other];
    poll_until_done(&mut s);
    assert_eq!(s.history.undo.len(), undo_before + 1);
    assert_eq!(s.state.selection, vec![other], "a background completion leaves the selection alone");
    let seq = s.project.sequence(seq_id).unwrap();
    let placed: Vec<(usize, &filmcraft_project::TrackItem)> =
        seq.video_tracks.iter().enumerate().flat_map(|(ti, t)| t.items.iter().map(move |it| (ti, it))).filter(|(_, it)| !before.contains(&it.id)).collect();
    assert_eq!(placed.len(), 1);
    assert!(placed[0].0 > 0);
    assert_eq!((placed[0].1.start, placed[0].1.duration), (start, dur));
}

#[test]
fn a_second_job_for_the_sequence_is_refused_while_one_runs() {
    let (mut s, seq_id, clip, _, _) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    // a job of this document that has not finished (the worker is still busy)
    job_record(&mut s, 500, false);
    let pj = PendingFlow { job: 500, ..pending(&s, seq_id, Vec::new()) };
    s.flow_jobs.push(pj);
    let r = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy(), "wait": true}));
    assert!(r.as_ref().is_err_and(|e| e.to_string().contains("already being generated")), "{r:?}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_job_left_from_a_replaced_document_does_not_block_the_sequence() {
    let (mut s, seq_id, clip, _, _) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    job_record(&mut s, 500, false);
    let stale = pending(&s, seq_id, Vec::new());
    s.flow_jobs.push(PendingFlow { job: 500, epoch: stale.epoch.wrapping_sub(1), ..stale });
    let r = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy(), "wait": true}));
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn a_placement_that_fails_is_reported_by_the_job() {
    let (mut s, seq_id, clip, _, _) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    let mut r = rendered(&mut s, seq_id, clip, dir.path());
    r.key.start += Tick(1);
    job_record(&mut s, 600, true);
    let pj = PendingFlow { job: 600, ..pending(&s, seq_id, vec![r]) };
    s.flow_jobs.push(pj);
    poll(&mut s);
    let job = s.jobs.iter().find(|j| j.id == 600).unwrap();
    let result = lock(&job.result).clone();
    assert!(result.as_ref().is_some_and(|r| r.as_ref().is_err_and(|e| e.contains("moved"))), "{result:?}");
    assert!(lock(&job.progress.error).is_some());
}

#[test]
fn one_undo_takes_back_the_whole_job_even_with_a_full_history() {
    let (mut s, seq_id, a, _, _) = demo(2);
    let (b, b_start) = {
        let seq = s.project.sequence(seq_id).unwrap();
        let it = seq.video_tracks[0].items.iter().find(|it| it.id != a).unwrap();
        (it.id, it.start)
    };
    let fd = s.project.sequence(seq_id).unwrap().settings.frame_rate.frame_duration();
    s.execute("timeline.razor", json!({"clip": b.0, "time": (b_start + Tick(fd.0 * 2)).0})).unwrap();
    // the history is full: every further step pushes the oldest one out
    assert!(!s.history.undo.is_empty());
    s.history.limit = s.history.undo.len();
    let before = s.project.clone();
    let dir = tempfile::tempdir().unwrap();
    s.execute("clip.opticalFlowOverlay", json!({"clips": [a.0, b.0], "dir": dir.path().to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(s.history.undo.len(), s.history.limit);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo restores the project from before the job");
}

#[test]
fn a_clip_on_another_track_or_slipped_while_rendering_is_refused() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let r = rendered(&mut s, seq_id, clip, dir.path());
    let original = s.project.clone();
    // the same timing on a new track of its own
    let pr = Arc::make_mut(&mut s.project);
    let track = TrackId(pr.alloc_id());
    let q = pr.sequence_mut(seq_id).unwrap();
    let pos = q.video_tracks[0].items.iter().position(|it| it.id == clip).unwrap();
    let it = q.video_tracks[0].items.remove(pos);
    let mut t = Track::new(track, TrackKind::Video, "Moved".into());
    t.items.push(it);
    q.video_tracks.push(t);
    let moved = s.project.clone();
    let e = {
        let pj = pending(&s, seq_id, vec![r.clone()]);
        apply(&mut s, pj, true)
    }
    .unwrap_err();
    assert!(e.to_string().contains("moved"), "{e}");
    assert_eq!(*s.project, *moved);
    // slipped: the same place on the timeline, other source frames
    s.project = original;
    let fd = s.project.sequence(seq_id).unwrap().settings.frame_rate.frame_duration();
    let pr = Arc::make_mut(&mut s.project);
    pr.sequence_mut(seq_id).unwrap().find_item_mut(clip).unwrap().1.source_in += fd;
    let e = {
        let pj = pending(&s, seq_id, vec![r]);
        apply(&mut s, pj, true)
    }
    .unwrap_err();
    assert!(e.to_string().contains("slipped"), "{e}");
}

#[test]
fn a_track_rendered_before_the_frame_rate_size_or_colour_changed_is_refused() {
    for change in [0, 1, 2] {
        let (mut s, seq_id, clip, _, _) = demo(4);
        let dir = tempfile::tempdir().unwrap();
        let r = rendered(&mut s, seq_id, clip, dir.path());
        let pj = pending(&s, seq_id, vec![r]);
        let pr = Arc::make_mut(&mut s.project);
        let q = pr.sequence_mut(seq_id).unwrap();
        if change == 0 {
            q.settings.frame_rate = FrameRate { num: q.settings.frame_rate.num * 2, den: q.settings.frame_rate.den };
        } else if change == 1 {
            q.settings.width += 16;
        } else {
            q.settings.color.working = filmcraft_color::WorkingSpace::Rec2100Pq;
        }
        let project = s.project.clone();
        let e = apply(&mut s, pj, true).unwrap_err();
        assert!(e.to_string().contains("frame rate, frame size or colour settings"), "{e}");
        assert_eq!(*s.project, *project);
    }
}

#[test]
fn the_overlay_goes_on_an_enabled_track() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let r = rendered(&mut s, seq_id, clip, dir.path());
    // every track above V1 is switched off, and one of them is free
    let pr = Arc::make_mut(&mut s.project);
    let free = TrackId(pr.alloc_id());
    let q = pr.sequence_mut(seq_id).unwrap();
    q.video_tracks.push(Track::new(free, TrackKind::Video, "Off".into()));
    for t in q.video_tracks.iter_mut().skip(1) {
        t.enabled = false;
    }
    let v = {
        let pj = pending(&s, seq_id, vec![r]);
        apply(&mut s, pj, true)
    }
    .unwrap();
    let placed = ClipId(v["clips"][0].as_u64().unwrap());
    let seq = s.project.sequence(seq_id).unwrap();
    let track = seq.video_tracks.iter().find(|t| t.item(placed).is_some()).unwrap();
    assert!(track.enabled, "the overlay is on a track the program shows");
}

#[test]
fn the_overlay_does_not_add_a_track_past_the_limit() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let r = rendered(&mut s, seq_id, clip, dir.path());
    // every track above V1 is locked, and the sequence is at the track limit
    let pr = Arc::make_mut(&mut s.project);
    let have = pr.sequence(seq_id).unwrap().video_tracks.len();
    let ids: Vec<TrackId> = (have..crate::sequence_tools::MAX_TRACKS).map(|_| TrackId(pr.alloc_id())).collect();
    let q = pr.sequence_mut(seq_id).unwrap();
    q.video_tracks.extend(ids.into_iter().map(|id| Track::new(id, TrackKind::Video, "Full".into())));
    for t in q.video_tracks.iter_mut().skip(1) {
        t.locked = true;
    }
    let project = s.project.clone();
    let e = {
        let pj = pending(&s, seq_id, vec![r]);
        apply(&mut s, pj, true)
    }
    .unwrap_err();
    assert!(e.to_string().contains("video tracks"), "{e}");
    assert_eq!(*s.project, *project);
    assert_eq!(s.project.sequence(seq_id).unwrap().video_tracks.len(), crate::sequence_tools::MAX_TRACKS);
}

#[test]
fn temporal_detail_renders_working_light_and_the_other_looks_the_display_picture() {
    let (mut s, seq_id, _, start, _) = demo(2);
    s.execute("sequence.colorSettings", json!({"workingSpace": "Rec. 2100 PQ"})).unwrap();
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone()).own_sources();
    let working = render_sequence_track(&s.project, seq_id, 0, start, RenderOptions { working_output: true, ..RenderOptions::default() }, &provider);
    let display = render_sequence_track(&s.project, seq_id, 0, start, RenderOptions::default(), &provider);
    assert_ne!(working.px, display.px, "this sequence's display transform is not the identity");
    assert_eq!(job_ctx(&s, seq_id, FlowVizMode::TemporalDetail).render(0, start).unwrap().px, working.px, "Temporal Detail encodes working light itself");
    for mode in [FlowVizMode::HsvWheel, FlowVizMode::MotionVectors, FlowVizMode::KineticGlow] {
        assert_eq!(job_ctx(&s, seq_id, mode).render(0, start).unwrap().px, display.px, "{mode:?} draws on the display picture");
    }
}

#[test]
fn a_file_another_job_created_is_left_alone() {
    let (s, seq_id, clip, _, _) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    // another job won the race for this name
    let path = dir.path().join("Clip (HSV Neon Flow Wheel) 1.mov");
    std::fs::write(&path, b"another job's output").unwrap();
    let seq = s.project.sequence(seq_id).unwrap();
    let key = source_key(&seq.video_tracks[0], seq.video_tracks[0].item(clip).unwrap());
    let work = Work { clip, track_index: 0, key, frames: 2, path: path.to_string_lossy().into_owned() };
    let prog = filmcraft_export::Progress::default();
    let r = job_ctx(&s, seq_id, FlowVizMode::HsvWheel).render_all(&[work], &prog, &Mutex::new(String::new()));
    assert!(r.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"another job's output");
}

#[test]
fn a_stopped_job_changes_nothing_and_deletes_its_file() {
    let (mut s, _, clip, _, _) = demo(6);
    let dir = tempfile::tempdir().unwrap();
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    let res = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy()})).unwrap();
    let id = res["job"].as_u64().unwrap();
    s.jobs.iter().find(|j| j.id == id).unwrap().progress.cancel.store(true, Ordering::Relaxed);
    poll_until_done(&mut s);
    assert_eq!(*s.project, *project);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "the stopped job's file is deleted");
}

#[test]
fn frame_counts_round_up_and_reject_empty_clips() {
    assert_eq!(frames_for(Tick(10), Tick(5)), Some(2));
    assert_eq!(frames_for(Tick(11), Tick(5)), Some(3));
    assert_eq!(frames_for(Tick(i64::MAX), Tick(1)), Some(i64::MAX));
    assert_eq!(frames_for(Tick(i64::MAX), Tick(2)), Some(i64::MAX / 2 + 1));
    assert_eq!(frames_for(Tick(0), Tick(5)), None);
    assert_eq!(frames_for(Tick(-3), Tick(5)), None);
    assert_eq!(frames_for(Tick(5), Tick(0)), None);
}

#[test]
fn a_headless_session_places_the_clip_on_its_next_command() {
    // CLI / MCP / control channel: no UI frame loop calls `poll`; any later command applies it
    let (mut s, seq_id, clip, start, dur) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    let undo_before = s.history.undo.len();
    let res = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy()})).unwrap();
    let id = res["job"].clone();
    let t0 = std::time::Instant::now();
    loop {
        let jobs = s.execute("jobs.list", json!({})).unwrap();
        let finished = jobs.as_array().unwrap().iter().any(|j| j["id"] == id && j["finished"] == true);
        if finished && s.flow_jobs.is_empty() {
            break;
        }
        assert!(t0.elapsed().as_secs() < 60, "job never applied");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(s.history.undo.len(), undo_before + 1);
    let seq = s.project.sequence(seq_id).unwrap();
    let placed = seq.video_tracks.iter().skip(1).flat_map(|t| t.items.iter()).any(|it| it.start == start && it.duration == dur);
    assert!(placed, "the overlay clip sits above V1");
}

#[test]
fn temporal_detail_mode_places_normal_blend_full_opacity_clip_above() {
    let (mut s, seq_id, clip, start, dur) = demo(3);
    let dir = tempfile::tempdir().unwrap();
    let res = s.execute("clip.opticalFlowOverlay.temporalDetail", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(res["mode"], "temporalDetail");
    assert_eq!(res["reconstructionBackend"], filmcraft_render::flow::CPU_NEURAL_BACKEND);
    assert_eq!(res["reconstructionCpuFrames"], 3);
    assert_eq!(res["overlayCpuFrames"], 3);
    assert_eq!(res["reconstructionHardwareFrames"], 0);
    assert_eq!(res["overlayGpuFrames"], 0);
    let new_clip = ClipId(res["clips"][0].as_u64().unwrap());
    let (ti, it) = overlay_of(&s, seq_id, new_clip);
    assert!(ti > 0);
    assert_eq!((it.start, it.duration), (start, dur));
    let op = it.effects.iter().find(|e| e.effect == "opacity").unwrap();
    assert_eq!(op.params["blend"].value, ParamValue::Choice(Blend::Normal.index()));
    assert_eq!(op.params["opacity"].value, ParamValue::Float(100.0));
}

#[test]
fn a_sequence_deleted_while_rendering_says_so() {
    let (mut s, seq_id, clip, _, _) = demo(2);
    let dir = tempfile::tempdir().unwrap();
    let r = rendered(&mut s, seq_id, clip, dir.path());
    let path = r.path.clone();
    let pj = pending(&s, seq_id, vec![r]);
    Arc::make_mut(&mut s.project).items.remove(&seq_id);
    let e = apply(&mut s, pj, true).unwrap_err();
    assert!(e.to_string().contains("deleted"), "{e}");
    assert!(!std::path::Path::new(&path).exists(), "the generated file is deleted");
}

#[test]
fn a_clip_disabled_while_rendering_is_refused() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let r = rendered(&mut s, seq_id, clip, dir.path());
    let pr = Arc::make_mut(&mut s.project);
    pr.sequence_mut(seq_id).unwrap().find_item_mut(clip).unwrap().1.enabled = false;
    let project = s.project.clone();
    let e = {
        let pj = pending(&s, seq_id, vec![r.clone()]);
        apply(&mut s, pj, true)
    }
    .unwrap_err();
    assert!(e.to_string().contains("disabled"), "{e}");
    assert_eq!(*s.project, *project);
    assert!(!std::path::Path::new(&r.path).exists());
}

#[test]
fn a_single_named_clip_works_without_a_selection() {
    let (mut s, seq_id, clip, start, dur) = demo(2);
    s.state.selection.clear();
    let dir = tempfile::tempdir().unwrap();
    let res = s.execute("clip.opticalFlowOverlay.hsvWheel", json!({"clip": clip.0, "dir": dir.path().to_string_lossy(), "wait": true})).unwrap();
    let (ti, it) = overlay_of(&s, seq_id, ClipId(res["clips"][0].as_u64().unwrap()));
    assert!(ti > 0);
    assert_eq!((it.start, it.duration), (start, dur));
}

/// A source whose pictures are still on their way, as a Blob read or a WebCodecs decode on the web.
struct Loading(filmcraft_media::MediaInfo);

impl filmcraft_media::MediaSource for Loading {
    fn info(&self) -> &filmcraft_media::MediaInfo {
        &self.0
    }
    fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
        Err(filmcraft_media::MediaError::Io(filmcraft_media::pending::would_block().to_string()))
    }
    fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
        Err(filmcraft_media::MediaError::Io(filmcraft_media::pending::would_block().to_string()))
    }
}

#[test]
fn a_source_frame_still_loading_fails_the_job_instead_of_drawing_a_blank_track() {
    let (mut s, seq_id, clip, _, _) = demo(2);
    let media = s.project.sequence(seq_id).unwrap().find_item(clip).unwrap().1.item;
    let generator = s.media.cached(media).unwrap();
    s.media.insert(media, Arc::new(Loading(generator.info().clone())));
    let dir = tempfile::tempdir().unwrap();
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    for mode in ["hsvWheel", "temporalDetail"] {
        let e = s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "mode": mode, "dir": dir.path().to_string_lossy(), "wait": true})).unwrap_err();
        assert!(e.to_string().contains("still loading"), "{mode}: {e}");
        assert_eq!(*s.project, *project, "{mode}");
        assert_eq!(s.history.undo.len(), undo, "{mode}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "{mode}: no file is left");
    }
    // once the pictures are there, the same command succeeds
    s.media.insert(media, generator);
    s.execute("clip.opticalFlowOverlay", json!({"clips": [clip.0], "dir": dir.path().to_string_lossy(), "wait": true})).unwrap();
}
