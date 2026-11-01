//! Tests of [`crate::effectcraft_link`]. The ones that render need a real `effectcraft-cli`: they
//! run when `EFFECTCRAFT_CLI` names one and return at once otherwise.

use super::*;

#[test]
fn stems_are_safe_file_names() {
    assert_eq!(safe_stem("A/B:C*?"), "A_B_C__");
    assert_eq!(safe_stem("  .. "), "Clip");
    assert_eq!(safe_stem("Interview 01"), "Interview 01");
    assert_eq!(safe_stem(&"x".repeat(200)).len(), 80);
}

#[test]
fn polling_a_pass_stores_stamps_and_reports_the_first_error() {
    let mut s = Session::default();
    s.ec_links.push(EcLink { project: "a.ecproj".into(), comp: "A".into(), render: "a.mov".into(), stamp: None, cli: None, app: None });
    s.ec_sync = Some(Arc::new(Mutex::new(Some(vec![("a.ecproj".into(), Some((1, 2)), Err("boom".into())), ("gone.ecproj".into(), None, Ok("x.mov".into()))]))));
    assert_eq!(s.poll_effectcraft_sync(), Some(Err("boom".into())));
    assert_eq!(s.ec_links.first().and_then(|l| l.stamp), Some((1, 2)));
    assert!(s.ec_sync.is_none());
    assert_eq!(s.poll_effectcraft_sync(), None);
}

#[test]
fn replacing_without_a_selection_is_refused() {
    let mut s = Session::default();
    assert!(s.execute("effectcraft.replaceClip", json!({})).is_err());
    assert!(s.execute("effectcraft.edit", json!({"clip": 1})).is_err());
}

/// The whole flow against a real `effectcraft-cli`: skipped unless `EFFECTCRAFT_CLI` names one.
#[test]
fn replacing_a_clip_renders_a_composition_and_swaps_the_footage() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 12);
    let (mut s, items, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap_or(ClipId(0));
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false})).unwrap();
    let new_item = ItemId(r["item"].as_u64().unwrap_or(0));
    assert_ne!(Some(new_item), items.first().copied());
    let q = s.project.sequence(seq).map(|q| q.find_item(clip).map(|(_, i)| i.item));
    assert_eq!(q, Some(Some(new_item)));
    // The fixture's audio clip is not linked to the picture, so only the picture clip moves.
    assert_eq!(r["replaced"], json!(1), "{r}");
    assert!(Path::new(r["render"].as_str().unwrap_or("")).exists());
    assert_eq!(s.ec_links.len(), 1);
    // Nothing changed in EffectCraft: nothing to render.
    let again = s.execute("effectcraft.sync", json!({})).map_err(|e| e.to_string());
    assert_eq!(again.map(|v| v["rendered"].clone()), Ok(json!([])));
}

/// The interface's way: the command returns at once and the clip moves when the render is done.
#[test]
fn a_background_replacement_swaps_the_clip_once_the_render_is_done() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-bg");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 12);
    let (mut s, items, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false, "background": true})).unwrap();
    assert_eq!(r["started"], json!(true), "{r}");
    // Nothing moved yet.
    assert_eq!(s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.item)), items.first().copied());
    let mut results = Vec::new();
    for _ in 0..600 {
        results = s.poll_effectcraft_jobs();
        if !results.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(results.len(), 1, "the render finished");
    let done = results.remove(0).unwrap();
    let new_item = ItemId(done["item"].as_u64().unwrap());
    assert_eq!(s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.item)), Some(new_item));
    assert!(s.ec_jobs.is_empty());
    assert_eq!(s.ec_links.len(), 1);
}

