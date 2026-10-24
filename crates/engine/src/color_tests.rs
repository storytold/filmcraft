use serde_json::json;

use crate::Session;
use filmcraft_color::{Lut, Lut3d};
use filmcraft_project::{ClipId, ParamValue};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The first V1 clip under the playhead, selected.
fn pick_clip(s: &mut Session) -> ClipId {
    let q = s.active_sequence().unwrap();
    let t = s.playhead();
    // the demo opens with the playhead inside the first V1 clip (its first frames fade from black)
    let id = q.video_tracks[0].item_at(t).unwrap().id;
    s.state.selection = vec![id];
    id
}

fn lumetri_text(s: &Session, clip: ClipId, p: &str) -> String {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    let e = it.effects.iter().find(|e| e.effect == "lumetri").unwrap();
    match e.param(p).map(|v| &v.value) {
        Some(ParamValue::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn mean(img: &filmcraft_render::Image) -> [f32; 3] {
    assert!(img.px.iter().any(|v| *v > 0.01), "blank render");
    let mut m = [0f64; 3];
    for p in img.px.as_chunks::<4>().0 {
        for k in 0..3 {
            m[k] += p[k] as f64;
        }
    }
    let n = (img.px.len() / 4) as f64;
    m.map(|v| (v / n) as f32)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-color-{}-{name}", std::process::id()));
    d.to_string_lossy().to_string()
}

#[test]
fn import_lut_set_input_and_look_then_undo() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let before = mean(&s.render_program(0.125).unwrap());
    // a LUT that swaps red and blue
    let path = tmp("swap.cube");
    std::fs::write(&path, Lut::from_cube(Lut3d::from_fn(17, |c| [c[2], c[1], c[0]])).to_cube()).unwrap();
    let r = s.execute("lumetri.setInputLut", json!({"path": path})).unwrap();
    let lref = r["lut"].as_str().unwrap().to_string();
    assert!(lref.starts_with("lib:"));
    assert_eq!(s.project.luts.len(), 1);
    assert_eq!(lumetri_text(&s, clip, "input_lut"), lref);
    let after = mean(&s.render_program(0.125).unwrap());
    assert!((after[0] - before[2]).abs() < 0.02 && (after[2] - before[0]).abs() < 0.02, "{before:?} → {after:?}");
    // importing the same file again reuses the library entry
    let again = s.execute("lut.import", json!({"path": path})).unwrap();
    assert_eq!(again["reused"], true);
    // Input LUT section switch: Basic Correction off bypasses it
    s.execute("lumetri.setSection", json!({"section": "basic", "on": false})).unwrap();
    let off = mean(&s.render_program(0.125).unwrap());
    assert!((off[0] - before[0]).abs() < 0.01, "{off:?} vs {before:?}");
    s.execute("lumetri.setSection", json!({"section": "basic"})).unwrap();
    // built-in look by reference
    s.execute("lumetri.setLook", json!({"lut": "builtin:look-monochrome"})).unwrap();
    let mono = mean(&s.render_program(0.125).unwrap());
    assert!((mono[0] - mono[2]).abs() < 0.03, "{mono:?}");
    assert!(s.execute("lumetri.setLook", json!({"lut": "lib:nope"})).is_err());
    // the library and the references survive save/load
    let lib = s.execute("lut.list", json!({})).unwrap();
    assert_eq!(lib["library"].as_array().unwrap().len(), 1);
    assert!(lib["builtin"].as_array().unwrap().iter().any(|b| b["ref"] == "builtin:slog3-sgamut3cine-to-rec709"));
    let js = serde_json::to_string(&*s.project).unwrap();
    let back: filmcraft_project::Project = serde_json::from_str(&js).unwrap();
    assert_eq!(back.luts, s.project.luts);
    // undo the look, the section toggles and the input LUT
    for _ in 0..4 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(lumetri_text(&s, clip, "look_lut"), "");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn lut_folders_list_cube_and_3dl_files_grouped_by_subfolder() {
    let root = std::env::temp_dir().join(format!("filmcraft-lut-folders-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, text) in [
        ("Creative/RED_LUSH.cube", "TITLE \"x\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n"),
        ("Creative/sub/deep.3DL", "3DMESH\n"),
        ("Creative/.hidden.cube", "nope"),
        ("Creative/readme.txt", "not a LUT"),
        ("top.cube", "LUT_3D_SIZE 2\n"),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let luts = crate::color::scan_lut_folders(&[root.clone(), root.join("missing")]);
    let seen: Vec<(String, String)> = luts.iter().map(|l| (l.group.clone(), l.name.clone())).collect();
    assert_eq!(
        seen,
        vec![
            ("Creative".to_string(), "RED_LUSH".to_string()),
            ("Creative / sub".to_string(), "deep".to_string()),
            (root.file_name().unwrap().to_string_lossy().to_string(), "top".to_string()),
        ]
    );
    // a folder pick is an import plus the slot assignment: the project stays self-contained
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let path = luts[0].path.clone();
    let r = s.execute("lumetri.setInputLut", json!({"clip": clip.0, "path": path})).unwrap();
    assert!(r["lut"].as_str().unwrap().starts_with("lib:"));
    assert_eq!(s.project.luts[0].source_path.as_deref(), Some(luts[0].path.as_str()));
    // the query itself never fails, whatever the folders hold
    let q = s.execute("lut.folders", json!({})).unwrap();
    assert!(q["folders"].is_array() && q["luts"].is_array());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lut_folder_scan_reads_a_bounded_number_of_entries_per_folder() {
    let base = std::env::temp_dir().join(format!("filmcraft-lut-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (wide, nested, small) = (base.join("wide"), base.join("nested"), base.join("small"));
    for (dir, n) in [(wide.clone(), 25), (nested.join("sub"), 25), (small.clone(), 1)] {
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            std::fs::write(dir.join(format!("l{i:02}.cube")), "").unwrap();
        }
    }
    let count = |luts: &[crate::color::FolderLut], dir: &std::path::Path| luts.iter().filter(|l| std::path::Path::new(&l.path).starts_with(dir)).count();
    // each folder gets its own budget of 10 entries: 10 of the 25 files, the subfolder entry plus
    // 9 files beneath it, and the next folder is still read
    let luts = crate::color::scan_lut_folders_within(&[wide.clone(), nested.clone(), small.clone()], 10, 2000);
    assert_eq!((count(&luts, &wide), count(&luts, &nested), count(&luts, &small)), (10, 9, 1));
    // the real budget lists them all
    let luts = crate::color::scan_lut_folders(&[wide.clone(), nested.clone(), small.clone()]);
    assert_eq!((count(&luts, &wide), count(&luts, &nested), count(&luts, &small)), (25, 25, 1));
    // no budget reads nothing
    assert!(crate::color::scan_lut_folders_within(&[wide], 0, 2000).is_empty());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn lut_folders_list_each_file_once() {
    let base = std::env::temp_dir().join(format!("filmcraft-lut-dedup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (a, nested, b) = (base.join("A"), base.join("A").join("LUTs"), base.join("B"));
    for dir in [&nested, &b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for p in [a.join("x.cube"), nested.join("y.cube"), b.join("z.cube")] {
        std::fs::write(p, "").unwrap();
    }
    // a folder named twice, and a folder inside another one
    let luts = crate::color::scan_lut_folders(&[a.clone(), b.clone(), a.clone(), nested.clone()]);
    let mut names: Vec<&str> = luts.iter().map(|l| l.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["x", "y", "z"], "{luts:?}");
    // y keeps its first listing, under A
    assert_eq!(luts.iter().find(|l| l.name == "y").map(|l| l.group.as_str()), Some("LUTs"));
    // a repeat does not count toward the LUT limit: with the nested folder first, A adds only x
    // and B's z still fits under a limit of 3
    let luts = crate::color::scan_lut_folders_within(&[nested.clone(), a.clone(), b.clone()], 100, 3);
    let mut names: Vec<&str> = luts.iter().map(|l| l.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["x", "y", "z"], "{luts:?}");
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn picking_a_copy_of_a_library_lut_records_its_path() {
    let base = std::env::temp_dir().join(format!("filmcraft-lut-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let text = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let (p1, p2) = (base.join("one.cube").to_string_lossy().to_string(), base.join("two.cube").to_string_lossy().to_string());
    std::fs::write(&p1, text).unwrap();
    std::fs::write(&p2, text).unwrap();
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let r1 = s.execute("lumetri.setInputLut", json!({"clip": clip.0, "path": p1})).unwrap();
    let r2 = s.execute("lumetri.setInputLut", json!({"clip": clip.0, "path": p2})).unwrap();
    // one library entry, now from the copy just picked
    assert_eq!(r1["lut"], r2["lut"]);
    assert_eq!(s.project.luts.len(), 1);
    assert_eq!(s.project.luts[0].source_path.as_deref(), Some(p2.as_str()));
    // the same path again is no edit
    let n = s.history.undo.len();
    s.execute("lut.import", json!({"path": p2})).unwrap();
    assert_eq!(s.history.undo.len(), n);
    // a bad target changes nothing, even with the undo history full (where an edit would drop
    // its oldest step): the path stays, no new entry, the same history
    let p3 = base.join("three.cube").to_string_lossy().to_string();
    let p4 = base.join("four.cube").to_string_lossy().to_string();
    std::fs::write(&p3, text).unwrap();
    std::fs::write(&p4, text.replace("1 1 1", "0.5 0.5 0.5")).unwrap();
    s.history.limit = s.history.undo.len();
    let labels = |s: &Session| s.history.undo.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>();
    let before = labels(&s);
    let revision = s.revision;
    s.state.selection = vec![];
    for path in [&p3, &p4] {
        assert!(s.execute("lumetri.setInputLut", json!({"clip": 9_999_999, "path": path})).is_err());
        assert!(s.execute("lumetri.setLook", json!({"path": path})).is_err(), "no clip and nothing selected");
    }
    assert_eq!(labels(&s), before);
    assert_eq!(s.revision, revision);
    assert_eq!(s.project.luts.len(), 1);
    assert_eq!(s.project.luts[0].source_path.as_deref(), Some(p2.as_str()));
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn export_builtin_and_library_luts() {
    let mut s = demo();
    for (fmt, ext) in [("cube", "cube"), ("3dl", "3dl")] {
        let path = tmp(&format!("slog3.{ext}"));
        s.execute("lut.export", json!({"lut": "builtin:slog3-sgamut3cine-to-rec709", "path": path, "format": fmt})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let back = Lut::parse(&text, filmcraft_color::LutFormat::from_path(&path)).unwrap();
        let orig = filmcraft_render::luts::resolve(None, "builtin:slog3-sgamut3cine-to-rec709").unwrap();
        for c in [[0.4, 0.4, 0.4], [0.6, 0.3, 0.2]] {
            let (a, b) = (orig.apply(c), back.apply(c));
            assert!((0..3).all(|k| (a[k] - b[k]).abs() < 2e-3), "{fmt}: {a:?} vs {b:?}");
        }
        // round trip through the library
        let r = s.execute("lut.import", json!({"path": path})).unwrap();
        assert!(r["ref"].as_str().unwrap().starts_with("lib:"));
        let _ = std::fs::remove_file(&path);
    }
    assert_eq!(s.project.luts.len(), 2);
    let id = s.project.luts[0].id.clone();
    s.execute("lut.remove", json!({"id": id})).unwrap();
    assert_eq!(s.project.luts.len(), 1);
    assert!(s.execute("lut.import", json!({"path": "/nonexistent.cube"})).is_err());
}

#[test]
fn set_param_fills_in_parameters_missing_from_old_instances() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    // simulate an instance saved before the section switches existed
    s.edit_sequence("strip", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).unwrap();
        let e = it.effects.iter_mut().find(|e| e.effect == "lumetri").unwrap();
        e.params.remove("vignette_on");
        Ok(())
    })
    .unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "lumetri", "param": "vignette_on", "value": false})).unwrap();
}

#[test]
fn apply_match_grades_the_clip_towards_the_reference() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let q = s.active_sequence().unwrap();
    // reference: the middle of the last clip on V1 (a different shot)
    let other = q.video_tracks[0].items.iter().rfind(|i| i.id != clip).unwrap();
    let reference = other.start + filmcraft_time::Tick(other.duration.0 / 2);
    let r = s.execute("lumetri.applyMatch", json!({"referenceTime": reference.0, "faceDetection": true})).unwrap();
    let (before, after) = (r["distanceBefore"].as_f64().unwrap(), r["distanceAfter"].as_f64().unwrap());
    assert!(after < before * 0.8, "match improves the statistics distance: {r}");
    let q = s.active_sequence().unwrap();
    let e = q.find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri").unwrap().clone();
    let moved = ["wheel_shadows", "wheel_midtones", "wheel_highlights"]
        .iter()
        .any(|w| e.param(w).and_then(|p| p.value.as_vec2()).is_some_and(|v| v.x.abs() + v.y.abs() > 1e-3));
    assert!(moved, "{r}");
    // one undo step
    s.execute("edit.undo", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let e = q.find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri").unwrap();
    assert_eq!(e.param("wheel_midtones").and_then(|p| p.value.as_vec2()).map(|v| v.x), Some(0.0));
    // a reference inside the clip itself is refused
    let inside = s.playhead();
    assert!(s.execute("lumetri.applyMatch", json!({"referenceTime": inside.0})).is_err());
    assert!(s.execute("lumetri.applyMatch", json!({})).is_err());
}

#[test]
fn interpret_footage_and_sequence_colour_settings() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let item = s.active_sequence().unwrap().find_item(clip).unwrap().1.item;
    let sdr = mean(&s.render_program(0.125).unwrap());
    // metadata: the demo footage is SDR
    let info = s.execute("media.colorInfo", json!({"item": item.0})).unwrap();
    assert_eq!(info["override"], serde_json::Value::Null);
    assert_eq!(info["hdr"], false);
    // interpret it as S-Log3: decoded as log (lifted signal → brighter scene light), tone mapped
    let r = s.execute("clip.interpretFootage", json!({"colorSpace": "slog3-sgamut3cine"})).unwrap();
    assert_eq!(r["items"][0], item.0);
    let info = s.execute("media.colorInfo", json!({"item": item.0})).unwrap();
    assert_eq!(info["effective"], "slog3-sgamut3cine");
    assert_eq!(info["hdr"], true);
    let log = mean(&s.render_program(0.125).unwrap());
    assert!((log[0] - sdr[0]).abs() + (log[1] - sdr[1]).abs() > 0.02, "{sdr:?} vs {log:?}");
    assert!(log.iter().all(|v| *v <= 1.0 + 1e-3), "tone mapped into SDR: {log:?}");
    assert!(s.execute("clip.interpretFootage", json!({"colorSpace": "nope"})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    let back = mean(&s.render_program(0.125).unwrap());
    assert!((back[0] - sdr[0]).abs() < 1e-4, "undo restores the metadata interpretation");
    // a PQ sequence: SDR media keeps its values in working space; the monitor shows the same SDR
    let r = s.execute("sequence.colorSettings", json!({"workingSpace": "Rec. 2100 PQ"})).unwrap();
    assert_eq!(r["workingSpace"], "rec2100-pq");
    assert_eq!(s.active_sequence().unwrap().settings.working_space, "Rec. 2100 PQ");
    let work = mean(&s.render_program_working(0.125).unwrap());
    let disp = mean(&s.render_program(0.125).unwrap());
    assert!(work.iter().zip(&sdr).all(|(a, b)| (a - b).abs() < 0.08), "BT.2020 working values ≈ BT.709 SDR values: {work:?} vs {sdr:?}");
    assert!(disp.iter().zip(&sdr).all(|(a, b)| (a - b).abs() < 0.08), "display transform: {disp:?} vs {sdr:?}");
    let spaces = s.execute("color.spaces", json!({})).unwrap();
    assert_eq!(spaces["colorSpaces"].as_array().unwrap().len(), filmcraft_color::ColorSpace::ALL.len());
    assert!(s.execute("sequence.colorSettings", json!({"workingSpace": "xyz"})).is_err());
}

#[test]
fn lumetri_presets_list_apply_and_thumbnails() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let r = s.execute("lumetri.presets", json!({})).unwrap();
    assert_eq!(r["folders"], json!(["Cinematic", "Film Emulation", "Monochrome", "Technical"]));
    let mono = s.execute("lumetri.presets", json!({"folder": "monochrome"})).unwrap();
    assert!(mono["presets"].as_array().unwrap().iter().all(|p| p["folder"] == "Monochrome"));
    // apply: one undo step adding a configured Lumetri Color before the intrinsic effects
    let before = s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.len();
    let r = s.execute("lumetri.applyPreset", json!({"name": "Neutral Mono"})).unwrap();
    assert_eq!(r["clips"], 1);
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    assert_eq!(it.effects.len(), before + 1);
    let k = it.effects.iter().position(|e| e.effect == "lumetri").unwrap();
    assert!(it.effects[k + 1..].iter().all(|e| e.def().is_some_and(|d| d.intrinsic)) || k + 1 == it.effects.len());
    assert_eq!(it.effects[k].param("saturation").unwrap().value, ParamValue::Float(0.0));
    let img = s.render_program(0.25).unwrap();
    let grey = img.px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0.5).all(|p| (p[0] - p[1]).abs() < 0.01 && (p[1] - p[2]).abs() < 0.01);
    assert!(grey, "the program is black and white");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.len(), before);
    assert!(s.execute("lumetri.applyPreset", json!({"name": "No Such Look"})).is_err());
    // thumbnails of a folder, written as a PNG
    let path = tmp("lumetri-presets.png");
    let r = s.execute("lumetri.presetThumbnails", json!({"folder": "Technical", "width": 64, "columns": 3, "path": path})).unwrap();
    let n = r["presets"].as_array().unwrap().len();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(3 * 68 + 4), Some((n.div_ceil(3) * 40 + 4) as u64)));
    assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn hdr_lumetri_and_mastering_metadata_through_commands() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    s.execute("sequence.colorSettings", json!({"workingSpace": "rec2100-pq"})).unwrap();
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    let idx = s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().position(|e| e.effect == "lumetri").unwrap();
    // +1 stop doubles the light of the (SDR) clip in the PQ working space: no clipping at
    // 203 cd/m², the HDR working output keeps the values above reference white
    let base = s.render_program_working(0.25).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": idx, "param": "exposure", "value": 1.0})).unwrap();
    let up = s.render_program_working(0.25).unwrap();
    let (m0, m1) = (mean(&base), mean(&up));
    for k in 0..3 {
        assert!((m1[k] / m0[k] - 2.0).abs() < 0.08, "channel {k}: {m0:?} → {m1:?}");
    }
    assert!(up.px.as_chunks::<4>().0.iter().any(|p| p[1] > 1.2), "values above reference white survive");
    // HDR White / HDR Specular / HDR Range are ordinary Lumetri parameters
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": idx, "param": "hdr_white", "value": 2000.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": idx, "param": "hdr_specular", "value": -50.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": idx, "param": "curves_hdr_range", "value": 4000.0})).unwrap();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    assert_eq!(it.effects[idx].param("hdr_white").unwrap().value, ParamValue::Float(2000.0));
    // media.colorInfo reports the HDR metadata (none for the demo's generated media)
    let item = it.item;
    let r = s.execute("media.colorInfo", json!({"item": item.0})).unwrap();
    assert!(r["hdrMetadata"].is_null() && r["toneMapPeakNits"].is_null(), "{r}");
}
