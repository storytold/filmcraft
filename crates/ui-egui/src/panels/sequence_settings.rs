//! Sequence ▸ Sequence Settings…, laid out as Premiere Pro's dialog: General, Color Management and
//! VR Properties tabs, Cancel / OK.
//!
//! File ▸ New ▸ Sequence… opens the same dialog as New Sequence ([`open_new`]): a Sequence Presets
//! tab (the named presets of `sequence.presets`, grouped HD / UHD 4K / DCI 4K / 6K; choosing one
//! sets the General tab's frame size, timebase and pixel aspect ratio, `preset.<name>`), the
//! General tab starting from the default settings, and a Tracks tab with the number of video and
//! audio tracks (`tracks.video`, `tracks.audio`); OK runs `file.newSequence`.
//!
//! What it changes (one `sequence.settings` command, one undo step): Timebase, Frame Size (with
//! "Scale motion effects proportionally when changing frame size", on by default), Pixel Aspect
//! Ratio (the ratios Interpret Footage offers, or Custom: two numbers), drop-frame or
//! non-drop-frame timecode for the NTSC rates, audio Channel Format and Sample Rate, Maximum Render
//! Quality, and the Color Management tab (working colour space, wide gamut, auto tone map).
//!
//! Premiere settings FilmCraft has no equivalent for yet are shown greyed out with their current
//! value and a "not supported yet" hint, rather than editable and silently ignored: Editing Mode,
//! Fields, the Frames and Feet + Frames display
//! formats, Number of Channels, audio Display Format, the Video Previews format, codec and size,
//! Maximum Bit Depth, and the VR Properties tab. Composite in Linear Color is shown on and greyed:
//! FilmCraft always composites in linear light.
//!
//! Save Preset (both dialogs) keeps the frame size, pixel aspect ratio, timebase, fields, audio
//! settings and track layout as a custom preset (`sequence.preset.save`, in the user's data
//! folder); New Sequence lists the custom presets first, in a Custom group, each with Rename and
//! Delete (`sequence.preset.rename` / `.delete`).
//!
//! Automation ids (`sequenceSettings.` + …): `tab.presets`, `tab.general`, `tab.tracks`, `tab.color`,
//! `tab.vr`; Sequence Presets: `preset.<name>` (+ `presetGroup.<group>`, the custom presets in
//! `presetGroup.Custom` with `presetRename.<name>` and `presetDelete.<name>`; while renaming
//! `presetRename.text` and `presetRename.ok`, while deleting `presetDelete.confirm` and
//! `presetDelete.cancel`); Save Preset: `savePreset`, then `savePreset.name`, `savePreset.save`,
//! `savePreset.cancel` (and `savePreset.replace` when the name is taken); Tracks:
//! `tracks.video`, `tracks.audio`, `tracks.type` (+ `tracks.type.option.<type>`); General:
//! `name`, `editingMode`, `timebase` (+ `timebase.option.<num>/<den>` while open), `width`, `height`,
//! `aspect`, `scaleMotion`, `par` (+ `par.option.<num>x<den>` and `par.option.custom` while open;
//! `par.num`, `par.den` on Custom), `fields`, `videoDisplay` (+ `videoDisplay.option.<df|ndf|tc>`),
//! `channelFormat` (+ `channelFormat.option.<Stereo|Mono|5.1|Adaptive>`), `channels`, `sampleRate`
//! (+ `sampleRate.option.<hz>`), `audioDisplay`, `previewFormat`, `previewCodec`, `previewWidth`,
//! `previewHeight`, `maxBitDepth`, `maxRenderQuality`, `linearColor`; Color Management:
//! `workingSpace` (+ `workingSpace.option.<id>`), `wideGamut`, `autoToneMap`; VR Properties:
//! `vr.projection`, `vr.layout`, `vr.horizontal`, `vr.vertical`; `ok`, `cancel`.

use filmcraft_color::WorkingSpace;
use filmcraft_project::{AudioChannels, SequenceSettings};
use filmcraft_time::FrameRate;
use serde_json::{Map, Value, json};

use filmcraft_engine::sequence_presets::CustomSequencePreset;

use crate::FilmcraftApp;
use crate::i18n::t;
use crate::state::SequenceSettingsDraft;

const NOT_YET: &str = "Not supported in FilmCraft yet";
const LINEAR: &str = "FilmCraft always composites in linear light";

/// The Timebase list (frames per second as num/den), Premiere's common rates.
const TIMEBASES: [(i64, i64); 15] = [
    (10, 1),
    (12, 1),
    (25, 2),
    (15, 1),
    (24_000, 1001),
    (24, 1),
    (25, 1),
    (30_000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60_000, 1001),
    (60, 1),
    (120_000, 1001),
    (120, 1),
];
const SAMPLE_RATES: [u32; 5] = [32_000, 44_100, 48_000, 88_200, 96_000];
const MIXES: [&str; 4] = ["Stereo", "Mono", "5.1", "Adaptive"];
const TRACK_TYPES: [&str; 4] = ["Standard", "Mono", "5.1", "Adaptive"];

/// The Tracks tab's name for the type of `tracks` (all the same, else Standard).
fn track_type_of(tracks: &[filmcraft_project::Track]) -> &'static str {
    match tracks.first().map(|t| t.channels) {
        Some(c) if c != AudioChannels::Stereo && tracks.iter().all(|t| t.channels == c) => mix_name(c),
        _ => "Standard",
    }
}

fn mix_name(c: AudioChannels) -> &'static str {
    match c {
        AudioChannels::Mono => "Mono",
        AudioChannels::Stereo => "Stereo",
        AudioChannels::Surround51 => "5.1",
        AudioChannels::Adaptive => "Adaptive",
    }
}

