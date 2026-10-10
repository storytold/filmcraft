use serde_json::json;

use crate::Session;
use filmcraft_project::graphic::{LayerContent, eval_layer, layer_indices};
use filmcraft_project::{ClipId, ItemKind};
use filmcraft_time::Tick;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn layers(s: &Session, clip: ClipId) -> Vec<filmcraft_project::graphic::LayerSpec> {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    layer_indices(&it.effects).into_iter().filter_map(|i| eval_layer(&it.effects[i], Tick::ZERO, (q.settings.width, q.settings.height))).collect()
}

fn text_of(l: &filmcraft_project::graphic::LayerSpec) -> String {
    match &l.content {
        LayerContent::Text(t) => t.text.clone(),
        _ => panic!("not text"),
    }
}

#[test]
fn graphic_duration_does_not_set_its_timeline_position() {
    for command in ["graphics.newText", "graphics.newShape"] {
        for playhead_frame in [0, 72] {
            for seconds in [1, 2] {
                let mut s = Session::default();
                s.execute("file.newSequence", json!({"name": "Placement", "fps": 24, "width": 128, "height": 128, "video": 1, "audio": 1})).unwrap();
                s.execute("playhead.set", json!({"frame": playhead_frame})).unwrap();
                let start = s.playhead();
                let r = s.execute(command, json!({"seconds": seconds})).unwrap();
                let clip = ClipId(r["clip"].as_u64().unwrap());
                let q = s.active_sequence().unwrap();
                let (_, it) = q.find_item(clip).unwrap();
                assert_eq!(it.start, start, "{command}, duration {seconds}, playhead {playhead_frame}");
                assert_eq!(it.duration, Tick::from_seconds_f64(f64::from(seconds)));
                s.execute("edit.undo", json!({})).unwrap();
                assert!(s.active_sequence().unwrap().find_item(clip).is_none());
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.start, start);
            }
        }
    }
}

#[test]
fn graphic_duration_keeps_explicit_time_frame_and_timecode_placement() {
    for command in ["graphics.newText", "graphics.newShape"] {
        for mut params in [json!({"time": 0}), json!({"frame": 0}), json!({"timecode": "00:00:00:00"})] {
            let mut s = Session::default();
            s.execute("file.newSequence", json!({"name": "Placement", "fps": 24, "width": 128, "height": 128, "video": 1, "audio": 1})).unwrap();
            s.execute("playhead.set", json!({"frame": 72})).unwrap();
            params["seconds"] = json!(2);
            let r = s.execute(command, params).unwrap();
            let clip = ClipId(r["clip"].as_u64().unwrap());
            let (_, it) = s.active_sequence().unwrap().find_item(clip).unwrap();
            assert_eq!(it.start, Tick::ZERO, "{command}");
            assert_eq!(it.duration, Tick::from_seconds_f64(2.0));
        }
    }
}

/// A new graphic goes on the track and at the time it is given, for a shape as for text; a track
/// that is not free there is an error named after the command that asked.
#[test]
fn graphic_placement_takes_a_track_and_a_time() {
    let commands =
        [("graphics.newText", json!({})), ("graphics.newShape", json!({})), ("graphics.newRectangle", json!({})), ("graphics.newPolygon", json!({"sides": 5}))];
    for (command, mut params) in commands {
        let mut s = Session::default();
        s.execute("file.newSequence", json!({"name": "Placement", "fps": 24, "width": 128, "height": 128, "video": 3, "audio": 1})).unwrap();
        s.execute("playhead.set", json!({"frame": 72})).unwrap();
        params["track"] = json!(2);
        params["frame"] = json!(24);
        params["seconds"] = json!(1);
        let r = s.execute(command, params.clone()).unwrap();
        let clip = ClipId(r["clip"].as_u64().unwrap());
        let q = s.active_sequence().unwrap();
        let (track, it) = q.find_item(clip).unwrap();
        assert_eq!(track, q.video_tracks[2].id, "{command}");
        assert_eq!((it.start, it.duration), (Tick::from_seconds_f64(1.0), Tick::from_seconds_f64(1.0)), "{command}");
        // the same place again is taken
        let e = s.execute(command, params).unwrap_err().to_string();
        assert!(e.contains("V3 is not free here"), "{command}: {e}");
        let named = if command == "graphics.newText" { "graphics.newText" } else { "graphics.newShape" };
        assert!(e.contains(named), "{command}: {e}");
        assert_eq!(s.active_sequence().unwrap().video_tracks[2].items.len(), 1, "{command}");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().find_item(clip).is_none(), "{command}");
    }
}

