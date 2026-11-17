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
    let it = s.project.sequence(seq_id).unwrap().find_item(clip).unwrap().1.clone();
    Rendered { clip, start: it.start, dur: it.duration, path: res["paths"][0].as_str().unwrap().to_string(), bytes: None }
}

fn pending(s: &Session, seq_id: ItemId, clips: Vec<Rendered>) -> PendingFlow {
    let look = Look { mode: FlowVizMode::HsvWheel, max_motion: 16.0, opacity: 85.0, blend: Blend::Screen.index() };
    let outcome = Outcome { clips, cpu_frames: 4, ..Default::default() };
    PendingFlow {
        job: u64::MAX,
        seq: seq_id,
        look,
        epoch: s.document_epoch,
        results: Arc::new(Mutex::new(Some(outcome))),
        hw_name: Arc::new(Mutex::new("hardware".into())),
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

    let pj = pending(&s, seq_id, vec![good.clone(), missing]);
    let r = apply(&mut s, pj);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(*s.project, *project, "the first clip's overlay was taken back");
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
    assert!(!std::path::Path::new(&good.path).exists(), "generated files are deleted");
}

#[test]
fn a_clip_moved_while_rendering_changes_nothing() {
    let (mut s, seq_id, clip, _, _) = demo(4);
    let dir = tempfile::tempdir().unwrap();
    let mut r = rendered(&mut s, seq_id, clip, dir.path());
    r.start += Tick(1);
    let (project, undo) = (s.project.clone(), s.history.undo.len());
    let pj = pending(&s, seq_id, vec![r]);
    let e = apply(&mut s, pj).unwrap_err();
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
    let res = s.execute("clip.opticalFlowOverlay.kineticGlow", json!({"dir": dir.path().to_string_lossy()})).unwrap();
    assert!(res["job"].as_u64().is_some(), "{res}");
    assert_eq!(res["frames"], 3);
    let second = s.execute("clip.opticalFlowOverlay.kineticGlow", json!({"dir": dir.path().to_string_lossy()}));
    assert!(second.is_err() || s.flow_jobs.is_empty(), "one job per sequence at a time");
    poll_until_done(&mut s);
    assert_eq!(s.history.undo.len(), undo_before + 1);
    let placed = s.state.selection.first().copied().unwrap();
    let (ti, it) = overlay_of(&s, seq_id, placed);
    assert!(ti > 0);
    assert_eq!((it.start, it.duration), (start, dur));
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