/// Fill the draft from the active sequence and open the dialog on the General tab.
pub fn open(app: &mut FilmcraftApp) {
    let Some(q) = app.session.active_sequence() else { return };
    let st = &q.settings;
    let name = app.session.state.active_sequence.and_then(|id| app.session.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
    app.ui.sequence_settings = SequenceSettingsDraft {
        tab: "general".into(),
        name,
        new_sequence: false,
        // the track layout Save Preset keeps
        video_tracks: q.video_tracks.len().min(256) as u32,
        audio_tracks: q.audio_tracks.len().min(256) as u32,
        track_type: track_type_of(&q.audio_tracks).into(),
        save_name: None,
        save_replace: false,
        renaming: None,
        deleting: None,
        fps_num: st.frame_rate.num,
        fps_den: st.frame_rate.den,
        width: st.width,
        height: st.height,
        par: filmcraft_project::sane_par(st.par),
        par_custom: !is_standard_par(st.par),
        preset: String::new(),
        scale_motion: true,
        drop_frame: st.drop_frame && st.frame_rate.supports_drop_frame(),
        mix: mix_name(st.audio_master).into(),
        sample_rate: st.sample_rate,
        max_render_quality: st.max_render_quality,
        working_space: st.color.working.id().into(),
        wide_gamut: st.color.wide_gamut,
        auto_tone_map: st.color.auto_tone_map,
    };
    app.dialog = Some(crate::Dialog::SequenceSettings);
}

/// File ▸ New ▸ Sequence…: the dialog as New Sequence, from the default settings, named like
/// `file.newSequence` would name it, with three video and three audio tracks.
pub fn open_new(app: &mut FilmcraftApp) {
    let st = SequenceSettings::default();
    let n = app.session.project.sequences().count() + 1;
    app.ui.sequence_settings = SequenceSettingsDraft {
        tab: "presets".into(),
        new_sequence: true,
        video_tracks: 3,
        audio_tracks: 3,
        track_type: "Standard".into(),
        save_name: None,
        save_replace: false,
        renaming: None,
        deleting: None,
        fps_num: st.frame_rate.num,
        fps_den: st.frame_rate.den,
        width: st.width,
        height: st.height,
        par: st.par,
        par_custom: false,
        // the defaults are the HD 1080p 23.976 preset
        preset: filmcraft_project::sequence_preset_of(&st).map(|p| p.name.to_string()).unwrap_or_default(),
        scale_motion: true,
        drop_frame: st.drop_frame && st.frame_rate.supports_drop_frame(),
        mix: mix_name(st.audio_master).into(),
        sample_rate: st.sample_rate,
        max_render_quality: st.max_render_quality,
        working_space: st.color.working.id().into(),
        wide_gamut: st.color.wide_gamut,
        auto_tone_map: st.color.auto_tone_map,
        name: format!("Sequence {n:02}"),
    };
    app.dialog = Some(crate::Dialog::SequenceSettings);
}

/// The `file.newSequence` parameters for a New Sequence draft. Settings `file.newSequence` doesn't
/// take (drop-frame timecode, Maximum Render Quality) are the second map, for `sequence.settings`
/// on the new sequence; empty when they are the defaults.
fn new_sequence_params(d: &SequenceSettingsDraft) -> (Map<String, Value>, Map<String, Value>) {
    let mut p = Map::new();
    let name = d.name.trim();
    if !name.is_empty() {
        p.insert("name".into(), json!(name));
    }
    // the chosen preset first (it carries the pixel aspect ratio and the name); the picture after it
    if !d.preset.is_empty() {
        p.insert("preset".into(), json!(d.preset));
    }
    p.insert("width".into(), json!(d.width));
    p.insert("height".into(), json!(d.height));
    p.insert("fps".into(), json!(rate(d).as_f64()));
    // square pixels are the default; a preset brings its own ratio
    let preset_par = filmcraft_project::sequence_preset(&d.preset).map_or((1, 1), |p| p.par);
    if filmcraft_project::checked_par(d.par) != Some(preset_par) {
        p.insert("pixelAspect".into(), json!([d.par.0, d.par.1]));
    }
    p.insert("sampleRate".into(), json!(d.sample_rate));
    p.insert("mix".into(), json!(d.mix));
    p.insert("video".into(), json!(d.video_tracks.min(256)));
    p.insert("audio".into(), json!(d.audio_tracks.min(256)));
    if d.track_type != "Standard" {
        p.insert("trackType".into(), json!(d.track_type));
    }
    // what's left: only the settings `file.newSequence` has no parameter for (a preset already
    // brings its timecode format)
    let base = filmcraft_project::sequence_preset(&d.preset).map(|p| p.settings()).unwrap_or_default();
    let after = SequenceSettings { frame_rate: rate(d), width: d.width, height: d.height, sample_rate: d.sample_rate, ..base };
    let mut rest = changes(d, &after, name);
    rest.retain(|k, _| matches!(k.as_str(), "dropFrame" | "maxRenderQuality"));
    (p, rest)
}

/// The `sequence.preset.save` parameters for the draft: everything a preset keeps.
fn preset_params(d: &SequenceSettingsDraft, name: &str, replace: bool) -> Value {
    json!({
        "name": name.trim(), "replace": replace, "width": d.width, "height": d.height, "fps": rate(d).as_f64(),
        "pixelAspect": [d.par.0, d.par.1], "dropFrame": d.drop_frame && rate(d).supports_drop_frame(), "fields": "progressive",
        "sampleRate": d.sample_rate, "mix": d.mix, "video": d.video_tracks.min(256), "audio": d.audio_tracks.min(256),
        "trackType": d.track_type,
    })
}

/// Choose the custom preset `p`: the draft takes all its settings.
fn choose_custom(d: &mut SequenceSettingsDraft, p: &CustomSequencePreset) {
    d.preset = p.name.clone();
    d.width = p.width;
    d.height = p.height;
    d.fps_num = p.frame_rate.num;
    d.fps_den = p.frame_rate.den;
    d.par = filmcraft_project::sane_par(p.pixel_aspect);
    d.par_custom = !is_standard_par(d.par);
    d.drop_frame = p.drop_frame && p.frame_rate.supports_drop_frame();
    d.sample_rate = p.sample_rate;
    d.mix = mix_name(p.audio_master).into();
    d.video_tracks = p.video_tracks;
    d.audio_tracks = p.audio_tracks;
    d.track_type = p.audio_track_type.map_or("Standard", mix_name).into();
}

/// The Custom group of the Sequence Presets tab: the user's presets, each with Rename and
/// Delete. The commands to run go to `actions`.
fn custom_presets(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, customs: &[CustomSequencePreset], elems: &mut Elems, actions: &mut Actions) {
    let header = egui::CollapsingHeader::new(t(filmcraft_engine::sequence_presets::CUSTOM_GROUP))
        .id_salt(("seq-preset-group", filmcraft_engine::sequence_presets::CUSTOM_GROUP))
        .default_open(true)
        .show(ui, |ui| {
            for p in customs {
                ui.horizontal(|ui| {
                    if let Some((old, new)) = d.renaming.as_mut().filter(|(old, _)| *old == p.name) {
                        let r = ui.add(egui::TextEdit::singleline(new).desired_width(220.0));
                        elems.push(("sequenceSettings.presetRename.text".into(), r.rect, new.clone()));
                        let ok = ui.button(tl!("OK"));
                        elems.push(("sequenceSettings.presetRename.ok".into(), ok.rect, "OK".into()));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ok.clicked() || enter {
                            actions.push(("sequence.preset.rename", json!({"name": old, "to": new.trim()})));
                        }
                        let c = ui.button(tl!("Cancel"));
                        elems.push(("sequenceSettings.presetRename.cancel".into(), c.rect, "Cancel".into()));
                        if c.clicked() {
                            d.renaming = None;
                        }
                        return;
                    }
                    let r = ui.selectable_label(d.preset == p.name, &p.name);
                    elems.push((format!("sequenceSettings.preset.{}", p.name), r.rect, p.name.clone()));
                    if r.clicked() {
                        choose_custom(d, p);
                    }
                    if d.deleting.as_deref() == Some(p.name.as_str()) {
                        ui.label(tl!("Delete this preset?"));
                        let y = ui.button(tl!("Delete"));
                        elems.push(("sequenceSettings.presetDelete.confirm".into(), y.rect, "Delete".into()));
                        if y.clicked() {
                            actions.push(("sequence.preset.delete", json!({"name": p.name})));
                        }
                        let n = ui.button(tl!("Cancel"));
                        elems.push(("sequenceSettings.presetDelete.cancel".into(), n.rect, "Cancel".into()));
                        if n.clicked() {
                            d.deleting = None;
                        }
                        return;
                    }
                    let rn = ui.small_button(tl!("Rename"));
                    elems.push((format!("sequenceSettings.presetRename.{}", p.name), rn.rect, "Rename".into()));
                    if rn.clicked() {
                        d.renaming = Some((p.name.clone(), p.name.clone()));
                        d.deleting = None;
                    }
                    let del = ui.small_button(tl!("Delete"));
                    elems.push((format!("sequenceSettings.presetDelete.{}", p.name), del.rect, "Delete".into()));
                    if del.clicked() {
                        d.deleting = Some(p.name.clone());
                        d.renaming = None;
                    }
                });
            }
        });
    elems.push((format!("sequenceSettings.presetGroup.{}", filmcraft_engine::sequence_presets::CUSTOM_GROUP), header.header_response.rect, "Custom".into()));
}

/// The Save Preset row: a name field, Save and Cancel; Replace when the name is taken. The
/// command to run goes to `actions`.
fn save_preset_row(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, customs: &[CustomSequencePreset], elems: &mut Elems, actions: &mut Actions) {
    let Some(mut name) = d.save_name.clone() else { return };
    ui.horizontal(|ui| {
        ui.label(tl!("Preset name:"));
        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(240.0));
        elems.push(("sequenceSettings.savePreset.name".into(), r.rect, name.clone()));
        if r.changed() {
            d.save_replace = false;
        }
        let s = ui.add_enabled(!name.trim().is_empty(), egui::Button::new(tl!("Save")));
        elems.push(("sequenceSettings.savePreset.save".into(), s.rect, "Save".into()));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty();
        if s.clicked() || enter {
            if customs.iter().any(|p| p.name.trim().to_lowercase() == name.trim().to_lowercase()) {
                d.save_replace = true;
            } else {
                actions.push(("sequence.preset.save", preset_params(d, &name, false)));
            }
        }
        let c = ui.button(tl!("Cancel"));
        elems.push(("sequenceSettings.savePreset.cancel".into(), c.rect, "Cancel".into()));
        if c.clicked() {
            d.save_name = None;
            d.save_replace = false;
            return;
        }
        d.save_name = Some(name.clone());
    });
    if d.save_replace {
        ui.horizontal(|ui| {
            ui.label(tlf!("A preset called {name} exists.", name = name.trim()));
            let r = ui.button(tl!("Replace"));
            elems.push(("sequenceSettings.savePreset.replace".into(), r.rect, "Replace".into()));
            if r.clicked() {
                actions.push(("sequence.preset.save", preset_params(d, &name, true)));
            }
        });
    }
}