#[test]
fn new_text_makes_a_graphic_clip_above_the_footage() {
    let mut s = demo();
    let t = s.playhead();
    let r = s.execute("graphics.newText", json!({"text": "Hello", "position": [200, 300]})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (tid, it) = q.find_item(clip).unwrap();
    let track_index = q.video_tracks.iter().position(|tr| tr.id == tid).unwrap();
    let below_busy = q.video_tracks[..track_index].iter().any(|tr| tr.item_at(t).is_some());
    assert!(below_busy || track_index == 0, "placed above the clip under the playhead");
    assert_eq!(it.name, "Hello");
    assert!(matches!(s.project.item(it.item).unwrap().kind, ItemKind::Graphic { .. }));
    assert_eq!(it.duration, q.settings.frame_rate.snap_nearest(Tick::from_seconds_f64(5.0)));
    assert!(!it.has_standard_effects(), "layers are not fx");
    assert_eq!(s.state.selection, vec![clip]);
    let ls = layers(&s, clip);
    assert_eq!(ls.len(), 1);
    assert_eq!(text_of(&ls[0]), "Hello");
    assert_eq!((ls[0].transform.position.x, ls[0].transform.position.y), (200.0, 300.0));

    // a second text layer in the same clip, then a shape
    let r2 = s.execute("graphics.newText", json!({"text": "World", "clip": clip.0})).unwrap();
    assert_eq!(r2["layer"], 1);
    s.execute("graphics.newShape", json!({"shape": "ellipse", "clip": clip.0, "size": [100, 50]})).unwrap();
    assert_eq!(layers(&s, clip).len(), 3);
    let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
    assert_eq!(list["layers"][2]["kind"], "Ellipse");
    assert_eq!(list["layers"][1]["name"], "World");

    // undo removes the shape layer again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layers(&s, clip).len(), 2);
}

#[test]
fn typing_coalesces_into_one_undo_step_and_props_set() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": ""})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let before = s.history.undo.len();
    for (i, t) in ["T", "Ti", "Tit", "Titl", "Title"].iter().enumerate() {
        s.execute("graphics.setText", json!({"clip": clip.0, "layer": 0, "text": t, "merge": i > 0})).unwrap();
    }
    assert_eq!(s.history.undo.len(), before + 1, "one undo step for the typing session");
    assert_eq!(text_of(&layers(&s, clip)[0]), "Title");
    s.execute("graphics.set", json!({"clip": clip.0, "props": {"font_style": "Bold", "fontSize": 140, "align": "center", "fill_color": "#ffcc00", "stroke": true, "caps": "small caps"}})).unwrap();
    let l = &layers(&s, clip)[0];
    let LayerContent::Text(t) = &l.content else { panic!() };
    assert_eq!((t.style.as_str(), t.size, t.align, t.caps), ("Bold", 140.0, 1, 2));
    assert_eq!(l.appearance.strokes.len(), 1);
    assert!((l.appearance.fill.unwrap()[1] - 0.8).abs() < 0.01);
    assert!(s.execute("graphics.set", json!({"clip": clip.0, "props": {"nope": 1}})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(text_of(&layers(&s, clip)[0]), "");
}

#[test]
fn align_and_distribute_layers() {
    let mut s = demo();
    let (w, h) = {
        let q = s.active_sequence().unwrap();
        (q.settings.width as f64, q.settings.height as f64)
    };
    let r = s.execute("graphics.newShape", json!({"shape": "rectangle", "position": [300, 200], "size": [100, 60]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": clip, "position": [500, 400], "size": [40, 40]})).unwrap();
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": clip, "position": [1500, 900], "size": [40, 40]})).unwrap();
    s.execute("graphics.align", json!({"clip": clip, "layers": [0], "align": "left"})).unwrap();
    s.execute("graphics.align", json!({"clip": clip, "layers": [0], "align": "bottom"})).unwrap();
    let l = layers(&s, ClipId(clip));
    assert!((l[0].transform.position.x - 50.0).abs() < 1e-6, "left edge on the frame edge");
    assert!((l[0].transform.position.y - (h - 30.0)).abs() < 1e-6);
    s.execute("graphics.align", json!({"clip": clip, "layers": [1, 2], "align": "vcenter", "to": "selection"})).unwrap();
    let l = layers(&s, ClipId(clip));
    assert!((l[1].transform.position.y - l[2].transform.position.y).abs() < 1e-6);
    s.execute("graphics.distribute", json!({"clip": clip, "layers": [0, 1, 2], "axis": "horizontal"})).unwrap();
    let l = layers(&s, ClipId(clip));
    let xs: Vec<f64> = l.iter().map(|x| x.transform.position.x).collect();
    assert!(((xs[1] - xs[0]) - (xs[2] - xs[1])).abs() < 1e-6, "{xs:?}");
    let _ = w;
}

#[test]
fn linear_gradient_fill_sets_and_evaluates() {
    let mut s = demo();
    let r = s.execute("graphics.newShape", json!({"shape": "rectangle", "position": [200, 200], "size": [80, 40]})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    s.execute(
        "graphics.set",
        json!({"clip": clip.0, "props": {"fill_kind": "linear gradient", "gradient_start": "#ff0000", "gradient_end": "#0000ff", "gradient_angle": 0}}),
    )
    .unwrap();
    let g = layers(&s, clip)[0].appearance.gradient.clone().expect("linear fill");
    assert_eq!(g.start[0], 1.0);
    assert!(g.start[1] < 0.01 && g.end[2] > 0.9 && g.end[0] < 0.01, "{g:?}");
    assert_eq!(g.angle, 0.0);
    s.execute("graphics.set", json!({"clip": clip.0, "props": {"fill_kind": "solid"}})).unwrap();
    assert!(layers(&s, clip)[0].appearance.gradient.is_none());
}

#[test]
fn graphic_renders_in_the_program_and_survives_save() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": "BIG", "size": 300, "position": [100, 500]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.set", json!({"clip": clip, "props": {"fill_color": "#ff0000"}})).unwrap();
    let img = s.render_program(0.25).unwrap();
    let red = img.px.chunks(4).filter(|p| p[0] > 0.9 && p[1] < 0.05 && p[2] < 0.05).count();
    assert!(red > 200, "red text in the program: {red}");
    let dir = std::env::temp_dir().join(format!("fc-gfx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("g.fcproj");
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    let mut s2 = Session::default();
    s2.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(s2.project.items, s.project.items, "graphic clips round-trip");
    let _ = std::fs::remove_dir_all(&dir);
    let fonts = s.execute("fonts.list", json!({"system": false})).unwrap();
    assert!(fonts.as_array().unwrap().iter().any(|f| f["family"] == "Inter"));
}

/// `file.exportInterchange` wrote a title out of the document without saying so: the command's
/// report names every graphic clip a format cannot carry.
#[test]
fn interchange_export_reports_the_title_it_cannot_carry() {
    let d = std::env::temp_dir().join(format!("fc-title-report-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": "Opening line", "time": 0, "seconds": 2.0})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let (track, it) = s.active_sequence().unwrap().find_item(clip).map(|(t, i)| (t, i.clone())).unwrap();
    let frame = s.sequence_rate().frame_at(it.start);
    let want = format!("graphic clip \"{}\" at frame {frame}", it.name);
    for format in ["xml", "fcpxml", "otio", "aaf"] {
        let path = d.join(format!("cut.{format}")).to_string_lossy().to_string();
        let r = s.execute("file.exportInterchange", json!({"format": format, "path": path})).unwrap();
        let named = r["report"].as_array().unwrap().iter().filter(|l| l.as_str().is_some_and(|l| l.starts_with(&want))).count();
        assert_eq!(named, 1, "{format}: {}", r["report"]);
    }
    // an EDL holds one video track: the report names the title when its track is the one written
    let q = s.active_sequence().unwrap();
    assert_ne!(q.video_tracks[0].id, track, "the title sits above the footage");
    let path = d.join("cut.edl").to_string_lossy().to_string();
    let r = s.execute("file.exportInterchange", json!({"format": "edl", "path": path})).unwrap();
    assert!(r["report"].to_string().contains("one video track"), "{}", r["report"]);
    let _ = std::fs::remove_dir_all(&d);
}

fn quad_box(s: &mut Session, clip: u64, layer: usize) -> [f64; 4] {
    let l = s.execute("graphics.list", json!({"clip": clip})).unwrap();
    let q = l["layers"][layer]["quad"].as_array().unwrap().clone();
    let xs: Vec<f64> = q.iter().map(|p| p[0].as_f64().unwrap()).collect();
    let ys: Vec<f64> = q.iter().map(|p| p[1].as_f64().unwrap()).collect();
    [
        xs.iter().copied().fold(f64::MAX, f64::min),
        ys.iter().copied().fold(f64::MAX, f64::min),
        xs.iter().copied().fold(f64::MIN, f64::max),
        ys.iter().copied().fold(f64::MIN, f64::max),
    ]
}

fn three_shapes(s: &mut Session) -> u64 {
    let r = s.execute("graphics.newRectangle", json!({"position": [300, 200], "size": [100, 60]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.newEllipse", json!({"clip": clip, "position": [500, 400], "size": [40, 40]})).unwrap();
    s.execute("graphics.newPolygon", json!({"clip": clip, "position": [1500, 900], "size": [80, 80], "sides": 5})).unwrap();
    clip
}

#[test]
fn new_layer_menu_items_make_shapes_and_vertical_text() {
    let mut s = demo();
    let clip = three_shapes(&mut s);
    let list = s.execute("graphics.list", json!({"clip": clip})).unwrap();
    let kinds: Vec<&str> = list["layers"].as_array().unwrap().iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Rectangle", "Ellipse", "Polygon"]);
    let l = layers(&s, ClipId(clip));
    let LayerContent::Shape(p) = &l[2].content else { panic!() };
    assert_eq!(p.sides, 5, "sides set");
    // the polygon (with its side count) is one undo step
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layers(&s, ClipId(clip)).len(), 2);
    // vertical text: taller than wide
    let r = s.execute("graphics.newVerticalText", json!({"text": "TALL", "newClip": true})).unwrap();
    let v = r["clip"].as_u64().unwrap();
    let LayerContent::Text(t) = &layers(&s, ClipId(v))[0].content else { panic!() };
    assert!(t.vertical);
    let b = quad_box(&mut s, v, 0);
    assert!(b[3] - b[1] > 2.5 * (b[2] - b[0]), "vertical text is tall: {b:?}");
    // the menu has the New Layer entries and no generic "Shape"
    let ids: Vec<&str> = crate::command_specs().iter().filter(|c| c.menu == ["Graphics and Titles", "New Layer"]).map(|c| c.id).collect();
    assert_eq!(
        ids,
        ["graphics.newText", "graphics.newVerticalText", "graphics.newRectangle", "graphics.newEllipse", "graphics.newPolygon", "graphics.newFromFile"]
    );
}

#[test]
fn align_menu_commands_frame_group_and_selection() {
    let mut s = demo();
    let (w, h) = {
        let q = s.active_sequence().unwrap();
        (q.settings.width as f64, q.settings.height as f64)
    };
    let clip = three_shapes(&mut s);
    // one layer: align to the video frame
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0]})).unwrap();
    assert!(s.execute("graphics.alignGroup.left", json!({})).is_err(), "group align needs two layers");
    s.execute("graphics.alignFrame.right", json!({})).unwrap();
    let b = quad_box(&mut s, clip, 0);
    assert!((b[2] - w).abs() < 1e-6, "{b:?}");
    s.execute("graphics.alignFrame.vcenter", json!({})).unwrap();
    let b = quad_box(&mut s, clip, 0);
    assert!(((b[1] + b[3]) / 2.0 - h / 2.0).abs() < 1e-6);
    // group: the union moves, relative positions kept
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [1, 2]})).unwrap();
    let (b1, b2) = (quad_box(&mut s, clip, 1), quad_box(&mut s, clip, 2));
    s.execute("graphics.alignGroup.top", json!({})).unwrap();
    let (c1, c2) = (quad_box(&mut s, clip, 1), quad_box(&mut s, clip, 2));
    assert!(c1[1].abs() < 1e-6, "topmost layer on the frame top: {c1:?}");
    assert!(((c2[1] - c1[1]) - (b2[1] - b1[1])).abs() < 1e-6, "relative offset kept");
    // each frame-aligned: both on the left edge individually
    s.execute("graphics.alignFrame.left", json!({})).unwrap();
    assert!(quad_box(&mut s, clip, 1)[0].abs() < 1e-6 && quad_box(&mut s, clip, 2)[0].abs() < 1e-6);
    // to selection: bottoms meet the lowest bottom
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0, 1, 2]})).unwrap();
    let lowest = (0..3).map(|i| quad_box(&mut s, clip, i)[3]).fold(f64::MIN, f64::max);
    s.execute("graphics.alignSelection.bottom", json!({})).unwrap();
    for i in 0..3 {
        assert!((quad_box(&mut s, clip, i)[3] - lowest).abs() < 1e-6);
    }
    // undo restores
    s.execute("edit.undo", json!({})).unwrap();
    assert!((0..3).any(|i| (quad_box(&mut s, clip, i)[3] - lowest).abs() > 1.0));
}