#[test]
fn a_background_render_that_fails_reports_why_and_changes_nothing() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    let dir = tmp_dir("ec-link-fail");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 6);
    let (mut s, items, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let r = s
        .execute(
            "effectcraft.replaceClip",
            json!({"clip": clip.0, "dir": dir.join("l").to_string_lossy(), "cli": "definitely-not-effectcraft-cli", "background": true}),
        )
        .unwrap();
    assert_eq!(r["started"], json!(true));
    let mut results = Vec::new();
    for _ in 0..100 {
        results = s.poll_effectcraft_jobs();
        if !results.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(results.len(), 1);
    assert!(results[0].as_ref().is_err_and(|e| e.contains("can't run")));
    assert_eq!(s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.item)), items.first().copied());
    assert!(s.ec_links.is_empty());
}

/// Only the part of the footage the clip shows goes to EffectCraft, and the clip looks the same
/// afterwards. Skipped unless `EFFECTCRAFT_CLI` names a real `effectcraft-cli`.
#[test]
fn a_trimmed_clip_sends_only_its_used_range() {
    use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-trim");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 24);
    let (mut s, items, seq) = session_with(&[&src]);
    let rate = s.sequence_rate();
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    // Use frames 8..16 of the 24-frame movie.
    let mut p = (*s.project).clone();
    if let Some(q) = p.sequence_mut(seq) {
        for tr in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
            for it in tr.items.iter_mut() {
                it.source_in = rate.tick_of(8);
                it.duration = rate.tick_of(8);
            }
        }
    }
    s.project = std::sync::Arc::new(p);
    let before: Vec<Vec<u8>> = (0..8).step_by(3).map(|f| frame_rgba(&mut s, f, 1.0).2).collect();
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false})).unwrap();
    let new_item = ItemId(r["item"].as_u64().unwrap());
    assert_ne!(Some(new_item), items.first().copied());
    let len = s.project.item(new_item).and_then(|i| i.as_media()).map(|m| m.info.duration).unwrap();
    assert!((len.seconds() - rate.tick_of(8).seconds()).abs() < 0.1, "the render is the 8 frames used, not the whole movie: {}s", len.seconds());
    let it = s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.clone())).unwrap();
    assert_eq!(it.source_in, Tick::ZERO, "the render starts where the clip starts");
    for (n, f) in (0..8).step_by(3).enumerate() {
        let after = frame_rgba(&mut s, f, 1.0).2;
        let db = psnr(&before[n], &after);
        assert!(db > 30.0, "frame {f} looks the same after the swap: {db} dB");
    }
}

/// Saving the project in EffectCraft renders again while FilmCraft still has the old movie
/// open (Windows can't replace an open file), and the clip shows the new render.
#[test]
fn a_saved_change_renders_again_even_though_the_old_movie_is_open() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-resync");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 12);
    let (mut s, _, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false})).unwrap();
    let first = r["render"].as_str().unwrap().to_string();
    let project = r["project"].as_str().unwrap().to_string();
    // The movie is open in the media pool now. EffectCraft saves the project again.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::OpenOptions::new().write(true).open(&project).unwrap().set_modified(later).unwrap();
    let r = s.execute("effectcraft.sync", json!({})).unwrap();
    assert_eq!(r["errors"], json!([]), "{r}");
    assert_eq!(r["rendered"].as_array().map(Vec::len), Some(1), "{r}");
    let now_item = s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.item)).unwrap();
    let path = match s.project.item(now_item).and_then(|i| i.as_media()).map(|m| m.media.clone()) {
        Some(MediaRef::File { path }) => path,
        other => panic!("not a file: {other:?}"),
    };
    assert_ne!(path, first, "the clip reads the new render");
    assert!(Path::new(&path).exists());
    // Nothing changed since: nothing to render.
    let r = s.execute("effectcraft.sync", json!({})).unwrap();
    assert_eq!(r["rendered"], json!([]), "{r}");
}