/// The Sequence Presets tab of New Sequence: the named presets by group; choosing one sets the
/// draft's frame size, timebase and pixel aspect ratio (the General tab shows them).
fn presets(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, customs: &[CustomSequencePreset], elems: &mut Elems, actions: &mut Actions) {
    let presets = filmcraft_project::sequence_presets_cached();
    let chosen_group = presets.iter().find(|p| p.name == d.preset).map(|p| p.group);
    let mut groups: Vec<&str> = Vec::new();
    for p in presets {
        if !groups.contains(&p.group) {
            groups.push(p.group);
        }
    }
    ui.label(tl!("Available Presets:"));
    if !customs.is_empty() {
        custom_presets(ui, d, customs, elems, actions);
    }
    for group in groups {
        let open = chosen_group == Some(group);
        let header = egui::CollapsingHeader::new(t(group)).id_salt(("seq-preset-group", group)).default_open(open).show(ui, |ui| {
            for p in presets.iter().filter(|p| p.group == group) {
                let r = ui.selectable_label(d.preset == p.name, p.name);
                elems.push((format!("sequenceSettings.preset.{}", p.name), r.rect, p.name.to_string()));
                if r.clicked() {
                    d.preset = p.name.to_string();
                    d.width = p.width;
                    d.height = p.height;
                    d.fps_num = p.frame_rate.num;
                    d.fps_den = p.frame_rate.den;
                    d.par = p.par;
                    d.par_custom = !is_standard_par(p.par);
                    d.drop_frame = p.frame_rate.supports_drop_frame();
                }
            }
        });
        elems.push((format!("sequenceSettings.presetGroup.{group}"), header.header_response.rect, group.to_string()));
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("Preset Description")).strong());
    if let Some(p) = customs.iter().find(|p| p.name == d.preset) {
        let shown = tlf!(
            "{w} x {h}, {rate} fps, {par}; {hz} Hz {mix}; {v} video and {a} audio tracks",
            w = p.width,
            h = p.height,
            rate = p.frame_rate.label(),
            par = par_label(p.pixel_aspect),
            hz = p.sample_rate,
            mix = t(mix_name(p.audio_master)),
            v = p.video_tracks,
            a = p.audio_tracks
        );
        let r = ui.label(&shown);
        elems.push(("sequenceSettings.presetDescription".into(), r.rect, shown));
        return;
    }
    match presets.iter().find(|p| p.name == d.preset) {
        Some(p) => {
            let (dw, _) = filmcraft_project::conformed_size((p.width, p.height), p.par, (1, 1));
            let shown = if p.par == (1, 1) {
                tlf!("{w} x {h}, {rate} fps, square pixels", w = p.width, h = p.height, rate = p.frame_rate.label())
            } else {
                tlf!(
                    "{w} x {h}, {rate} fps, {par}; shown {dw} x {h} ({aspect})",
                    w = p.width,
                    h = p.height,
                    rate = p.frame_rate.label(),
                    par = filmcraft_project::pixel_aspect_label(p.par),
                    dw = dw.round() as u64,
                    aspect = aspect(dw.round().clamp(0.0, f64::from(u32::MAX)) as u32, p.height)
                )
            };
            let r = ui.label(&shown);
            elems.push(("sequenceSettings.presetDescription".into(), r.rect, shown));
        }
        None => {
            let r = ui.label(tl!("Custom settings (see the General tab)"));
            elems.push(("sequenceSettings.presetDescription".into(), r.rect, "Custom".into()));
        }
    }
}