#[test]
fn distribute_centres_and_spaces() {
    let mut s = demo();
    let r = s.execute("graphics.newRectangle", json!({"position": [100, 500], "size": [100, 40]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.newRectangle", json!({"clip": clip, "position": [300, 500], "size": [20, 40]})).unwrap();
    s.execute("graphics.newRectangle", json!({"clip": clip, "position": [1000, 500], "size": [300, 40]})).unwrap();
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0, 1]})).unwrap();
    assert!(s.execute("graphics.distributeHorizontally", json!({})).is_err(), "needs three");
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0, 1, 2]})).unwrap();
    s.execute("graphics.distributeSpaceHorizontally", json!({})).unwrap();
    let b: Vec<[f64; 4]> = (0..3).map(|i| quad_box(&mut s, clip, i)).collect();
    let (g1, g2) = (b[1][0] - b[0][2], b[2][0] - b[1][2]);
    assert!((g1 - g2).abs() < 1e-6, "equal gaps {g1} {g2}");
    s.execute("graphics.distributeHorizontally", json!({})).unwrap();
    let c: Vec<f64> = (0..3).map(|i| quad_box(&mut s, clip, i)).map(|b| (b[0] + b[2]) / 2.0).collect();
    assert!(((c[1] - c[0]) - (c[2] - c[1])).abs() < 1e-6, "{c:?}");
    // vertical variants move only y
    s.execute("graphics.newRectangle", json!({"clip": clip, "position": [800, 100], "size": [40, 40]})).unwrap();
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0, 1, 3]})).unwrap();
    let x0 = quad_box(&mut s, clip, 3)[0];
    s.execute("graphics.distributeVertically", json!({})).unwrap();
    s.execute("graphics.distributeSpaceVertically", json!({})).unwrap();
    assert!((quad_box(&mut s, clip, 3)[0] - x0).abs() < 1e-6);
}

