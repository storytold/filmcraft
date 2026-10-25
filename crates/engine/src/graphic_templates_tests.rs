use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;
use filmcraft_project::graphic::LayerContent;
use filmcraft_project::{ClipId, ParamValue, TrackItem};
use filmcraft_render::graphic_clip::{item_layer_specs, layer_bounds};
use filmcraft_time::Tick;

fn demo(dir: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.prefs_path = Some(tmp_dir(dir).join("prefs.json"));
    // far after the demo clips, so graphics have free tracks around them
    s.execute("playhead.set", json!({"seconds": 600})).unwrap();
    s
}

fn item(s: &Session, clip: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(clip).unwrap().1.clone()
}

fn canvas(s: &Session) -> (u32, u32) {
    let q = s.active_sequence().unwrap();
    (q.settings.width, q.settings.height)
}

fn texts(s: &Session, clip: ClipId) -> Vec<String> {
    let it = item(s, clip);
    item_layer_specs(&it, it.source_in, canvas(s))
        .into_iter()
        .filter_map(|(_, sp)| match sp.content {
            LayerContent::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

fn clip_of(v: &Value) -> ClipId {
    ClipId(v["clip"].as_u64().unwrap())
}

#[test]
fn template_export_install_apply_round_trip() {
    let mut s = demo("gt-export");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Headline", "position": [200, 900]})).unwrap());
    s.execute("graphics.newText", json!({"text": "Subhead", "clip": c.0, "position": [200, 960], "size": 40})).unwrap();
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": c.0, "position": [400, 920], "size": [500, 150]})).unwrap();
    s.execute("graphics.arrangeLayer", json!({"clip": c.0, "layer": 2, "to": "back"})).unwrap();
    s.execute("graphics.setRoll", json!({"clip": c.0, "mode": "crawlLeft", "startOffScreen": true})).unwrap();
    let r = s
        .execute(
            "graphics.template.export",
            json!({"clip": c.0, "name": "Show Lower Third", "category": "Lower Thirds", "controls": [
                {"layer": 1, "param": "text", "name": "Headline"},
                {"layer": "Subhead", "param": "text", "name": "Subhead"},
                {"layer": 0, "param": "fill_color", "name": "Box Color"},
                {"layer": 1, "param": "size", "name": "Headline Size", "min": 20, "max": 200},
                {"layer": 2, "param": "enabled", "name": "Show Subhead"},
            ]}),
        )
        .unwrap();
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("show-lower-third.fcgt"), "{path}");
    assert_eq!(r["controls"].as_array().unwrap().len(), 5);
    assert_eq!(r["controls"][3]["kind"], "slider");
    assert_eq!(r["controls"][4]["kind"], "checkbox");
    // the file is our documented JSON format
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["format"], "filmcraft.graphicsTemplate");
    assert_eq!(file["version"], 1);
    assert_eq!(file["graphic"]["roll"]["mode"], "crawlLeft");

    // install into another user's library
    let mut s2 = demo("gt-install");
    let ins = s2.execute("graphics.template.install", json!({"path": path})).unwrap();
    assert_eq!(ins["id"], "user:show-lower-third");
    let list = s2.execute("graphics.template.list", json!({"query": "lower third", "source": "user"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let all = s2.execute("graphics.template.list", json!({})).unwrap();
    assert!(all.as_array().unwrap().len() >= 9, "built-ins and the user template");
    let undo_before = s2.history.undo.len();
    let a = s2
        .execute(
            "graphics.template.apply",
            json!({"template": "Show Lower Third", "values": {"Headline": "Breaking News", "Box Color": "#ff0000", "headline_size": 500}}),
        )
        .unwrap();
    assert_eq!(s2.history.undo.len(), undo_before + 1, "one undo step");
    let c2 = clip_of(&a);
    assert_eq!(texts(&s2, c2), vec!["Breaking News", "Subhead"]);
    let it = item(&s2, c2);
    assert_eq!(it.graphic.as_ref().unwrap().roll.mode, filmcraft_project::RollMode::CrawlLeft);
    let ctl = s2.execute("graphics.template.controls", json!({"clip": c2.0})).unwrap();
    assert_eq!(ctl["controls"][0]["value"], "Breaking News");
    assert_eq!(ctl["controls"][2]["value"], "#ff0000");
    assert_eq!(ctl["controls"][3]["value"], 200.0, "slider clamped to its range");
    s2.execute("graphics.template.set", json!({"clip": c2.0, "control": "Show Subhead", "value": false})).unwrap();
    let ctl = s2.execute("graphics.template.controls", json!({"clip": c2.0})).unwrap();
    assert_eq!(ctl["controls"][4]["value"], false);
    s2.execute("edit.undo", json!({})).unwrap();
    s2.execute("edit.undo", json!({})).unwrap();
    assert!(s2.active_sequence().unwrap().find_item(c2).is_none(), "undo removes the applied template");
    // unknown property
    assert!(s2.execute("graphics.template.apply", json!({"template": "Show Lower Third", "values": {"Nope": 1}})).is_err());
    assert!(s2.active_sequence().unwrap().video_tracks.iter().all(|t| t.items.iter().all(|i| i.name != "Show Lower Third")));
    // remove
    s2.execute("graphics.template.remove", json!({"template": "user:show-lower-third"})).unwrap();
    assert!(s2.execute("graphics.template.list", json!({"source": "user"})).unwrap().as_array().unwrap().is_empty());
}

#[test]
fn mogrt_files_are_never_read() {
    let mut s = demo("gt-mogrt");
    let d = tmp_dir("gt-mogrt-files");
    let m = d.join("fancy.mogrt");
    std::fs::write(&m, b"PK\x03\x04whatever").unwrap();
    let e = s.execute("graphics.template.install", json!({"path": m.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains(".mogrt"), "{e}");
    // a ZIP renamed to .fcgt is refused too
    let z = d.join("renamed.fcgt");
    std::fs::write(&z, b"PK\x03\x04whatever").unwrap();
    let e = s.execute("graphics.template.install", json!({"path": z.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("does not read"), "{e}");
}

#[test]
fn builtin_templates_apply_and_thumbnail() {
    let mut s = demo("gt-builtin");
    let list = s.execute("graphics.template.list", json!({"category": "Callouts"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    for e in s.execute("graphics.template.list", json!({"source": "builtin"})).unwrap().as_array().unwrap().clone() {
        let id = e["id"].as_str().unwrap();
        let r = s.execute("graphics.template.apply", json!({"template": id})).unwrap();
        assert!(!texts(&s, clip_of(&r)).is_empty(), "{id}");
        let th = s.execute("graphics.template.thumbnail", json!({"template": id, "width": 160})).unwrap();
        assert_eq!((th["width"].as_u64(), th["height"].as_u64()), (Some(160), Some(90)));
        let png = filmcraft_project::gtemplate::base64_decode(th["pngBase64"].as_str().unwrap()).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let (_, _, rgba) = super::thumbnail(&super::find_template(&s, id).unwrap().template, 160);
        assert!(rgba.chunks(4).filter(|p| p[3] > 128).count() > 50, "{id}: the thumbnail shows something");
        s.execute("playhead.set", json!({"seconds": 600 + 20 * s.history.undo.len()})).unwrap();
    }
}

#[test]
fn template_thumbnail_dimensions_fit_a_bounded_rectangle() {
    assert_eq!(super::thumbnail_dimensions([1920, 1080], 160), (160, 90));
    assert_eq!(super::thumbnail_dimensions([1, 16_384], 1920), (1, 1920));
    assert_eq!(super::thumbnail_dimensions([16_384, 1], 1920), (1920, 1));
}

#[test]
fn responsive_time_protects_intro_and_outro_when_trimmed() {
    let mut s = demo("gt-time");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Fade", "seconds": 5})).unwrap());
    let sec = |x: f64| Tick::from_seconds_f64(x);
    // opacity: 0 → 100 over the first second, 100 → 0 over the last
    s.edit_sequence("kf", |q, _, _| {
        let (_, it) = q.find_item_mut(c).unwrap();
        let (si, d) = (it.source_in, it.duration);
        let e = it.effects.iter_mut().find(|e| e.effect == "graphic_text").unwrap();
        let p = e.params.get_mut("opacity").unwrap();
        p.toggle_animation(si);
        p.set_at(si, ParamValue::Float(0.0));
        for (t, v) in [(si + sec(1.0), 100.0), (si + d - sec(1.0), 100.0), (si + d, 0.0)] {
            p.set_at(t, ParamValue::Float(v));
        }
        Ok(())
    })
    .unwrap();
    s.execute("graphics.setResponsiveTime", json!({"clip": c.0, "introSeconds": 1, "outroSeconds": 1})).unwrap();
    // opacity (0..100) at clip-relative `u` seconds, or `u` seconds before the clip end
    let op = |s: &Session, u: f64| {
        let it = item(s, c);
        let sp = item_layer_specs(&it, it.source_in + sec(u), canvas(s));
        (sp[0].1.transform.opacity * 100.0) as f64
    };
    let op_end = |s: &Session, before: f64| {
        let it = item(s, c);
        let d = it.duration;
        let sp = item_layer_specs(&it, it.source_in + d - sec(before), canvas(s));
        (sp[0].1.transform.opacity * 100.0) as f64
    };
    let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
    let d0 = item(&s, c).duration;
    assert!(near(op(&s, 0.5), 50.0));
    assert!(near(op_end(&s, 0.5), 50.0));
    // extend by 150 frames: the intro and outro keep their timing, the middle holds
    let rate = s.sequence_rate();
    s.execute("timeline.trim", json!({"clip": c.0, "edge": "out", "deltaFrames": 150})).unwrap();
    assert_eq!(item(&s, c).duration, d0 + rate.tick_of(150));
    assert!(near(op(&s, 0.5), 50.0));
    assert!(near(op(&s, 5.0), 100.0));
    assert!(near(op_end(&s, 0.5), 50.0));
    assert!(near(op_end(&s, 0.25), 25.0));
    // trim the head by 60 frames: the intro plays from the new start
    s.execute("timeline.trim", json!({"clip": c.0, "edge": "in", "deltaFrames": 60})).unwrap();
    assert!(near(op(&s, 0.0), 0.0));
    assert!(near(op(&s, 0.5), 50.0));
    assert!(near(op_end(&s, 0.5), 50.0), "the outro still ends at the clip end");
    // without responsive time the head trim would have cut the fade-in
    assert!(s.execute("graphics.setResponsiveTime", json!({"clip": c.0, "introSeconds": 30})).is_err());
}

#[test]
fn rolls_at_exact_times() {
    let mut s = demo("gt-roll");
    let c = clip_of(&s.execute("graphics.newShape", json!({"shape": "rectangle", "position": [960, 540], "size": [200, 100], "seconds": 4})).unwrap());
    s.execute("graphics.setRoll", json!({"clip": c.0, "mode": "roll", "startOffScreen": true, "endOffScreen": true, "prerollFrames": 0})).unwrap();
    let (w, h) = canvas(&s);
    let top = |s: &Session, u: Tick| {
        let it = item(s, c);
        layer_bounds(&item_layer_specs(&it, it.source_in + u, (w, h))[0].1)[1]
    };
    let d = item(&s, c).duration;
    // starts with its top on the frame bottom, ends with its bottom on the frame top: travel = h + 100
    assert!((top(&s, Tick::ZERO) - h as f64).abs() < 1e-6);
    assert!((top(&s, d) - (-100.0)).abs() < 1e-6);
    assert!((top(&s, Tick(d.0 / 4)) - (h as f64 - (h as f64 + 100.0) / 4.0)).abs() < 1e-6);
    // ease in / out
    s.execute("graphics.setRoll", json!({"clip": c.0, "easeInSeconds": 1, "easeOutSeconds": 1})).unwrap();
    let u = Tick::from_seconds_f64(0.5);
    let v = 1.0 / (d.seconds() - 1.0);
    let want = h as f64 - (h as f64 + 100.0) * v * 0.25 / 2.0;
    assert!((top(&s, u) - want).abs() < 1e-6, "{} vs {want}", top(&s, u));
    s.execute("graphics.setRoll", json!({"clip": c.0, "mode": "off"})).unwrap();
    assert!((top(&s, u) - 490.0).abs() < 1e-6);
    assert!(s.execute("graphics.setRoll", json!({"clip": c.0, "mode": "sideways"})).is_err());
}

#[test]
fn pins_follow_text_growth() {
    let mut s = demo("gt-pins");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Hi", "position": [300, 500], "size": 80})).unwrap());
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": c.0, "position": [330, 470], "size": [200, 120]})).unwrap();
    s.execute("graphics.arrangeLayer", json!({"clip": c.0, "layer": 1, "to": "back"})).unwrap();
    // box (layer 0) pinned to the text (layer 1) on all edges
    s.execute("graphics.pin", json!({"clip": c.0, "layer": 0, "to": 1})).unwrap();
    let boxes = |s: &mut Session| {
        let l = s.execute("graphics.list", json!({"clip": c.0})).unwrap();
        let q = |i: usize| {
            let v: Vec<f64> = l["layers"][i]["quad"].as_array().unwrap().iter().flat_map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]).collect();
            [v[0], v[1], v[4], v[5]]
        };
        (q(0), q(1))
    };
    let (b0, t0) = boxes(&mut s);
    let gap = [b0[0] - t0[0], b0[1] - t0[1], b0[2] - t0[2], b0[3] - t0[3]];
    s.execute("graphics.setText", json!({"clip": c.0, "layer": 1, "text": "Hello, responsive world"})).unwrap();
    let (b1, t1) = boxes(&mut s);
    assert!(t1[2] > t0[2] + 300.0, "the text grew");
    for k in 0..4 {
        assert!((b1[k] - t1[k] - gap[k]).abs() < 1e-3, "edge {k}: {b1:?} {t1:?} {gap:?}");
    }
    // move the text: the box follows; unpin
    s.execute("graphics.set", json!({"clip": c.0, "layer": 1, "props": {"position": [100, 300]}})).unwrap();
    let (b2, t2) = boxes(&mut s);
    assert!((b2[0] - t2[0] - gap[0]).abs() < 1e-3);
    s.execute("graphics.pin", json!({"clip": c.0, "layer": 0, "to": "none"})).unwrap();
    let list = s.execute("graphics.list", json!({"clip": c.0})).unwrap();
    assert!(list["layers"][0]["pin"].is_null());
    assert!(s.execute("graphics.pin", json!({"clip": c.0, "layer": 0, "to": 0})).is_err(), "not to itself");
}

#[test]
fn character_styles_follow_edits() {
    let mut s = demo("gt-chars");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Hello World"})).unwrap());
    s.execute("graphics.setCharStyle", json!({"clip": c.0, "start": 6, "end": 11, "style": {"bold": true, "color": "#ff0000", "size": 140}})).unwrap();
    let st = s.execute("graphics.list", json!({"clip": c.0})).unwrap()["layers"][0]["styles"].clone();
    assert_eq!(st, json!([{"start": 6, "end": 11, "fauxBold": true, "fill": [1.0, 0.0, 0.0, 1.0], "size": 140.0}]));
    s.execute("graphics.setText", json!({"clip": c.0, "text": "Oh, Hello World"})).unwrap();
    let st = s.execute("graphics.list", json!({"clip": c.0})).unwrap()["layers"][0]["styles"].clone();
    assert_eq!((st[0]["start"].as_u64(), st[0]["end"].as_u64()), (Some(10), Some(15)));
    // the layout uses the run: the styled word is taller
    let it = item(&s, c);
    let sp = &item_layer_specs(&it, it.source_in, canvas(&s))[0].1;
    let LayerContent::Text(t) = &sp.content else { panic!() };
    let l = filmcraft_render::graphic_clip::text_layout(t);
    assert!(l.glyphs.iter().filter(|g| g.run == 1).all(|g| g.size == 140.0 && g.synth_bold));
    s.execute("graphics.setCharStyle", json!({"clip": c.0, "clear": true})).unwrap();
    assert_eq!(s.execute("graphics.list", json!({"clip": c.0})).unwrap()["layers"][0]["styles"], json!([]));
    assert!(s.execute("graphics.setCharStyle", json!({"clip": c.0, "start": 3, "end": 3, "style": {"bold": true}})).is_err());
    assert!(s.execute("graphics.setCharStyle", json!({"clip": c.0, "style": {"sparkle": true}})).is_err());
}

#[test]
fn upgrade_caption_to_graphic() {
    let mut s = demo("gt-caption");
    s.execute("captions.add", json!({"text": "Hello <i>there</i>", "seconds": 600, "durationSeconds": 2})).unwrap();
    let caps = s.execute("captions.list", json!({})).unwrap()["tracks"][0]["captions"].clone();
    let before = caps.as_array().unwrap().len();
    let cap = caps.as_array().unwrap().iter().find(|c| c["text"] == "Hello <i>there</i>").unwrap().clone();
    let r = s.execute("graphics.upgradeCaption", json!({})).unwrap();
    assert_eq!(r["upgraded"], 1);
    let c = ClipId(r["clips"][0].as_u64().unwrap());
    assert_eq!(texts(&s, c), vec!["Hello there"]);
    let it = item(&s, c);
    assert_eq!(it.start.0, cap["start"].as_i64().unwrap());
    assert_eq!(it.end().0, cap["end"].as_i64().unwrap());
    let ex = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap().layer.as_ref().unwrap();
    assert_eq!((ex.runs[0].start, ex.runs[0].end, ex.runs[0].style.faux_italic), (6, 11, Some(true)));
    let after = s.execute("captions.list", json!({})).unwrap()["tracks"][0]["captions"].as_array().unwrap().len();
    assert_eq!(after, before - 1, "the caption became a graphic");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().find_item(c).is_none());
    assert_eq!(s.execute("captions.list", json!({})).unwrap()["tracks"][0]["captions"].as_array().unwrap().len(), before);
}

#[test]
fn source_graphics_share_edits() {
    let mut s = demo("gt-source");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Shared"})).unwrap());
    let r = s.execute("graphics.upgradeToSourceGraphic", json!({"clip": c.0})).unwrap();
    let item_id = r["item"].as_u64().unwrap();
    assert!(s.execute("graphics.upgradeToSourceGraphic", json!({"clip": c.0})).is_err());
    // a second instance from the project item
    let v1 = s.active_sequence().unwrap().find_item(c).unwrap().0;
    s.execute("timeline.place", json!({"item": item_id, "track": v1.0, "time": Tick::from_seconds_f64(700.0).0})).unwrap();
    let c2 = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| &t.items).find(|i| i.item.0 == item_id && i.id != c).unwrap().id;
    assert_eq!(texts(&s, c2), vec!["Shared"]);
    s.execute("graphics.setText", json!({"clip": c2.0, "text": "Edited once"})).unwrap();
    assert_eq!(texts(&s, c), vec!["Edited once"], "the other instance follows");
    s.execute("graphics.set", json!({"clip": c.0, "props": {"size": 60}})).unwrap();
    let size = |s: &Session, cl: ClipId| item(s, cl).effects.iter().find(|e| e.effect == "graphic_text").unwrap().params["size"].value.clone();
    assert_eq!(size(&s, c2), ParamValue::Float(60.0));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(size(&s, c2), size(&s, c), "undo restores both");
    assert_ne!(size(&s, c2), ParamValue::Float(60.0));
    // new plain graphics do not reuse the source graphic's item
    let c3 = clip_of(&s.execute("graphics.newText", json!({"text": "Plain", "seconds": 1, "time": Tick::from_seconds_f64(800.0).0})).unwrap());
    assert_ne!(item(&s, c3).item.0, item_id);
}

#[test]
fn replace_fonts_in_project() {
    let mut s = demo("gt-fonts");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Serif", "font": "Noto Serif"})).unwrap());
    s.execute("graphics.setCharStyle", json!({"clip": c.0, "start": 0, "end": 2, "style": {"font": "Noto Serif"}})).unwrap();
    let used = s.execute("graphics.fonts.used", json!({})).unwrap();
    assert!(used.as_array().unwrap().iter().any(|f| f["family"] == "Noto Serif" && f["missing"] == false), "{used}");
    let r = s.execute("file.replaceFonts", json!({"from": "Noto Serif", "to": "JetBrains Mono"})).unwrap();
    assert_eq!(r["replaced"], 2);
    let used = s.execute("graphics.fonts.used", json!({})).unwrap();
    assert!(!used.as_array().unwrap().iter().any(|f| f["family"] == "Noto Serif"));
    s.execute("edit.undo", json!({})).unwrap();
    let used = s.execute("graphics.fonts.used", json!({})).unwrap();
    assert!(used.as_array().unwrap().iter().any(|f| f["family"] == "Noto Serif"));
}

#[test]
fn caption_formats_through_the_engine() {
    let mut s = demo("gt-capfmt");
    s.execute("captions.add", json!({"text": "One", "seconds": 1, "durationSeconds": 2})).unwrap();
    s.execute("captions.add", json!({"text": "Two\nlines", "seconds": 4, "durationSeconds": 1})).unwrap();
    let d = tmp_dir("gt-capfmt-files");
    let want = s.execute("captions.list", json!({})).unwrap()["tracks"][0]["captions"].clone();
    for ext in ["mcc", "stl", "ttml", "dfxp"] {
        let path = d.join(format!("caps.{ext}")).to_string_lossy().to_string();
        let r = s.execute("captions.export", json!({"path": path})).unwrap();
        assert_eq!(r["captions"], 2, "{ext}");
        let imp = s.execute("captions.import", json!({"path": path})).unwrap();
        assert_eq!(imp["captions"], 2, "{ext}: {imp}");
        let got = s.execute("captions.list", json!({"track": "C1"})).unwrap()["tracks"][0]["captions"].clone();
        for i in 0..2 {
            assert_eq!(got[i]["text"], want[i]["text"], "{ext}");
            assert_eq!(got[i]["in"], want[i]["in"], "{ext}");
            assert_eq!(got[i]["out"], want[i]["out"], "{ext}");
        }
    }
}

/// #188: a copied Graphic clip pastes at the playhead as a second Graphic clip.
#[test]
fn copy_paste_duplicates_a_graphic_clip() {
    let mut s = demo("gt-paste");
    let c = clip_of(&s.execute("graphics.newText", json!({"text": "Lyric", "seconds": 2})).unwrap());
    s.execute("timeline.select", json!({"clips": [c.0]})).unwrap();
    let v = s.execute("edit.copy", json!({})).unwrap();
    assert_eq!(v["copied"], 1, "{v}");
    s.execute("playhead.set", json!({"seconds": 620})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let pasted: Vec<&TrackItem> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| i.id != c && i.item == item(&s, c).item).collect();
    assert_eq!(pasted.len(), 1, "one copy");
    assert!((pasted[0].start.seconds() - 620.0).abs() < 0.05, "at the playhead: {:?}", pasted[0].start);
    assert_eq!(texts(&s, pasted[0].id), vec!["Lyric".to_string()]);
}

#[test]
fn character_style_drag_merges_only_its_target_and_range() {
    let mut s = demo("gt-char-drag");
    let c = clip_of(&s.execute("graphics.newText", json!({"text":"Alpha Bravo"})).unwrap());
    let before = s.project.clone();
    let n = s.history.undo.len();
    for i in 0..8 {
        s.execute("graphics.setCharStyle", json!({"clip":c.0,"start":0,"end":5,"style":{"size":105+i*5},"merge":true,"begin":i==0})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1, "one drag, one undo step");
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
    s.execute("graphics.setCharStyle", json!({"clip":c.0,"start":0,"end":5,"style":{"size":150},"merge":true,"begin":true})).unwrap();
    assert_eq!(s.history.undo.len(), n + 2, "a new gesture on the same range starts a new step");
    s.execute("graphics.setCharStyle", json!({"clip":c.0,"start":6,"end":11,"style":{"size":160},"merge":true})).unwrap();
    assert_eq!(s.history.undo.len(), n + 3, "another character range does not merge");
    let before = s.project.clone();
    assert!(s.execute("graphics.setCharStyle", json!({"clip":c.0,"start":3,"end":3,"style":{"size":170},"merge":true})).is_err());
    assert_eq!(*s.project, *before);
    assert_eq!(s.history.undo.len(), n + 3, "an invalid edit adds no undo step");
}

#[test]
fn character_style_merge_preserves_default_calls_and_separate_clips() {
    let mut s = demo("gt-char-merge-targets");
    let a = clip_of(&s.execute("graphics.newText", json!({"text":"First", "newClip":true})).unwrap());
    let b = clip_of(&s.execute("graphics.newText", json!({"text":"Other", "newClip":true})).unwrap());
    assert_ne!(a, b);
    let n = s.history.undo.len();
    for c in [a, b] {
        s.execute("graphics.setCharStyle", json!({"clip":c.0,"start":0,"end":5,"style":{"size":120},"merge":true})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 2, "distinct clips do not merge");
    for size in [130, 140] {
        s.execute("graphics.setCharStyle", json!({"clip":b.0,"style":{"size":size}})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 4, "default calls retain separate undo steps");
    let added = s.execute("graphics.newText", json!({"clip":b.0,"text":"Layer"})).unwrap();
    assert_eq!(clip_of(&added), b);
    let n = s.history.undo.len();
    for layer in [0, 1] {
        s.execute("graphics.setCharStyle", json!({"clip":b.0,"layer":layer,"start":0,"end":5,"style":{"size":150},"merge":true})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 2, "distinct layers do not merge");
}