/// The Tracks tab of New Sequence: how many video and audio tracks the sequence starts with.
fn tracks(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, elems: &mut Elems) {
    for (id, label, n) in [("video", "Video:", &mut d.video_tracks), ("audio", "Audio:", &mut d.audio_tracks)] {
        ui.horizontal(|ui| {
            ui.label(t(label));
            let r = ui.add(crate::widgets::number_drag(n).range(0..=256));
            elems.push((format!("sequenceSettings.tracks.{id}"), r.rect, n.to_string()));
            ui.label(tl!("tracks"));
        });
    }
    ui.horizontal(|ui| {
        ui.label(tl!("Track type"));
        let shown = d.track_type.clone();
        let resp = egui::ComboBox::from_id_salt("seq-settings-track-type").selected_text(t(&shown)).width(120.0).show_ui(ui, |ui| {
            for ty in TRACK_TYPES {
                let o = ui.selectable_value(&mut d.track_type, ty.to_string(), t(ty));
                elems.push((format!("sequenceSettings.tracks.type.option.{ty}"), o.rect, ty.to_string()));
            }
        });
        elems.push(("sequenceSettings.tracks.type".into(), resp.response.rect, shown));
    });
}

fn rate(d: &SequenceSettingsDraft) -> FrameRate {
    FrameRate { num: d.fps_num, den: d.fps_den }.sane()
}

/// "16:9" for a frame size, or "1.90:1" when the reduced ratio has no small integer form.
fn aspect(w: u32, h: u32) -> String {
    let g = gcd(w, h).max(1);
    let (a, b) = (w / g, h / g);
    if a <= 64 && b <= 64 {
        format!("{a}:{b}")
    } else if h > 0 {
        format!("{:.2}:1", f64::from(w) / f64::from(h))
    } else {
        String::new()
    }
}

/// The width a `w` x `h` frame with `par` pixels shows at, in square pixels (rounded).
fn display_width(w: u32, h: u32, par: (u32, u32)) -> u32 {
    let dw = filmcraft_project::conformed_size((w, h), par, (1, 1)).0.round();
    if dw >= f64::from(u32::MAX) { u32::MAX } else { dw.max(0.0) as u32 }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn timebase_label(r: FrameRate) -> String {
    tlf!("{rate} frames/second", rate = r.label())
}

/// The Video Display Format choices for a timebase: drop-frame / non-drop-frame timecode for the
/// NTSC rates, plain timecode otherwise (`(id, label)`).
fn timecode_formats(r: FrameRate) -> Vec<(&'static str, String)> {
    if r.supports_drop_frame() {
        vec![("df", tlf!("{rate} fps Drop-Frame Timecode", rate = r.label())), ("ndf", tlf!("{rate} fps Non Drop-Frame Timecode", rate = r.label()))]
    } else {
        vec![("tc", tlf!("{rate} fps Timecode", rate = r.label()))]
    }
}

fn par_label(par: (u32, u32)) -> String {
    t(&filmcraft_project::pixel_aspect_label(par)).to_string()
}

/// Whether `par` is one of the ratios the Pixel Aspect Ratio list names.
fn is_standard_par(par: (u32, u32)) -> bool {
    filmcraft_project::checked_par(par).is_some_and(|par| filmcraft_project::STANDARD_PIXEL_ASPECTS.iter().any(|(_, p)| *p == par))
}

/// The Pixel Aspect Ratio row: the named ratios (the list Interpret Footage offers) and Custom,
/// which shows two number fields, the width and the height of a pixel.
fn par_row(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, list_w: f32, elems: &mut Elems) {
    let shown = if d.par_custom { tlf!("Custom ({ratio})", ratio = format!("{:.4}", filmcraft_project::par_ratio(d.par))) } else { par_label(d.par) };
    let resp = egui::ComboBox::from_id_salt("seq-settings-par").selected_text(&shown).width(list_w).height(f32::INFINITY).show_ui(ui, |ui| {
        for (label, par) in filmcraft_project::STANDARD_PIXEL_ASPECTS {
            let o = ui.selectable_label(!d.par_custom && filmcraft_project::sane_par(d.par) == par, t(label));
            if o.clicked() {
                d.par = par;
                d.par_custom = false;
            }
            elems.push((format!("sequenceSettings.par.option.{}x{}", par.0, par.1), o.rect, label.to_string()));
        }
        let o = ui.selectable_label(d.par_custom, tl!("Custom…"));
        if o.clicked() {
            d.par_custom = true;
        }
        elems.push(("sequenceSettings.par.option.custom".into(), o.rect, "Custom…".into()));
    });
    elems.push(("sequenceSettings.par".into(), resp.response.rect, shown));
    if d.par_custom {
        let n = ui.add(crate::widgets::number_drag(&mut d.par.0).range(1..=10_000).speed(1.0));
        elems.push(("sequenceSettings.par.num".into(), n.rect, d.par.0.to_string()));
        ui.label(":");
        let m = ui.add(crate::widgets::number_drag(&mut d.par.1).range(1..=10_000).speed(1.0));
        elems.push(("sequenceSettings.par.den".into(), m.rect, d.par.1.to_string()));
    }
}

/// The parameters for `sequence.settings` that differ from `cur` (and the sequence's name
/// `cur_name`); empty when nothing changed. A blank name keeps the current one.
fn changes(d: &SequenceSettingsDraft, cur: &SequenceSettings, cur_name: &str) -> Map<String, Value> {
    let mut p = Map::new();
    let name = d.name.trim();
    if !name.is_empty() && name != cur_name {
        p.insert("name".into(), json!(name));
    }
    let r = rate(d);
    if r != cur.frame_rate {
        p.insert("fps".into(), json!(r.as_f64()));
    }
    if (d.width, d.height) != (cur.width, cur.height) {
        p.insert("width".into(), json!(d.width));
        p.insert("height".into(), json!(d.height));
        p.insert("scaleMotion".into(), json!(d.scale_motion));
    }
    // compared in lowest terms (8:6 is the 4:3 the sequence has); a ratio that makes no sense
    // is sent as it is, for the command to refuse
    if filmcraft_project::checked_par(d.par) != Some(filmcraft_project::sane_par(cur.par)) {
        p.insert("pixelAspect".into(), json!([d.par.0, d.par.1]));
    }
    let df = d.drop_frame && r.supports_drop_frame();
    if df != (cur.drop_frame && cur.frame_rate.supports_drop_frame()) {
        p.insert("dropFrame".into(), json!(df));
    }
    if d.mix != mix_name(cur.audio_master) {
        p.insert("mix".into(), json!(d.mix));
    }
    if d.sample_rate != cur.sample_rate {
        p.insert("sampleRate".into(), json!(d.sample_rate));
    }
    if d.max_render_quality != cur.max_render_quality {
        p.insert("maxRenderQuality".into(), json!(d.max_render_quality));
    }
    if d.working_space != cur.color.working.id() {
        p.insert("workingSpace".into(), json!(d.working_space));
    }
    if d.wide_gamut != cur.color.wide_gamut {
        p.insert("wideGamut".into(), json!(d.wide_gamut));
    }
    if d.auto_tone_map != cur.color.auto_tone_map {
        p.insert("autoToneMap".into(), json!(d.auto_tone_map));
    }
    p
}

type Elems = Vec<(String, egui::Rect, String)>;
/// Preset commands a frame of the dialog asks for (run after the window is drawn).
type Actions = Vec<(&'static str, Value)>;

/// One labelled row: the label right-aligned in a `label_w` column, then the control.
fn row<R>(ui: &mut egui::Ui, label_w: f32, label: &str, enabled: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let h = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(egui::vec2(label_w, h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled(enabled, egui::Label::new(t(label)));
        });
        add(ui)
    })
    .inner
}