#[test]
fn arrange_select_and_reset() {
    let mut s = demo();
    let clip = three_shapes(&mut s);
    let kind = |s: &mut Session, i: usize| s.execute("graphics.list", json!({"clip": clip})).unwrap()["layers"][i]["kind"].as_str().unwrap().to_string();
    s.execute("graphics.selectLayer", json!({"clip": clip, "layers": [0]})).unwrap();
    s.execute("graphics.bringToFront", json!({})).unwrap();
    assert_eq!(kind(&mut s, 2), "Rectangle");
    assert_eq!(s.state.graphic_layers, vec![2], "selection follows the layer");
    s.execute("graphics.sendBackward", json!({})).unwrap();
    assert_eq!(kind(&mut s, 1), "Rectangle");
    s.execute("graphics.sendToBack", json!({})).unwrap();
    assert_eq!(kind(&mut s, 0), "Rectangle");
    s.execute("graphics.bringForward", json!({})).unwrap();
    assert_eq!(kind(&mut s, 1), "Rectangle");
    // menu shortcuts
    let spec = |id: &str| crate::command_specs().iter().find(|c| c.id == id).unwrap();
    assert_eq!(spec("graphics.bringToFront").shortcut, Some("Cmd+Shift+]"));
    assert_eq!(spec("graphics.sendBackward").shortcut, Some("Cmd+["));
    // select next / previous layer cycles
    s.execute("graphics.selectNextLayer", json!({})).unwrap();
    assert_eq!(s.state.graphic_layers, vec![2]);
    s.execute("graphics.selectNextLayer", json!({})).unwrap();
    assert_eq!(s.state.graphic_layers, vec![0]);
    s.execute("graphics.selectPreviousLayer", json!({})).unwrap();
    assert_eq!(s.state.graphic_layers, vec![2]);
    // reset all parameters of the selected layer: position back to the centre, content kept
    s.execute("graphics.set", json!({"clip": clip, "layer": 2, "props": {"rotation": 30, "opacity": 50}})).unwrap();
    s.execute("graphics.resetAllParameters", json!({})).unwrap();
    let l = &layers(&s, ClipId(clip))[2];
    let (w, h) = {
        let q = s.active_sequence().unwrap();
        (q.settings.width as f64, q.settings.height as f64)
    };
    assert_eq!((l.transform.rotation, l.transform.opacity), (0.0, 1.0));
    assert_eq!((l.transform.position.x, l.transform.position.y), (w / 2.0, h / 2.0));
    let LayerContent::Shape(sh) = &l.content else { panic!() };
    assert_eq!(sh.shape, 2, "still a polygon");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layers(&s, ClipId(clip))[2].transform.rotation, 30.0);
}