/// The interface's loop: a background replacement, then the periodic look at the project.
#[test]
fn the_periodic_sync_renders_a_project_saved_after_a_background_replacement() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-ui");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 12);
    let (mut s, _, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false, "background": true})).unwrap();
    let project = r["project"].as_str().unwrap().to_string();
    let mut done = Vec::new();
    for _ in 0..600 {
        done = s.poll_effectcraft_jobs();
        if !done.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done.first().is_some_and(|r| r.is_ok()), "{done:?}");
    let link = s.ec_links.first().cloned().unwrap();
    assert_eq!(link.stamp, stamp_of(&project), "the link remembers the project as it was when the render was made");
    // EffectCraft saves.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::OpenOptions::new().write(true).open(&project).unwrap().set_modified(later).unwrap();
    s.start_effectcraft_sync();
    assert!(s.ec_sync.is_some(), "a changed project starts a render");
    let mut result = None;
    for _ in 0..600 {
        result = s.poll_effectcraft_sync();
        if result.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(result, Some(Ok(1)));
    s.start_effectcraft_sync();
    assert!(s.ec_sync.is_none(), "nothing changed since");
}

#[test]
fn numbered_names_skip_what_exists() {
    use crate::media_test_util::tmp_dir;
    let dir = tmp_dir("ec-link-names");
    let touch = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();
    // Keep Old: `-2`, `-3`... counted from the original name, by project or movie.
    assert_eq!(free_stem(&dir, "Clip EffectCraft"), "Clip EffectCraft-2");
    touch("Clip EffectCraft-2.mov");
    touch("Clip EffectCraft-3.ecproj");
    assert_eq!(free_stem(&dir, "Clip EffectCraft"), "Clip EffectCraft-4");
    // Re-renders: ` v2`, ` v3`... and a version of a version counts on from the original.
    let first = dir.join("Clip EffectCraft.mov").to_string_lossy().into_owned();
    let v2 = variant_of(&first);
    assert!(v2.ends_with("Clip EffectCraft v2.mov"), "{v2}");
    touch("Clip EffectCraft v2.mov");
    assert!(variant_of(&first).ends_with("Clip EffectCraft v3.mov"));
    assert!(variant_of(&v2).ends_with("Clip EffectCraft v3.mov"));
}

#[test]
fn existing_files_ask_first_and_change_nothing() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    let dir = tmp_dir("ec-link-ask");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 6);
    let (mut s, items, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let out = dir.join("linked");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("src.mov EffectCraft.ecproj"), b"old").unwrap();
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "cli": "definitely-not-effectcraft-cli"})).unwrap();
    assert_eq!(r["needsDecision"], json!("exists"), "{r}");
    assert_eq!(r["paths"].as_array().map(Vec::len), Some(1));
    assert_eq!(std::fs::read(out.join("src.mov EffectCraft.ecproj")).unwrap(), b"old");
    assert_eq!(s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.item)), items.first().copied());
    // A wrong answer is refused, not guessed.
    let e = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "existing": "maybe"}));
    assert!(e.is_err());
}

/// Keep Old writes `-2` beside the first files; Overwrite reuses the names.
#[test]
fn keep_old_numbers_the_new_files_and_overwrite_reuses_the_old_ones() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-keep");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 8);
    let (mut s, _, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    let out = dir.join("linked").to_string_lossy().into_owned();
    let first = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out, "open": false})).unwrap();
    assert!(first["render"].as_str().unwrap().ends_with("src.mov EffectCraft.mov"), "{first}");
    // Replacing the same clip again finds the files from the first time and asks.
    let again = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out, "open": false})).unwrap();
    assert_eq!(again["needsDecision"], json!("exists"), "{again}");
    // Keep Old: new files `-2` beside the first, which stay.
    let keep = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out, "open": false, "existing": "keepOld"})).unwrap();
    assert!(keep["render"].as_str().unwrap().ends_with("src.mov EffectCraft-2.mov"), "{keep}");
    assert!(keep["project"].as_str().unwrap().ends_with("src.mov EffectCraft-2.ecproj"), "{keep}");
    assert!(Path::new(first["render"].as_str().unwrap()).exists(), "the first render is kept");
    assert!(Path::new(first["project"].as_str().unwrap()).exists(), "the first project is kept");
    // Overwrite: the first project's name again (the movie too, or a numbered one if it is in use).
    let over = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out, "open": false, "existing": "overwrite"})).unwrap();
    assert!(over["project"].as_str().unwrap().ends_with("src.mov EffectCraft.ecproj"), "{over}");
    assert!(Path::new(over["render"].as_str().unwrap()).exists(), "{over}");
}