/// A greyed-out list showing `value`, with the "not supported yet" hint (or `hint`).
fn fixed_combo(ui: &mut egui::Ui, elems: &mut Elems, id: &str, value: &str, width: f32, hint: &str) {
    let r = ui
        .add_enabled_ui(false, |ui| egui::ComboBox::from_id_salt(("seq-settings-fixed", id)).selected_text(t(value)).width(width).show_ui(ui, |_| {}).response)
        .inner
        .on_disabled_hover_text(t(hint));
    elems.push((format!("sequenceSettings.{id}"), r.rect, value.to_string()));
}

/// A greyed-out read-only value (a number field in Premiere), with the hint.
fn fixed_value(ui: &mut egui::Ui, elems: &mut Elems, id: &str, value: &str) {
    let r = ui.add_enabled(false, egui::Button::new(value).min_size(egui::vec2(64.0, 0.0))).on_disabled_hover_text(t(NOT_YET));
    elems.push((format!("sequenceSettings.{id}"), r.rect, value.to_string()));
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(t(title)).strong());
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn general(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, cur: &SequenceSettings, audio_samples: bool, elems: &mut Elems) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let label_w = ["Preview File Format:", "Pixel Aspect Ratio:", "Number of Channels:"]
        .iter()
        .map(|l| ui.painter().layout_no_wrap(t(l).to_string(), font.clone(), egui::Color32::WHITE).size().x)
        .fold(0.0, f32::max);
    let list_w = 260.0;
    row(ui, label_w, "Sequence Name:", true, |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(list_w));
        elems.push(("sequenceSettings.name".into(), r.rect, d.name.clone()));
    });
    // the preset the picture still matches (New Sequence: the chosen one), else Custom
    let picture = SequenceSettings { width: d.width, height: d.height, frame_rate: rate(d), par: d.par, ..Default::default() };
    let mode = match filmcraft_project::sequence_preset_of(&picture) {
        Some(p) if !d.new_sequence || d.preset == p.name || d.preset.is_empty() => p.name.to_string(),
        Some(_) if d.new_sequence => {
            filmcraft_project::sequence_preset(&d.preset).filter(|p| p.matches(&picture)).map_or("Custom".to_string(), |p| p.name.to_string())
        }
        _ => "Custom".to_string(),
    };
    let hint = if d.new_sequence { "Choose a preset on the Sequence Presets tab" } else { NOT_YET };
    row(ui, label_w, "Editing Mode:", false, |ui| fixed_combo(ui, elems, "editingMode", &mode, list_w, hint));
    row(ui, label_w, "Timebase:", true, |ui| {
        let r = rate(d);
        let mut choices: Vec<(i64, i64)> = TIMEBASES.to_vec();
        if !choices.contains(&(r.num, r.den)) {
            choices.insert(0, (r.num, r.den));
        }
        let shown = timebase_label(r);
        // no row limit of our own: the list grows to egui's popup limit (`Spacing::default_area_size`,
        // 400 px), so every rate up to 119.88 shows without scrolling (the default 200 px stopped
        // before 29.97)
        let resp = egui::ComboBox::from_id_salt("seq-settings-timebase").selected_text(&shown).width(list_w).height(f32::INFINITY).show_ui(ui, |ui| {
            for (num, den) in choices {
                let label = timebase_label(FrameRate { num, den });
                let o = ui.selectable_label((d.fps_num, d.fps_den) == (num, den), &label);
                // a new timebase starts on its usual timecode (drop-frame for the NTSC rates);
                // picking the current one again keeps the Display Format chosen for it
                if o.clicked() && (d.fps_num, d.fps_den) != (num, den) {
                    d.fps_num = num;
                    d.fps_den = den;
                    d.drop_frame = FrameRate { num, den }.supports_drop_frame();
                }
                elems.push((format!("sequenceSettings.timebase.option.{num}/{den}"), o.rect, label));
            }
        });
        elems.push(("sequenceSettings.timebase".into(), resp.response.rect, shown));
    });
    section(ui, "Video", |ui| {
        row(ui, label_w, "Frame Size:", true, |ui| {
            let w = ui.add(crate::widgets::number_drag(&mut d.width).range(1..=filmcraft_project::MAX_FRAME_SIDE).speed(1.0));
            elems.push(("sequenceSettings.width".into(), w.rect, d.width.to_string()));
            ui.label(tl!("horizontal"));
            let h = ui.add(crate::widgets::number_drag(&mut d.height).range(1..=filmcraft_project::MAX_FRAME_SIDE).speed(1.0));
            elems.push(("sequenceSettings.height".into(), h.rect, d.height.to_string()));
            ui.label(tl!("vertical"));
            // the display aspect: 1440 x 1080 with 4:3 pixels is 16:9
            let a = aspect(display_width(d.width, d.height, d.par), d.height);
            let ar = ui.label(&a);
            elems.push(("sequenceSettings.aspect".into(), ar.rect, a));
        });
        let r = ui.checkbox(&mut d.scale_motion, tl!("Scale motion effects proportionally when changing frame size"));
        elems.push(("sequenceSettings.scaleMotion".into(), r.rect, d.scale_motion.to_string()));
        row(ui, label_w, "Pixel Aspect Ratio:", true, |ui| par_row(ui, d, list_w, elems));
        row(ui, label_w, "Fields:", false, |ui| fixed_combo(ui, elems, "fields", "No Fields (Progressive Scan)", list_w, NOT_YET));
        row(ui, label_w, "Display Format:", true, |ui| {
            let r = rate(d);
            let formats = timecode_formats(r);
            let chosen = if r.supports_drop_frame() { if d.drop_frame { "df" } else { "ndf" } } else { "tc" };
            let shown = formats.iter().find(|(id, _)| *id == chosen).map(|(_, l)| l.clone()).unwrap_or_default();
            let resp = egui::ComboBox::from_id_salt("seq-settings-video-display").selected_text(&shown).width(list_w).show_ui(ui, |ui| {
                for (id, label) in &formats {
                    let o = ui.selectable_label(*id == chosen, label);
                    if o.clicked() {
                        d.drop_frame = *id == "df";
                    }
                    elems.push((format!("sequenceSettings.videoDisplay.option.{id}"), o.rect, label.clone()));
                }
                for label in ["Feet + Frames 16mm", "Feet + Frames 35mm", "Frames"] {
                    ui.add_enabled(false, egui::Button::selectable(false, t(label))).on_disabled_hover_text(t(NOT_YET));
                }
            });
            elems.push(("sequenceSettings.videoDisplay".into(), resp.response.rect, shown));
        });
    });
    section(ui, "Audio", |ui| {
        row(ui, label_w, "Channel Format:", true, |ui| {
            let shown = d.mix.clone();
            let resp = egui::ComboBox::from_id_salt("seq-settings-mix").selected_text(t(&shown)).width(120.0).show_ui(ui, |ui| {
                for m in MIXES {
                    let o = ui.selectable_value(&mut d.mix, m.to_string(), t(m));
                    elems.push((format!("sequenceSettings.channelFormat.option.{m}"), o.rect, m.to_string()));
                }
            });
            elems.push(("sequenceSettings.channelFormat".into(), resp.response.rect, shown));
            ui.add_enabled(false, egui::Label::new(tl!("Number of Channels:")));
            let n = match d.mix.as_str() {
                "Mono" => "1",
                "5.1" => "6",
                "Adaptive" => "—",
                _ => "2",
            };
            fixed_combo(ui, elems, "channels", n, 60.0, NOT_YET);
        });
        row(ui, label_w, "Sample Rate:", true, |ui| {
            let mut rates = SAMPLE_RATES.to_vec();
            if !rates.contains(&d.sample_rate) {
                rates.insert(0, d.sample_rate);
            }
            let shown = format!("{} Hz", d.sample_rate);
            let resp = egui::ComboBox::from_id_salt("seq-settings-sample-rate").selected_text(&shown).width(list_w).show_ui(ui, |ui| {
                for hz in rates {
                    let label = format!("{hz} Hz");
                    let o = ui.selectable_value(&mut d.sample_rate, hz, &label);
                    elems.push((format!("sequenceSettings.sampleRate.option.{hz}"), o.rect, label));
                }
            });
            elems.push(("sequenceSettings.sampleRate".into(), resp.response.rect, shown));
        });
        let shown = if audio_samples { "Audio Samples" } else { "Milliseconds" };
        row(ui, label_w, "Display Format:", false, |ui| fixed_combo(ui, elems, "audioDisplay", shown, list_w, NOT_YET));
    });
    section(ui, "Video Previews", |ui| {
        row(ui, label_w, "Preview File Format:", false, |ui| {
            fixed_combo(ui, elems, "previewFormat", "QuickTime", list_w, NOT_YET);
            ui.add_enabled(false, egui::Button::new(tl!("Configure…"))).on_disabled_hover_text(t(NOT_YET));
        });
        row(ui, label_w, "Codec:", false, |ui| fixed_combo(ui, elems, "previewCodec", &cur.preview_codec, list_w, NOT_YET));
        row(ui, label_w, "Width:", false, |ui| fixed_value(ui, elems, "previewWidth", &d.width.to_string()));
        row(ui, label_w, "Height:", false, |ui| {
            fixed_value(ui, elems, "previewHeight", &d.height.to_string());
            ui.add_enabled(false, egui::Button::new(tl!("Reset"))).on_disabled_hover_text(t(NOT_YET));
        });
        ui.horizontal(|ui| {
            let mut bit_depth = cur.max_bit_depth;
            let r = ui.add_enabled(false, egui::Checkbox::new(&mut bit_depth, tl!("Maximum Bit Depth"))).on_disabled_hover_text(t(NOT_YET));
            elems.push(("sequenceSettings.maxBitDepth".into(), r.rect, bit_depth.to_string()));
            let r = ui.checkbox(&mut d.max_render_quality, tl!("Maximum Render Quality"));
            elems.push(("sequenceSettings.maxRenderQuality".into(), r.rect, d.max_render_quality.to_string()));
        });
        let mut linear = true;
        let r = ui.add_enabled(false, egui::Checkbox::new(&mut linear, tl!("Composite in Linear Color"))).on_disabled_hover_text(t(LINEAR));
        elems.push(("sequenceSettings.linearColor".into(), r.rect, "true".into()));
    });
}