#[test]
fn select_next_graphic_and_reset_duration() {
    let mut s = demo();
    s.execute("playhead.set", json!({"seconds": 1})).unwrap();
    let a = s.execute("graphics.newText", json!({"text": "A"})).unwrap()["clip"].as_u64().unwrap();
    s.execute("playhead.set", json!({"seconds": 8})).unwrap();
    let b = s.execute("graphics.newText", json!({"text": "B", "seconds": 2})).unwrap()["clip"].as_u64().unwrap();
    s.execute("graphics.selectPreviousGraphic", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![ClipId(a)]);
    assert!(s.playhead() < Tick::from_seconds_f64(6.0), "playhead moved onto the graphic");
    s.execute("graphics.selectNextGraphic", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![ClipId(b)]);
    assert!(s.execute("graphics.selectNextGraphic", json!({})).is_err(), "no next graphic");
    // reset duration: 2 s → 5 s
    let r = s.execute("graphics.resetDuration", json!({})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    assert_eq!(Tick(r["duration"].as_i64().unwrap()), rate.snap_nearest(Tick::from_seconds_f64(5.0)));
    s.execute("edit.undo", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.find_item(ClipId(b)).unwrap().1.duration, rate.snap_nearest(Tick::from_seconds_f64(2.0)));
}

#[test]
fn new_layer_from_file_places_the_image_above() {
    let mut s = demo();
    let dir = crate::media_test_util::tmp_dir("gfx-file");
    // a 4×2 24-bit BMP
    let (w, h) = (4u32, 2u32);
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = Vec::new();
    b.extend(b"BM");
    b.extend(size.to_le_bytes());
    b.extend([0u8; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend((w as i32).to_le_bytes());
    b.extend((h as i32).to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0u8; 24]);
    for _ in 0..h {
        for _ in 0..w {
            b.extend([0u8, 0, 255]);
        }
        b.resize(b.len() + (row - w * 3) as usize, 0);
    }
    let path = dir.join("logo.bmp");
    std::fs::write(&path, &b).unwrap();
    let t = s.playhead();
    let r = s.execute("graphics.newFromFile", json!({"path": path.to_string_lossy()})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (tid, it) = q.find_item(clip).unwrap();
    assert_eq!(it.start, q.settings.frame_rate.snap(t));
    let ti = q.video_tracks.iter().position(|tr| tr.id == tid).unwrap();
    assert!(q.video_tracks[..ti].iter().any(|tr| tr.item_at(t).is_some()), "above the footage");
    // the anchor is the picture's centre, not the frame's, so `position` places the picture
    let anchor = it.effect("motion").unwrap().vec2_at("anchor", Tick::ZERO);
    assert_eq!((anchor.x, anchor.y), (w as f64 / 2.0, h as f64 / 2.0));
    assert_eq!(s.state.selection, vec![clip]);
    assert!(s.execute("graphics.newFromFile", json!({})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where each line of text layer `layer` starts on the canvas (the left end of its baseline),
/// with the line's text.
fn line_starts(s: &Session, clip: ClipId, layer: usize) -> Vec<(String, f64, f64)> {
    let spec = layers(s, clip).remove(layer);
    let LayerContent::Text(t) = &spec.content else { panic!("not text") };
    let l = filmcraft_render::graphic_clip::text_layout(t);
    let m = filmcraft_render::graphic_clip::layer_matrix(&spec);
    l.lines
        .iter()
        .map(|line| {
            let p = m.apply(filmcraft_geom::Vec2::new(line.x as f64, line.baseline as f64));
            (t.text[line.range.clone()].trim_end().to_string(), p.x, p.y)
        })
        .collect()
}

fn same_places(a: &[(String, f64, f64)], b: &[(String, f64, f64)]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.0, y.0);
        assert!((x.1 - y.1).abs() < 0.05 && (x.2 - y.2).abs() < 0.05, "line `{}` moved: {x:?} -> {y:?}", x.0);
    }
}

const FOX: &str = "The quick brown fox jumps over the lazy dog";

#[test]
fn paragraph_text_wraps_in_its_box_and_hides_what_does_not_fit() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": FOX, "position": [270, 583], "box": [500, 150], "size": 60})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
    let l = &list["layers"][0];
    assert_eq!(l["textType"], "paragraph");
    assert_eq!(l["box"], json!([500.0, 150.0]));
    assert_eq!(l["overflow"], true, "three lines of 60 px text do not fit 150 px");
    assert_eq!(l["localBounds"], json!([0.0, 0.0, 500.0, 150.0]), "the box is the layer's bounds");
    assert_eq!(l["quad"][0], json!([270.0, 583.0]), "the position is the box's top-left corner");
    assert_eq!(l["quad"][2], json!([770.0, 733.0]));
    let shown = line_starts(&s, clip, 0);
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(shown.iter().all(|(_, x, _)| (*x - 270.0).abs() < 8.0), "lines start at the box's left edge: {shown:?}");

    // a taller box shows everything; the font size and scale are untouched
    s.execute("graphics.set", json!({"clip": clip.0, "props": {"boxHeight": 400}})).unwrap();
    let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
    assert_eq!(list["layers"][0]["overflow"], false);
    assert_eq!(list["layers"][0]["scale"], 100.0);
    assert!(line_starts(&s, clip, 0).len() >= 3);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.execute("graphics.list", json!({"clip": clip.0})).unwrap()["layers"][0]["overflow"], true);

    // point text has no box, and a vertical layer never gets one
    let r = s.execute("graphics.newText", json!({"text": "Title", "newClip": true, "track": 3})).unwrap();
    let point = s.execute("graphics.list", json!({"clip": r["clip"]})).unwrap();
    assert_eq!(point["layers"][0]["textType"], "point");
    assert_eq!(point["layers"][0]["box"], json!(null));
    let r = s.execute("graphics.newText", json!({"text": "Tate", "vertical": true, "box": [300, 300], "newClip": true, "track": 4})).unwrap();
    assert_eq!(s.execute("graphics.list", json!({"clip": r["clip"]})).unwrap()["layers"][0]["textType"], "point");
}

#[test]
fn text_type_converts_both_ways_and_keeps_the_text_in_place() {
    for align in ["left", "center", "right"] {
        let mut s = demo();
        let r = s.execute("graphics.newText", json!({"text": FOX, "position": [270, 400], "box": [500, 150], "size": 60})).unwrap();
        let clip = ClipId(r["clip"].as_u64().unwrap());
        // scaled and turned, so that the anchor arithmetic matters
        s.execute("graphics.set", json!({"clip": clip.0, "props": {"align": align, "scale": 150, "scale_width": 150, "rotation": 20}})).unwrap();
        let hidden = line_starts(&s, clip, 0);
        assert_eq!(hidden.len(), 2, "the box hides the third line");
        s.execute("graphics.set", json!({"clip": clip.0, "props": {"boxHeight": 0}})).unwrap();
        let before = line_starts(&s, clip, 0);
        assert!(before.len() >= 3);
        s.execute("edit.undo", json!({})).unwrap();
        let position = layers(&s, clip)[0].transform.position;

        // paragraph → point: every line stays where it was, also the hidden one
        let r = s.execute("graphics.setTextType", json!({"clip": clip.0, "type": "point"})).unwrap();
        assert_eq!(r, json!({"clip": clip.0, "layer": 0, "type": "point", "changed": true}));
        let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
        assert_eq!(list["layers"][0]["textType"], "point", "{align}");
        let spec = layers(&s, clip).remove(0);
        assert_eq!(text_of(&spec).lines().count(), before.len(), "the wrapped lines became real lines: {:?}", text_of(&spec));
        assert_eq!(text_of(&spec).replace('\n', " "), FOX, "nothing but the line breaks changed");
        assert_eq!(spec.transform.position, position, "the layer's position is untouched ({align})");
        assert!(spec.transform.anchor.y < -1.0, "the anchor, still on the box's old corner, is above the first baseline: {:?}", spec.transform.anchor);
        same_places(&before, &line_starts(&s, clip, 0));
        // again: nothing to do
        assert_eq!(s.execute("graphics.setTextType", json!({"clip": clip.0, "type": "Point Text"})).unwrap()["changed"], false);

        // point → paragraph: a box fitted to the text, which still does not move
        s.execute("graphics.setTextType", json!({"clip": clip.0, "type": "paragraph"})).unwrap();
        let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
        let l = &list["layers"][0];
        assert_eq!(l["textType"], "paragraph");
        assert_eq!(l["overflow"], false, "the fitted box shows every line");
        let widest = 500.0;
        assert!(l["box"][0].as_f64().unwrap() <= widest + 2.0 && l["box"][0].as_f64().unwrap() > 200.0, "{}", l["box"]);
        let spec = layers(&s, clip).remove(0);
        assert_eq!(spec.transform.position, position);
        // the anchor is level with the box's top again; the fitted box is narrower than the old
        // one, so centred and right-aligned text leave the anchor to its left
        let a = spec.transform.anchor;
        assert!(a.y.abs() < 1e-3 && a.x < 1e-3 && (align != "left" || a.x.abs() < 1e-3), "{align}: {a:?}");
        same_places(&before, &line_starts(&s, clip, 0));

        // each conversion is one undo step
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.execute("graphics.list", json!({"clip": clip.0})).unwrap()["layers"][0]["textType"], "point");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.execute("graphics.list", json!({"clip": clip.0})).unwrap()["layers"][0]["textType"], "paragraph");
        same_places(&hidden, &line_starts(&s, clip, 0));
        s.execute("edit.redo", json!({})).unwrap();
        same_places(&before, &line_starts(&s, clip, 0));
    }
}