#[test]
fn progress_lines_are_read_and_other_lines_are_not() {
    assert_eq!(parse_progress("progress 16 180 1.4 14.2"), Some((16, 180, Some(14.2))));
    assert_eq!(parse_progress("progress 0 180 0.0 -"), Some((0, 180, None)));
    assert_eq!(parse_progress("done: T -> out.mov in 2.0s"), None);
    assert_eq!(parse_progress("progress x y"), None);
    assert_eq!(parse_progress("progress 5"), None);
    assert_eq!(duration_text(4.4), "4 s");
    assert_eq!(duration_text(185.0), "3 min 05 s");
}

#[test]
fn progress_is_reported_as_frames_and_time_left() {
    let p = Progress::default();
    report_progress(&p, 60, 180, Some(75.0));
    assert_eq!(p.fraction(), 60.0 / 180.0);
    assert_eq!(*p.status.lock().unwrap(), "Rendering 33% (frame 60 of 180, about 1 min 15 s left)");
    // a CLI that reports more done than total, or no total, can't push the bar past its end
    report_progress(&p, 500, 0, None);
    assert_eq!(p.fraction(), 1.0);
}

#[test]
fn ecproj_files_are_listed_by_the_media_browser() {
    assert_eq!(crate::media_browser::kind_of("Clip EffectCraft.ecproj"), Some("effectcraft"));
    assert_eq!(crate::media_browser::kind_of("Clip EffectCraft.ECPROJ"), Some("effectcraft"));
    assert!(crate::media_browser::passes("all", "effectcraft", "x.ecproj"));
    assert!(!crate::media_browser::passes("video", "effectcraft", "x.ecproj"));
}

/// What the user sees afterwards: the clip and the new project item take the Dynamic Link
/// label, the render is a job that finished, and the Media Browser shows the new files.
#[test]
fn a_replaced_clip_changes_color_has_a_job_and_shows_in_the_media_browser() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-link-look");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 12);
    let (mut s, _, seq) = session_with(&[&src]);
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    s.prefs.labels.defaults.dynamic_link = Label::Rose;
    let before = s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.label)).unwrap();
    assert_ne!(before, Label::Rose, "the fixture isn't already that color");
    let out = dir.join("linked");
    let r = s.execute("effectcraft.replaceClip", json!({"clip": clip.0, "dir": out.to_string_lossy(), "open": false, "background": true})).unwrap();
    let mut done = Vec::new();
    for _ in 0..600 {
        done = s.poll_effectcraft_jobs();
        if !done.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let done = done.remove(0).unwrap();
    // color
    let it = s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.clone())).unwrap();
    assert_eq!(it.label, Label::Rose);
    let new_item = ItemId(done["item"].as_u64().unwrap());
    assert_eq!(s.project.item(new_item).map(|i| i.label), Some(Label::Rose));
    // job
    let job = s.jobs.iter().find(|j| j.label.starts_with("EffectCraft: ")).expect("the render is in the Jobs list");
    assert!(job.progress.finished.load(Ordering::Relaxed));
    assert_eq!(job.progress.fraction(), 1.0);
    assert!(matches!(job.result.lock().unwrap().as_ref(), Some(Ok(rep)) if rep.path.ends_with(".mov")));
    assert!(s.effectcraft_progress().is_none(), "nothing is rendering any more");
    // media browser
    let render = r["render"].as_str().unwrap().to_string();
    let project = r["project"].as_str().unwrap().to_string();
    assert_eq!(Path::new(&s.browser.dir), out.as_path());
    assert!(s.browser.selection.contains(&render) && s.browser.selection.contains(&project), "{:?}", s.browser.selection);
}