fn color(ui: &mut egui::Ui, d: &mut SequenceSettingsDraft, elems: &mut Elems) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let label_w = ui.painter().layout_no_wrap(tl!("Working Color Space:").to_string(), font, egui::Color32::WHITE).size().x;
    row(ui, label_w, "Working Color Space:", true, |ui| {
        let shown = WorkingSpace::parse(&d.working_space).map_or(d.working_space.clone(), |w| t(w.label()).to_string());
        let resp = egui::ComboBox::from_id_salt("seq-settings-working-space").selected_text(&shown).width(220.0).show_ui(ui, |ui| {
            for w in WorkingSpace::ALL {
                let o = ui.selectable_value(&mut d.working_space, w.id().to_string(), t(w.label()));
                elems.push((format!("sequenceSettings.workingSpace.option.{}", w.id()), o.rect, w.label().to_string()));
            }
        });
        elems.push(("sequenceSettings.workingSpace".into(), resp.response.rect, shown));
    });
    ui.add_space(4.0);
    let r = ui.checkbox(&mut d.wide_gamut, tl!("Wide gamut color (composite in BT.2020)"));
    elems.push(("sequenceSettings.wideGamut".into(), r.rect, d.wide_gamut.to_string()));
    let r = ui.checkbox(&mut d.auto_tone_map, tl!("Auto Tone Map Media (HDR and log into SDR)"));
    elems.push(("sequenceSettings.autoToneMap".into(), r.rect, d.auto_tone_map.to_string()));
}