#[test]
fn text_type_refuses_what_it_cannot_convert() {
    let mut s = Session::default();
    assert!(s.execute("graphics.setTextType", json!({"type": "point"})).is_err(), "no sequence");
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": "Title"})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    assert!(s.execute("graphics.setTextType", json!({"clip": clip})).is_err(), "no type");
    assert!(s.execute("graphics.setTextType", json!({"clip": clip, "type": "wavy"})).is_err());
    s.execute("graphics.newShape", json!({"shape": "ellipse", "clip": clip})).unwrap();
    let e = s.execute("graphics.setTextType", json!({"clip": clip, "layer": 1, "type": "paragraph"})).unwrap_err();
    assert!(e.to_string().contains("not a text layer"), "{e}");
    s.execute("graphics.newText", json!({"text": "Tate", "vertical": true, "clip": clip})).unwrap();
    let e = s.execute("graphics.setTextType", json!({"clip": clip, "layer": 2, "type": "paragraph"})).unwrap_err();
    assert!(e.to_string().contains("vertical"), "{e}");
    // a point-text layer keeps its own line breaks when it gets a box
    s.execute("graphics.setText", json!({"clip": clip, "layer": 0, "text": "one\ntwo words\n\nfour"})).unwrap();
    let before = line_starts(&s, ClipId(clip), 0);
    s.execute("graphics.setTextType", json!({"clip": clip, "layer": 0, "type": "paragraph"})).unwrap();
    same_places(&before, &line_starts(&s, ClipId(clip), 0));
    s.execute("graphics.setTextType", json!({"clip": clip, "layer": 0, "type": "point"})).unwrap();
    assert_eq!(text_of(&layers(&s, ClipId(clip))[0]), "one\ntwo words\n\nfour");
}

#[test]
fn a_layer_saved_before_the_box_height_existed_can_still_get_one() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": FOX, "box": [500, 150], "size": 60})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    s.edit_sequence("old file", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).unwrap();
        let ei = layer_indices(&it.effects)[0];
        it.effects[ei].params.remove("box_height");
        Ok(())
    })
    .unwrap();
    let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
    assert_eq!(list["layers"][0]["box"], json!([500.0, 0.0]), "as tall as its text");
    assert_eq!(list["layers"][0]["overflow"], false);
    s.execute("graphics.set", json!({"clip": clip.0, "props": {"box_height": 100}})).unwrap();
    assert_eq!(s.execute("graphics.list", json!({"clip": clip.0})).unwrap()["layers"][0]["box"], json!([500.0, 100.0]));
}