/// The timeline paints a progress overlay on the clips of a link while its project renders.
#[test]
fn clips_of_a_rendering_link_get_an_overlay_until_it_is_done() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    let dir = tmp_dir("ec-link-overlay");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 160, 90, 6);
    let (mut s, items, seq) = session_with(&[&src]);
    let item = items[0];
    let clip = s.project.sequence(seq).and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id).unwrap();
    assert_eq!(s.effectcraft_clip_overlay(clip, item), None, "nothing renders");
    // a link whose movie is this clip's footage, re-rendering
    s.ec_links.push(EcLink { project: "p.ecproj".into(), comp: "C".into(), render: src.to_string_lossy().into_owned(), stamp: None, cli: None, app: None });
    let prog = Arc::new(Progress::default());
    s.ec_progress.push(("p.ecproj".into(), prog.clone()));
    prog.total.store(200, Ordering::Relaxed);
    assert_eq!(s.effectcraft_clip_overlay(clip, item), Some((0.0, false)));
    prog.done.store(50, Ordering::Relaxed);
    assert_eq!(s.effectcraft_clip_overlay(clip, item), Some((0.25, true)));
    // another project rendering doesn't mark this clip
    s.ec_progress[0].0 = "other.ecproj".into();
    assert_eq!(s.effectcraft_clip_overlay(clip, item), None);
    s.ec_progress[0].0 = "p.ecproj".into();
    prog.finished.store(true, Ordering::Relaxed);
    assert_eq!(s.effectcraft_clip_overlay(clip, item), None, "done: no overlay");
}

/// A small EffectCraft project (one 2 s composition, no layers) made with the real CLI.
fn make_ecproj(dir: &Path, name: &str) -> String {
    let cli = std::env::var("EFFECTCRAFT_CLI").unwrap();
    let project = dir.join(name).to_string_lossy().into_owned();
    let comp = json!({"name": "Titles", "width": 320, "height": 180, "frameRate": 24, "duration": 2}).to_string();
    run_cli(&cli, &["run".into(), "--empty".into(), "--save-as".into(), project.clone(), "comp.new".into(), comp]).unwrap();
    project
}

#[test]
fn project_files_are_recognised() {
    assert!(is_project("C:\\x\\Intro.ecproj"));
    assert!(is_project("intro.ECPROJ"));
    assert!(!is_project("intro.mov"));
    assert!(!is_project("ecproj"));
}

/// Importing an .ecproj (Import, a drop, the Media Browser) gives an item that plays the
/// composition's render, labelled as a Dynamic Link item and linked to its project.
#[test]
fn an_imported_ecproj_is_an_item_that_plays_its_composition() {
    use crate::media_test_util::tmp_dir;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-import");
    let project = make_ecproj(&dir, "Intro.ecproj");
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [project]})).unwrap();
    assert_eq!(r["errors"], json!([]), "{r}");
    let item = ItemId(r["items"][0].as_u64().unwrap());
    let it = s.project.item(item).unwrap();
    assert_eq!(it.name, "Intro");
    assert_eq!(it.label, s.prefs.labels.defaults.dynamic_link);
    let m = it.as_media().unwrap();
    let v = m.info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (320, 180));
    assert!((m.info.duration.seconds() - 2.0).abs() < 0.1);
    let MediaRef::File { path } = &m.media else { panic!("not a file") };
    assert!(Path::new(path).exists(), "the composition is rendered");
    assert!(s.ec_links.iter().any(|l| l.project == project && l.render == *path));
    assert!(s.offline.missing.is_empty());
    // Importing it again gives the same item, not a second render.
    let again = s.execute("file.import", json!({"paths": [project]})).unwrap();
    assert_eq!(again["items"][0].as_u64(), Some(item.0));
    assert_eq!(s.project.items.values().filter(|i| i.as_media().is_some()).count(), 1);
}