fn vr(ui: &mut egui::Ui, elems: &mut Elems) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let label_w = ui.painter().layout_no_wrap(tl!("Captured View:").to_string(), font, egui::Color32::WHITE).size().x;
    row(ui, label_w, "Projection:", false, |ui| fixed_combo(ui, elems, "vr.projection", "None", 200.0, NOT_YET));
    row(ui, label_w, "Layout:", false, |ui| fixed_combo(ui, elems, "vr.layout", "Monoscopic", 200.0, NOT_YET));
    row(ui, label_w, "Captured View:", false, |ui| {
        fixed_value(ui, elems, "vr.horizontal", "0°");
        ui.add_enabled(false, egui::Label::new(tl!("Horizontal")));
        fixed_value(ui, elems, "vr.vertical", "0°");
        ui.add_enabled(false, egui::Label::new(tl!("Vertical")));
    });
}

/// Draw the dialog. Returns whether it stays open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let new_sequence = app.ui.sequence_settings.new_sequence;
    // New Sequence compares against the defaults it started from; Sequence Settings against the
    // active sequence
    let (cur, cur_name) = if new_sequence {
        (SequenceSettings::default(), String::new())
    } else {
        let Some(q) = app.session.active_sequence() else { return false };
        let name = app.session.state.active_sequence.and_then(|id| app.session.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
        (q.settings.clone(), name)
    };
    let audio_samples = app.session.project.settings.audio_display_samples;
    let accent = app.tokens.accent;
    let customs = app.session.sequence_presets.user.clone();
    let mut actions: Actions = Vec::new();
    let mut d = app.ui.sequence_settings.clone();
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::new("sequence-settings-before-drag"), &mut d);
    let mut keep = true;
    let mut apply = false;
    let mut elems: Elems = Vec::new();
    let max_h = ctx.content_rect().height() * 0.8;
    let title = if new_sequence { tl!("New Sequence") } else { tl!("Sequence Settings") };
    let tabs: &[(&str, &str)] = if new_sequence {
        &[("presets", "Sequence Presets"), ("general", "General"), ("tracks", "Tracks")]
    } else {
        &[("general", "General"), ("color", "Color Management"), ("vr", "VR Properties")]
    };
    egui::Window::new(title)
        .id(egui::Id::new("Sequence Settings"))
        .collapsible(false)
        .resizable(false)
        .default_width(620.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.set_width(620.0);
            ui.horizontal(|ui| {
                for &(id, label) in tabs {
                    let r = ui.selectable_label(d.tab == id, t(label));
                    if r.clicked() {
                        d.tab = id.to_string();
                    }
                    elems.push((format!("sequenceSettings.tab.{id}"), r.rect, label.to_string()));
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| match d.tab.as_str() {
                "color" if !new_sequence => color(ui, &mut d, &mut elems),
                "presets" if new_sequence => presets(ui, &mut d, &customs, &mut elems, &mut actions),
                "tracks" if new_sequence => tracks(ui, &mut d, &mut elems),
                "vr" if !new_sequence => vr(ui, &mut elems),
                _ => general(ui, &mut d, &cur, audio_samples, &mut elems),
            });
            ui.add_space(10.0);
            save_preset_row(ui, &mut d, &customs, &mut elems, &mut actions);
            ui.horizontal(|ui| {
                let r = ui.add_enabled(d.save_name.is_none(), egui::Button::new(tl!("Save Preset…")));
                elems.push(("sequenceSettings.savePreset".into(), r.rect, "Save Preset…".into()));
                if r.clicked() {
                    d.save_name = Some(String::new());
                    d.save_replace = false;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(accent));
                    elems.push(("sequenceSettings.ok".into(), o.rect, "OK".into()));
                    if o.clicked() {
                        apply = true;
                    }
                    let c = ui.button(tl!("Cancel"));
                    elems.push(("sequenceSettings.cancel".into(), c.rect, "Cancel".into()));
                    if c.clicked() {
                        keep = false;
                    }
                });
            });
        });
    // the preset commands: not edits of the project, so they don't close the dialog
    for (cmd, params) in actions {
        let name = params["name"].as_str().unwrap_or("").to_string();
        match app.session.execute(cmd, params.clone()) {
            Ok(_) => match cmd {
                "sequence.preset.save" => {
                    app.ui.status = tlf!("Saved sequence preset {name}", name).to_string();
                    d.save_name = None;
                    d.save_replace = false;
                    // New Sequence: the saved preset is the chosen one
                    if d.new_sequence {
                        d.preset = name;
                    }
                }
                "sequence.preset.rename" => {
                    let to = params["to"].as_str().unwrap_or("").to_string();
                    if d.preset == name {
                        d.preset = to;
                    }
                    d.renaming = None;
                }
                _ => {
                    if d.preset == name {
                        d.preset.clear();
                    }
                    d.deleting = None;
                }
            },
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    // typing a preset name: Enter and Escape belong to the name field
    let editing_name = d.save_name.is_some() || d.renaming.is_some();
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if !editing_name && crate::widgets::escape_closes(ctx) {
        keep = false;
    }
    if !editing_name && ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !ctx.egui_wants_keyboard_input() {
        apply = true;
    }
    if apply && new_sequence {
        let (p, rest) = new_sequence_params(&d);
        let r = app
            .session
            .execute("file.newSequence", Value::Object(p))
            .and_then(|_| if rest.is_empty() { Ok(Value::Null) } else { app.session.execute("sequence.settings", Value::Object(rest)) });
        if let Err(e) = r {
            app.ui.status = e.to_string();
            keep = true;
        } else {
            keep = false;
        }
        app.ui.sequence_settings = d;
        return keep;
    }
    if apply {
        let p = changes(&d, &cur, &cur_name);
        // nothing changed: OK closes the dialog and adds no undo step
        if !p.is_empty()
            && let Err(e) = app.session.execute("sequence.settings", Value::Object(p))
        {
            app.ui.status = e.to_string();
            keep = true;
        } else {
            keep = false;
        }
    }
    app.ui.sequence_settings = d;
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_labels() {
        assert_eq!(aspect(1920, 1080), "16:9");
        assert_eq!(aspect(640, 360), "16:9");
        assert_eq!(aspect(1080, 1920), "9:16");
        assert_eq!(aspect(1440, 1080), "4:3");
        assert_eq!(aspect(4096, 2160), "1.90:1");
        // non-square pixels: the display aspect
        assert_eq!(aspect(display_width(1440, 1080, (4, 3)), 1080), "16:9");
        assert_eq!(aspect(display_width(1280, 1080, (3, 2)), 1080), "16:9");
        assert_eq!(display_width(u32::MAX, 1, (8, 1)), u32::MAX);
        assert_eq!(display_width(1440, 1080, (0, 0)), 1440);
    }

    #[test]
    fn display_formats_follow_the_timebase() {
        assert_eq!(timecode_formats(FrameRate::FPS_23_976), vec![("tc", "23.976 fps Timecode".to_string())]);
        let ntsc = timecode_formats(FrameRate::FPS_29_97);
        assert_eq!(ntsc.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec!["df", "ndf"]);
        assert_eq!(ntsc[0].1, "29.97 fps Drop-Frame Timecode");
    }

    #[test]
    fn new_sequence_sends_its_settings_to_the_command() {
        let d = SequenceSettingsDraft {
            new_sequence: true,
            name: " Shorts ".into(),
            width: 1080,
            height: 1920,
            video_tracks: 2,
            audio_tracks: 4,
            ..Default::default()
        };
        let (p, rest) = new_sequence_params(&d);
        assert_eq!(
            Value::Object(p),
            json!({"name":"Shorts","width":1080,"height":1920,"fps":24_000.0/1001.0,"sampleRate":48_000,"mix":"Stereo","video":2,"audio":4})
        );
        assert!(rest.is_empty(), "everything else is the default: {rest:?}");
        // a chosen preset travels first (the engine takes its pixel aspect ratio and name from it)
        let d = SequenceSettingsDraft {
            new_sequence: true,
            preset: "6K Anamorphic 1.33x 6048x3402 23.976".into(),
            width: 6048,
            height: 3402,
            par: (4, 3),
            ..Default::default()
        };
        let (p, _) = new_sequence_params(&d);
        assert_eq!(p["preset"], "6K Anamorphic 1.33x 6048x3402 23.976");
        assert_eq!((p["width"].clone(), p["height"].clone()), (json!(6048), json!(3402)));
        // what file.newSequence has no parameter for follows on the new sequence; a blank name
        // lets the command name it
        let d = SequenceSettingsDraft {
            new_sequence: true,
            name: " ".into(),
            fps_num: 30_000,
            fps_den: 1001,
            drop_frame: true,
            max_render_quality: true,
            ..Default::default()
        };
        let (p, rest) = new_sequence_params(&d);
        assert!(p.get("name").is_none());
        assert_eq!(Value::Object(rest), json!({"dropFrame":true,"maxRenderQuality":true}));
    }

    #[test]
    fn only_changed_settings_are_sent() {
        let cur = SequenceSettings::default();
        let mut d = SequenceSettingsDraft::default();
        assert!(changes(&d, &cur, "").is_empty(), "the defaults match: nothing to send");
        d.width = 1280;
        d.height = 720;
        d.max_render_quality = true;
        let p = changes(&d, &cur, "");
        assert_eq!(Value::Object(p), json!({"width":1280,"height":720,"scaleMotion":true,"maxRenderQuality":true}));
        // drop-frame only travels for a rate that has it
        let mut d = SequenceSettingsDraft { drop_frame: true, ..Default::default() };
        assert!(changes(&d, &cur, "").is_empty());
        (d.fps_num, d.fps_den) = (30_000, 1001);
        assert_eq!(Value::Object(changes(&d, &cur, "")), json!({"fps":30_000.0/1001.0,"dropFrame":true}));
        // the name travels when it changed; blank or unchanged it doesn't
        let d = SequenceSettingsDraft { name: "  Rough Cut  ".into(), ..Default::default() };
        assert_eq!(Value::Object(changes(&d, &cur, "Sequence 01")), json!({"name":"Rough Cut"}));
        assert!(changes(&d, &cur, "Rough Cut").is_empty());
        let d = SequenceSettingsDraft { name: "   ".into(), ..Default::default() };
        assert!(changes(&d, &cur, "Sequence 01").is_empty());
        // the pixel aspect ratio travels when it differs in lowest terms
        let d = SequenceSettingsDraft { par: (133, 100), ..Default::default() };
        assert_eq!(Value::Object(changes(&d, &cur, "")), json!({"pixelAspect":[133,100]}));
        let four_thirds = SequenceSettings { par: (4, 3), ..Default::default() };
        let d = SequenceSettingsDraft { par: (8, 6), ..Default::default() };
        assert!(changes(&d, &four_thirds, "").is_empty(), "8:6 is the 4:3 it has");
        let d = SequenceSettingsDraft { par: (0, 3), ..Default::default() };
        assert_eq!(Value::Object(changes(&d, &cur, "")), json!({"pixelAspect":[0,3]}), "sent for the command to refuse");
    }
}