/// What the interface does: the item is there at once and can be placed while it renders.
#[test]
fn a_dropped_ecproj_can_be_placed_before_its_render_is_done() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-import-bg");
    let project = make_ecproj(&dir, "Lower Third.ecproj");
    let other = dir.join("other.mov");
    make_movie(&other, DemoScene::Aurora, 320, 180, 12);
    let (mut s, _, seq) = session_with(&[&other]);
    s.ec_background = true;
    s.prefs.labels.defaults.dynamic_link = Label::Rose;
    let r = s.execute("file.import", json!({"paths": [project]})).unwrap();
    let item = ItemId(r["items"][0].as_u64().unwrap());
    // there, named, sized and long as the composition, though no movie exists yet
    let MediaRef::File { path } = s.project.item(item).and_then(|i| i.as_media()).map(|m| m.media.clone()).unwrap() else { panic!("not a file") };
    let m = s.project.item(item).and_then(|i| i.as_media()).unwrap();
    assert_eq!(m.info.video.as_ref().map(|v| (v.width, v.height)), Some((320, 180)));
    assert!((m.info.duration.seconds() - 2.0).abs() < 0.1);
    assert!(!Path::new(&path).exists() || s.ec_imports.is_empty(), "still rendering, or already finished");
    assert!(s.offline.missing.is_empty(), "a render in progress is not an offline-media problem");
    // dropped on the timeline
    let placed = s.execute("timeline.place", json!({"item": item.0, "track": "V1", "seconds": 5.0})).unwrap();
    let clip = filmcraft_project::ClipId(placed["clips"][0].as_u64().unwrap());
    assert!(s.project.sequence(seq).and_then(|q| q.find_item(clip)).is_some());
    assert_eq!(s.project.sequence(seq).and_then(|q| q.find_item(clip).map(|(_, i)| i.label)), Some(Label::Rose));
    if !s.ec_imports.is_empty() {
        let (fraction, _frames_flow) = s.effectcraft_clip_overlay(clip, item).expect("the clip shows the render's progress");
        assert!((0.0..=1.0).contains(&fraction), "{fraction}");
    }
    // finished: the movie is the item's media, and the overlay is gone
    let mut done = Vec::new();
    for _ in 0..600 {
        done.extend(s.poll_effectcraft_jobs());
        if s.ec_imports.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done.iter().all(|r| r.is_ok()), "{done:?}");
    assert!(Path::new(&path).exists(), "the render is written");
    assert_eq!(s.effectcraft_clip_overlay(clip, item), None);
    assert!(s.offline.missing.is_empty());
    assert!(s.ec_pending.is_empty());
    // nothing changed in the project since: no re-render
    s.start_effectcraft_sync();
    assert!(s.ec_sync.is_none());
}

#[test]
fn an_ecproj_that_cannot_be_rendered_says_so_and_is_not_linked() {
    use crate::media_test_util::tmp_dir;
    if std::env::var_os("EFFECTCRAFT_CLI").is_none() {
        return;
    }
    let dir = tmp_dir("ec-import-bad");
    let bad = dir.join("Broken.ecproj");
    std::fs::write(&bad, b"not a project").unwrap();
    let mut s = Session::default();
    let e = s.execute("file.import", json!({"paths": [bad.to_string_lossy()]})).unwrap_err().to_string();
    assert!(e.contains("not a project"), "{e}");
    assert!(s.ec_links.is_empty() && s.ec_imports.is_empty() && s.ec_pending.is_empty());
    assert!(s.project.items.values().all(|i| i.as_media().is_none()), "no item is left behind");
    let e = s.execute("file.import", json!({"paths": ["Q:/definitely/not/here.ecproj"]})).unwrap_err().to_string();
    assert!(e.contains("does not exist"), "{e}");
}

/// New commands get hostile-parameter tests: wrong types, absurd ids, odd paths. Errors are fine;
/// a panic is not.
#[test]
fn hostile_parameters_give_errors_not_panics() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    let dir = tmp_dir("ec-link-hostile");
    let src = dir.join("src.mov");
    make_movie(&src, DemoScene::Aurora, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&src]);
    let junk = [
        json!(null),
        json!([]),
        json!("text"),
        json!({"clip": "a"}),
        json!({"clip": u64::MAX}),
        json!({"clip": -1}),
        json!({"clip": 1.5e300}),
        json!({"path": 5}),
        json!({"path": ""}),
        json!({"project": "", "comp": 7}),
        json!({"dir": "\u{0}", "cli": "\u{0}", "app": ["x"]}),
        json!({"dir": "ODD-DIR", "existing": 5}),
        json!({"existing": "maybe", "background": "yes", "open": "no", "reveal": 3}),
        json!({"paths": [null, 1, "", "\u{0}.ecproj", "x.ecproj"]}),
    ];
    // Nothing here may start a real program or write outside the test's folder: every call names
    // tools that don't exist and folders under `dir`, and none opens an app.
    let harmless = |p: &Value| {
        let mut q = p.clone();
        if let Some(o) = q.as_object_mut() {
            o.entry("cli").or_insert(json!("definitely-not-effectcraft-cli"));
            o.entry("app").or_insert(json!("definitely-not-effectcraft"));
            o.entry("open").or_insert(json!(false));
            let odd = dir.join("odd \u{202e} name").to_string_lossy().into_owned();
            match o.get("dir").and_then(Value::as_str) {
                Some("ODD-DIR") => {
                    o.insert("dir".into(), json!(odd));
                }
                Some(_) => {}
                None => {
                    o.insert("dir".into(), json!(dir.join("out").to_string_lossy()));
                }
            }
        }
        q
    };
    for id in ["effectcraft.replaceClip", "effectcraft.edit", "effectcraft.link", "effectcraft.sync", "effectcraft.openProject", "file.import"] {
        for p in &junk {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(id, harmless(p))));
            assert!(run.is_ok(), "{id} panicked on {p}");
        }
    }
    // the same against a clip that exists
    let clip =
        s.state.selection.first().copied().or_else(|| s.active_sequence().and_then(|q| q.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id));
    for p in &junk {
        let mut q = harmless(p);
        if let (Some(o), Some(c)) = (q.as_object_mut(), clip) {
            o.insert("clip".into(), json!(c.0));
        }
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("effectcraft.replaceClip", q)));
        assert!(run.is_ok(), "replaceClip panicked on {p}");
    }
    // and nothing was left half-done
    assert!(s.ec_links.is_empty() && s.ec_imports.is_empty() && s.ec_jobs.is_empty() && s.ec_pending.is_empty());
}

/// CWE-427: an installed program must not run `C:\EffectCraft\target\release\...`, a folder any
/// user can create. Only development builds look in the parent folders.
#[test]
fn the_parent_folder_search_is_for_development_builds_only() {
    let exe_dir = Path::new("C:\\Program Files\\FilmCraft\\bin");
    let release = tool_candidates(Some(exe_dir), "effectcraft-cli.exe", false);
    assert_eq!(release, vec![exe_dir.join("effectcraft-cli.exe")], "only the copy beside the program");
    let dev = tool_candidates(Some(exe_dir), "effectcraft-cli.exe", true);
    assert_eq!(dev.first(), release.first(), "the copy beside the program comes first");
    assert!(dev.len() > 1 && dev.iter().skip(1).all(|p| p.components().any(|c| c.as_os_str() == "EffectCraft")), "{dev:?}");
    assert!(tool_candidates(None, "effectcraft-cli", true).is_empty());
}

/// Switching to another project while a background import renders must not bind the render to
/// whatever item of the new project has the same id.
#[test]
fn a_render_finishing_after_a_project_switch_changes_nothing() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "First"})).unwrap();
    let (prog, _) = register_job(&mut s, "Lower Third", "Starting...");
    let imported = s.execute("file.importDemoFootage", json!({"scene": "Aurora"})).unwrap();
    let item = ItemId(imported["items"][0].as_u64().unwrap());
    let done = Arc::new(Mutex::new(Some(Ok("elsewhere.mov".to_string()))));
    s.ec_links.push(EcLink { project: "a.ecproj".into(), comp: "A".into(), render: "r.mov".into(), stamp: None, cli: None, app: None });
    s.ec_pending.insert("r.mov".into());
    s.ec_imports.push(EcImport { item, project: "a.ecproj".into(), comp: "A".into(), render: "r.mov".into(), progress: prog, done });
    // another project, whose first item has the same id
    s.execute("file.newProject", json!({"name": "Second"})).unwrap();
    s.execute("file.importDemoFootage", json!({"scene": "Aurora"})).unwrap();
    let before = s.project.item(item).and_then(|i| i.as_media()).map(|m| m.media.clone());
    assert!(before.is_some(), "the new project has an item with the old item's id");
    assert!(s.ec_links.is_empty() && s.ec_imports.is_empty() && s.ec_jobs.is_empty() && s.ec_pending.is_empty());
    assert!(s.poll_effectcraft_jobs().is_empty());
    assert_eq!(s.project.item(item).and_then(|i| i.as_media()).map(|m| m.media.clone()), before, "the unrelated item keeps its footage");
}

/// A synchronous import that fails leaves no item and no undo step behind.
#[test]
fn a_failed_synchronous_import_takes_its_item_back() {
    let mut s = Session::default();
    let mark = s.undo_mark();
    s.execute("file.importDemoFootage", json!({"scene": "Aurora"})).unwrap();
    assert!(s.project.items.values().any(|i| i.as_media().is_some()));
    s.ec_links.push(EcLink { project: "a.ecproj".into(), comp: "A".into(), render: "r.mov".into(), stamp: None, cli: None, app: None });
    abandon_import(&mut s, mark, "a.ecproj");
    assert!(s.project.items.values().all(|i| i.as_media().is_none()), "the item is gone");
    assert!(s.ec_links.is_empty());
    assert_eq!(s.undo_mark(), mark, "and so is its undo step");
}

/// Undo after a re-render goes back to the old movie, so the old movie must still be there.
#[test]
fn undoing_a_re_render_finds_the_old_movie() {
    use crate::media_test_util::{make_movie, session_with, tmp_dir};
    use filmcraft_media::DemoScene;
    let dir = tmp_dir("ec-link-undo");
    let (old, new) = (dir.join("old.mov"), dir.join("new.mov"));
    make_movie(&old, DemoScene::Aurora, 64, 36, 4);
    make_movie(&new, DemoScene::Plasma, 64, 36, 4);
    let (mut s, items, _) = session_with(&[&old]);
    let item = items[0];
    let (old_s, new_s) = (old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned());
    s.ec_links.push(EcLink { project: "a.ecproj".into(), comp: "A".into(), render: old_s.clone(), stamp: None, cli: None, app: None });
    let path_of = |s: &Session| match s.project.item(item).and_then(|i| i.as_media()).map(|m| m.media.clone()) {
        Some(MediaRef::File { path }) => path,
        other => panic!("not a file: {other:?}"),
    };
    s.switch_linked_render("a.ecproj", None, &new_s).unwrap();
    assert_eq!(path_of(&s), new_s);
    assert!(old.exists(), "the superseded movie is kept");
    s.undo();
    assert_eq!(path_of(&s), old_s);
    assert!(Path::new(&path_of(&s)).exists(), "so the clips are not offline after Undo");
}

#[test]
fn a_missing_cli_is_reported_not_fatal() {
    let r = run_cli("definitely-not-effectcraft-cli", &[]);
    assert!(r.is_err());
}
