//! Essential Sound panel: Browse / Edit tabs, audio-type assignment, per-type collapsible sections
//! (Loudness, Repair, Clarity, Creative, Ducking, Duration, Pan) with switch + slider rows, presets,
//! and the Clip Volume / Mute footer. Every control runs an `essentialSound.*` command (slider drags
//! are one undo step), so the panel, MCP and the control channel share one code path.
//!
//! Automation ids: `essentialSound.tab.<Browse|Edit>`, `essentialSound.type.<Dialogue|Music|SFX|
//! Ambience>`, `essentialSound.clearType`, `essentialSound.preset` (+ `.save`, `.delete`,
//! `.name`, `.ok`), `essentialSound.section.<Name>` (twirl) and `.toggle` (switch), setting rows by
//! key (`essentialSound.repair.noise.on`, `essentialSound.repair.noise.amount`,
//! `essentialSound.clarity.eqPreset`…), `essentialSound.autoMatch`,
//! `essentialSound.ducking.against.<Type>`, `essentialSound.generateDucking`; while a dropdown is
//! open its entries `<dropdown id>.option.<name>` (`essentialSound.preset.option.<preset>`,
//! `essentialSound.clarity.eqPreset.option.<name>`…). A slider's id is its knob (a click there
//! keeps the value, a drag from it moves it); `<slider id>.track` is the whole track.
//! `essentialSound.volume.on` / `.levelDb`, `essentialSound.mute`, and in Browse
//! `essentialSound.browse.<Type>.<preset>`.

use egui::{Align2, Color32, Painter, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::TrackItem;
use filmcraft_project::essential::{self as es, AudioType, EssentialSound, Section, Slot};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const PAD: f32 = 14.0;
const FOOTER_H: f32 = 86.0;

/// A command to run after layout.
type Actions = Vec<(String, Value)>;

struct Ctx<'a> {
    t: Tokens,
    auto: &'a mut crate::automation::Registry,
    actions: Actions,
    clips: Vec<u64>,
}

impl Ctx<'_> {
    fn run(&mut self, cmd: &str, mut p: Value) {
        if p.get("clips").is_none() && !cmd.ends_with("deletePreset") {
            p["clips"] = json!(self.clips);
        }
        self.actions.push((cmd.to_string(), p));
    }
    fn set(&mut self, key: &str, v: Value, begin: bool) {
        self.run("essentialSound.set", json!({"key": key, "value": v, "begin": begin}));
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clip_ids = filmcraft_engine::essential_sound::targets(&app.session, &Value::Null);
    let items: Vec<TrackItem> =
        app.session.active_sequence().map(|seq| clip_ids.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.clone())).collect()).unwrap_or_default();
    let presets = filmcraft_engine::essential_sound::presets(&app.session);
    let tab = if app.ui.essential_sound_tab == "Browse" { "Browse" } else { "Edit" };
    let mut cx = Ctx { t, auto: &mut app.auto, actions: Vec::new(), clips: clip_ids.iter().map(|c| c.0).collect() };
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.panel_bg);
    // ---- Browse | Edit
    let mut x = rect.min.x + PAD;
    let ty = rect.min.y + 16.0;
    let mut new_tab = None;
    for name in ["Browse", "Edit"] {
        let g = painter.layout_no_wrap(name.to_string(), Tokens::ui(12.5), t.text);
        let r = Rect::from_min_size(pos2(x, ty - 9.0), vec2(g.size().x, 20.0));
        let resp = ui.interact(r, egui::Id::new(("es-tab", name)), Sense::click());
        let col = if name == tab || resp.hovered() { t.text } else { t.text_dim };
        painter.text(pos2(x, ty), Align2::LEFT_CENTER, name, Tokens::ui(12.5), col);
        if name == tab {
            painter.line_segment([pos2(x, ty + 10.0), pos2(x + g.size().x, ty + 10.0)], Stroke::new(1.5, t.text));
        }
        cx.auto.add(&format!("essentialSound.tab.{name}"), r, name);
        if resp.clicked() {
            new_tab = Some(name.to_string());
        }
        x += g.size().x + 22.0;
    }
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + 32.0), rect.max);
    if tab == "Browse" {
        browse(ui, &mut cx, body, &presets, &items);
    } else {
        edit(ui, &mut cx, body, &items, &presets, app.ui.collapsed_fx.clone(), app.ui.expanded_fx.clone());
    }
    if let Some(n) = new_tab {
        app.ui.essential_sound_tab = n;
    }
    // section twirls are UI state; everything else is a command
    for (cmd, p) in std::mem::take(&mut cx.actions) {
        if cmd == "ui.twirl" {
            let key = p["key"].as_str().unwrap_or_default().to_string();
            if p["open"].as_bool().unwrap_or(false) {
                app.ui.collapsed_fx.retain(|k| *k != key);
                app.ui.expanded_fx.push(key);
            } else {
                app.ui.expanded_fx.retain(|k| *k != key);
                app.ui.collapsed_fx.push(key);
            }
            continue;
        }
        if let Err(e) = app.session.execute(&cmd, p) {
            app.ui.status = e.to_string();
        }
    }
}

// ------------------------------------------------------------------------------------- browse

fn browse(ui: &mut egui::Ui, cx: &mut Ctx, rect: Rect, presets: &[es::Preset], items: &[TrackItem]) {
    let t = cx.t;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(PAD, 4.0))).id_salt("es-browse"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_max_width(ui.available_width() - 10.0);
        if items.is_empty() {
            ui.label(egui::RichText::new("Select audio clips, then click a preset to apply it.").color(t.text_faint).size(11.5));
        }
        for kind in AudioType::ALL {
            ui.add_space(8.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            type_icon(ui.painter(), Rect::from_center_size(pos2(r.min.x + 8.0, r.center().y), vec2(16.0, 16.0)), kind, t.text);
            ui.painter().text(pos2(r.min.x + 24.0, r.center().y), Align2::LEFT_CENTER, kind.label(), Tokens::semibold(12.5), t.text);
            for p in presets.iter().filter(|p| p.kind == kind) {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 2.0, t.hover);
                }
                let name = if p.builtin { p.name.clone() } else { format!("{} (user)", p.name) };
                ui.painter().text(
                    pos2(r.min.x + 24.0, r.center().y),
                    Align2::LEFT_CENTER,
                    name,
                    Tokens::ui(12.0),
                    if items.is_empty() { t.text_dim } else { t.text },
                );
                cx.auto.add(&format!("essentialSound.browse.{}.{}", kind.label(), p.name), r, &p.name);
                if resp.clicked() && !items.is_empty() {
                    cx.run("essentialSound.applyPreset", json!({"type": kind.id(), "preset": p.name}));
                }
            }
        }
    });
}

// ------------------------------------------------------------------------------------- edit

fn edit(ui: &mut egui::Ui, cx: &mut Ctx, rect: Rect, items: &[TrackItem], presets: &[es::Preset], collapsed: Vec<String>, expanded: Vec<String>) {
    let t = cx.t;
    if items.is_empty() {
        crate::dock::placeholder(ui, rect, &t, "Select audio clips to edit them here");
        return;
    }
    let mut kinds: Vec<Option<AudioType>> = items.iter().map(|i| i.essential.as_ref().map(|e| e.kind)).collect();
    kinds.dedup();
    let title = if items.len() > 1 { "Multiple Clips Selected".to_string() } else { items[0].name.clone() };
    let body = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - FOOTER_H));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(body.min + vec2(PAD, 2.0), body.max - vec2(PAD - 2.0, 2.0))).id_salt("es-edit"));
    let typed = items.iter().find_map(|i| i.essential.clone());
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_max_width(ui.available_width() - 10.0);
        ui.label(egui::RichText::new(&title).font(Tokens::semibold(14.0)).color(t.text));
        ui.add_space(6.0);
        match (&kinds[..], &typed) {
            ([Some(_)], Some(st)) => typed_body(ui, cx, st, items, presets, &collapsed, &expanded),
            _ => {
                if kinds.len() > 1 {
                    ui.label(
                        egui::RichText::new("The selected clips have different audio types. Assign one type to edit them together.")
                            .color(t.text_dim)
                            .size(11.5),
                    );
                } else {
                    ui.label(egui::RichText::new("Assign an audio type to the selection:").color(t.text_dim).size(12.0));
                }
                ui.add_space(6.0);
                for kind in AudioType::ALL {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                    let p = ui.painter();
                    p.rect_filled(r.shrink(2.0), 4.0, if resp.hovered() { t.hover } else { t.field_bg });
                    p.rect_stroke(r.shrink(2.0), 4.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
                    type_icon(p, Rect::from_center_size(pos2(r.min.x + 26.0, r.center().y), vec2(22.0, 22.0)), kind, t.text);
                    p.text(pos2(r.min.x + 50.0, r.center().y), Align2::LEFT_CENTER, kind.label(), Tokens::semibold(13.0), t.text);
                    cx.auto.add(&format!("essentialSound.type.{}", kind.label()), r, kind.label());
                    if resp.clicked() {
                        cx.run("essentialSound.setType", json!({"type": kind.id()}));
                    }
                }
            }
        }
        ui.add_space(24.0);
    });
    footer(ui, cx, Rect::from_min_max(pos2(rect.min.x, rect.max.y - FOOTER_H), rect.max), typed.as_ref());
}

#[allow(clippy::too_many_arguments)]
fn typed_body(ui: &mut egui::Ui, cx: &mut Ctx, st: &EssentialSound, items: &[TrackItem], presets: &[es::Preset], collapsed: &[String], expanded: &[String]) {
    let t = cx.t;
    let kind = st.kind;
    // type badge + Clear Audio Type
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    type_icon(ui.painter(), Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(18.0, 18.0)), kind, t.text);
    ui.painter().text(pos2(r.min.x + 28.0, r.center().y), Align2::LEFT_CENTER, kind.label(), Tokens::ui(12.5), t.text);
    let br = Rect::from_min_max(pos2(r.max.x - 122.0, r.min.y + 3.0), pos2(r.max.x, r.max.y - 3.0));
    if button(ui, cx, br, "Clear Audio Type", "essentialSound.clearType") {
        cx.run("essentialSound.clearType", json!({}));
    }
    ui.add_space(6.0);
    // Preset: [▾] save delete
    ui.label(egui::RichText::new("Preset:").color(t.text_dim).size(12.0));
    let mine: Vec<&es::Preset> = presets.iter().filter(|p| p.kind == kind).collect();
    let current = if st.preset.is_empty() { "(Custom)".to_string() } else { st.preset.clone() };
    let mut picked = None;
    let draft_id = egui::Id::new("es-preset-draft");
    let mut draft: Option<String> = ui.data(|d| d.get_temp(draft_id));
    ui.horizontal(|ui| {
        let w = ui.available_width() - 56.0;
        let cb = egui::ComboBox::from_id_salt("es-preset").selected_text(&current).width(w).show_ui(ui, |ui| {
            for p in &mine {
                let o = ui.selectable_label(p.name == st.preset, &p.name);
                cx.auto.add(&format!("essentialSound.preset.option.{}", p.name), o.rect, &p.name);
                if o.clicked() {
                    picked = Some(p.name.clone());
                }
            }
        });
        cx.auto.add("essentialSound.preset", cb.response.rect, "Preset");
        let (sr, sresp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
        save_icon(ui.painter(), sr.shrink(3.0), if sresp.hovered() { t.text } else { t.text_dim });
        cx.auto.add("essentialSound.preset.save", sr, "Save Preset");
        if sresp.clicked() {
            draft = Some(String::new());
        }
        let deletable = mine.iter().any(|p| !p.builtin && p.name == st.preset);
        let (dr, dresp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
        icons::paint(ui.painter(), dr.shrink(3.0), Icon::Trash, if deletable { t.text_dim } else { t.text_faint });
        cx.auto.add("essentialSound.preset.delete", dr, "Delete Preset");
        if dresp.clicked() && deletable {
            cx.run("essentialSound.deletePreset", json!({"name": st.preset, "type": kind.id()}));
        }
    });
    if let Some(name) = picked {
        cx.run("essentialSound.applyPreset", json!({"preset": name}));
    }
    if let Some(mut d) = draft.take() {
        let mut done = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut d).hint_text("Preset name").desired_width(ui.available_width() - 70.0));
            cx.auto.add("essentialSound.preset.name", r.rect, "Preset name");
            let ok = ui.button("Save");
            cx.auto.add("essentialSound.preset.ok", ok.rect, "Save");
            if (ok.clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && !d.trim().is_empty() {
                cx.run("essentialSound.savePreset", json!({"name": d.trim()}));
                done = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = true;
            }
        });
        ui.data_mut(|m| {
            if done {
                m.remove::<String>(draft_id);
            } else {
                m.insert_temp(draft_id, d);
            }
        });
    } else {
        ui.data_mut(|m| m.remove::<String>(draft_id));
    }
    ui.add_space(10.0);
    for sec in kind.sections() {
        let key = format!("essentialSound:{}", sec.label());
        let default_open = !matches!(sec, Section::Loudness | Section::Pan | Section::Duration);
        let open = if default_open { !collapsed.contains(&key) } else { expanded.contains(&key) };
        let enabled = sec.key().and_then(|k| st.get(k)).and_then(|v| v.as_bool());
        if section_header(ui, cx, *sec, open, enabled, &key) {
            ui.add_space(4.0);
            let on = enabled.unwrap_or(true);
            match sec {
                Section::Loudness => loudness(ui, cx, st, items),
                Section::Repair => {
                    slot(ui, cx, "Reduce Noise", "repair.noise", st.repair.noise, on);
                    slot(ui, cx, "Reduce Rumble", "repair.rumble", st.repair.rumble, on);
                    slot(ui, cx, "DeHum", "repair.dehum", st.repair.dehum, on);
                    choice_row(ui, cx, "repair.humHz", &["50 Hz", "60 Hz"], usize::from(st.repair.hum_hz == 60), on && st.repair.dehum.on, |i| {
                        json!(if i == 1 { 60 } else { 50 })
                    });
                    slot(ui, cx, "DeEss", "repair.deess", st.repair.deess, on);
                    slot(ui, cx, "Reduce Reverb", "repair.reverb", st.repair.reverb, on);
                }
                Section::Clarity => {
                    slot(ui, cx, "Dynamics", "clarity.dynamics", st.clarity.dynamics, on);
                    switch_row(ui, cx, "EQ", "clarity.eq.on", st.clarity.eq.on, on);
                    let names: Vec<&str> = es::EQ_PRESETS.iter().map(|p| p.name).collect();
                    combo(ui, cx, "clarity.eqPreset", &names, &st.clarity.eq_preset, on && st.clarity.eq.on);
                    slider(ui, cx, "Amount", "clarity.eq.amount", st.clarity.eq.amount, (0.0, 10.0), 1, "", on && st.clarity.eq.on);
                    switch_row(ui, cx, "Enhance Speech", "clarity.enhance.on", st.clarity.enhance.on, on);
                    choice_row(ui, cx, "clarity.enhanceTone", &["Low Tone", "High Tone"], st.clarity.enhance_tone as usize, on && st.clarity.enhance.on, |i| {
                        json!(i)
                    });
                    slider(ui, cx, "Mix", "clarity.enhance.amount", st.clarity.enhance.amount, (0.0, 10.0), 1, "", on && st.clarity.enhance.on);
                }
                Section::Creative => {
                    switch_row(ui, cx, "Reverb", "creative.reverb.on", st.creative.reverb.on, on);
                    ui.label(egui::RichText::new("Preset:").color(t.text_dim).size(12.0));
                    let names: Vec<&str> = es::REVERB_PRESETS.iter().map(|p| p.name).collect();
                    combo(ui, cx, "creative.reverbPreset", &names, &st.creative.reverb_preset, on && st.creative.reverb.on);
                    slider(ui, cx, "Amount", "creative.reverb.amount", st.creative.reverb.amount, (0.0, 10.0), 1, "", on && st.creative.reverb.on);
                    if kind == AudioType::Ambience {
                        slot(ui, cx, "Stereo Width", "creative.width", st.creative.width, on);
                    }
                }
                Section::Ducking => ducking(ui, cx, st, on),
                Section::Duration => {
                    ui.label(
                        egui::RichText::new("Remixing music to a target duration is not available yet. Trim the clip and add a crossfade instead.")
                            .color(t.text_faint)
                            .size(11.5),
                    );
                }
                Section::Pan => slider(ui, cx, "Pan", "pan.value", st.pan.value, (-100.0, 100.0), 1, "", on),
            }
            ui.add_space(8.0);
        }
    }
    let r = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover()).0;
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
}

fn loudness(ui: &mut egui::Ui, cx: &mut Ctx, st: &EssentialSound, items: &[TrackItem]) {
    let t = cx.t;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let br = Rect::from_min_size(r.min + vec2(0.0, 2.0), vec2(104.0, 24.0));
    if button(ui, cx, br, "Auto-Match", "essentialSound.autoMatch") {
        cx.run("essentialSound.autoMatch", json!({}));
    }
    let text = match (st.loudness.measured_lufs, st.loudness.target_lufs) {
        (Some(m), Some(tg)) => format!("{m:.1} → {tg:.1} LUFS ({:+.1} dB)", st.loudness.gain_db),
        _ => "Not matched yet".to_string(),
    };
    ui.painter().text(pos2(br.max.x + 10.0, r.center().y), Align2::LEFT_CENTER, text, Tokens::ui(11.5), t.text_dim);
    if items.len() > 1 {
        ui.label(egui::RichText::new(format!("Matches each of the {} clips on its own.", items.len())).color(t.text_faint).size(11.0));
    }
}

fn ducking(ui: &mut egui::Ui, cx: &mut Ctx, st: &EssentialSound, on: bool) {
    let t = cx.t;
    ui.label(egui::RichText::new("Duck against:").color(t.text_dim).size(12.0));
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let mut x = r.min.x;
    for kind in AudioType::ALL {
        let br = Rect::from_min_size(pos2(x, r.min.y + 2.0), vec2(30.0, 26.0));
        let sel = st.ducking.against.contains(&kind);
        let resp = ui.interact(br, egui::Id::new(("es-duck", kind.id())), Sense::click());
        ui.painter().rect_filled(
            br,
            4.0,
            if sel {
                Color32::from_gray(0x4b)
            } else if resp.hovered() {
                t.hover
            } else {
                Color32::TRANSPARENT
            },
        );
        type_icon(ui.painter(), br.shrink(6.0), kind, if sel { t.text } else { t.text_faint });
        cx.auto.add(&format!("essentialSound.ducking.against.{}", kind.label()), br, kind.label());
        if resp.clicked() && on {
            let mut v = st.ducking.against.clone();
            if sel {
                v.retain(|k| *k != kind);
            } else {
                v.push(kind);
            }
            cx.set("ducking.against", json!(v), true);
        }
        x += 34.0;
    }
    let br = Rect::from_min_size(pos2(x + 4.0, r.min.y + 2.0), vec2(84.0, 26.0));
    let resp = ui.interact(br, egui::Id::new("es-duck-untyped"), Sense::click());
    let sel = st.ducking.against_untyped;
    ui.painter().rect_filled(
        br,
        4.0,
        if sel {
            Color32::from_gray(0x4b)
        } else if resp.hovered() {
            t.hover
        } else {
            Color32::TRANSPARENT
        },
    );
    ui.painter().text(br.center(), Align2::CENTER_CENTER, "Untagged", Tokens::ui(11.5), if sel { t.text } else { t.text_faint });
    cx.auto.add("essentialSound.ducking.against.Untagged", br, "Untagged");
    if resp.clicked() && on {
        cx.set("ducking.againstUntyped", json!(!sel), true);
    }
    slider(ui, cx, "Sensitivity", "ducking.sensitivity", st.ducking.sensitivity, (0.0, 10.0), 1, "", on);
    slider(ui, cx, "Reduce By", "ducking.reduceDb", st.ducking.reduce_db, (-40.0, 0.0), 1, " dB", on);
    slider(ui, cx, "Fades", "ducking.fadeS", st.ducking.fade_s * 1000.0, (0.0, 5000.0), 0, " ms", on);
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let br = Rect::from_min_size(r.min + vec2(0.0, 2.0), vec2(150.0, 24.0));
    if button(ui, cx, br, "Generate Keyframes", "essentialSound.generateDucking") && on {
        cx.run("essentialSound.generateDucking", json!({}));
    }
}

fn footer(ui: &mut egui::Ui, cx: &mut Ctx, rect: Rect, st: Option<&EssentialSound>) {
    let t = cx.t;
    let p = ui.painter();
    p.line_segment([pos2(rect.min.x + PAD, rect.min.y + 4.0), pos2(rect.max.x - PAD, rect.min.y + 4.0)], Stroke::new(1.0, t.separator));
    let Some(st) = st else { return };
    // Clip Volume: switch, label, blue value, slider
    let row = Rect::from_min_max(pos2(rect.min.x + PAD, rect.min.y + 12.0), pos2(rect.max.x - PAD, rect.min.y + 34.0));
    if switch(ui, cx, Rect::from_min_size(row.min + vec2(0.0, 4.0), vec2(28.0, 14.0)), st.volume.on, "essentialSound.volume.on", "Clip Volume") {
        cx.set("volume.on", json!(!st.volume.on), true);
    }
    ui.painter().text(pos2(row.min.x + 38.0, row.center().y), Align2::LEFT_CENTER, "Clip Volume", Tokens::ui(12.5), t.text);
    let vr = Rect::from_min_max(pos2(row.max.x - 70.0, row.min.y), row.max);
    ui.painter().text(pos2(vr.max.x - 18.0, vr.center().y), Align2::RIGHT_CENTER, format!("{:.1}", st.volume.level_db), Tokens::ui(12.0), t.hot_text);
    ui.painter().text(pos2(vr.max.x, vr.center().y), Align2::RIGHT_CENTER, "dB", Tokens::ui(12.0), t.text_dim);
    let track = Rect::from_min_max(pos2(row.min.x + 38.0, row.max.y + 6.0), pos2(row.max.x, row.max.y + 18.0));
    slider_track(ui, cx, track, "volume.levelDb", st.volume.level_db, (-24.0, 15.0), st.volume.on);
    let mr = Rect::from_min_size(pos2(rect.min.x + PAD, rect.max.y - 24.0), vec2(28.0, 14.0));
    if switch(ui, cx, mr, st.mute, "essentialSound.mute", "Mute") {
        cx.set("mute", json!(!st.mute), true);
    }
    ui.painter().text(pos2(mr.max.x + 10.0, mr.center().y), Align2::LEFT_CENTER, "Mute", Tokens::ui(12.5), t.text);
}

// ------------------------------------------------------------------------------------- widgets

/// Section header: twirl, bold title, switch at the right. Returns whether it is open.
fn section_header(ui: &mut egui::Ui, cx: &mut Ctx, sec: Section, open: bool, enabled: Option<bool>, key: &str) -> bool {
    let t = cx.t;
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(11.0, 11.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text,
    );
    ui.painter().text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, sec.label(), Tokens::semibold(13.0), t.text);
    cx.auto.add(&format!("essentialSound.section.{}", sec.label()), r, sec.label());
    if let (Some(on), Some(k)) = (enabled, sec.key()) {
        let sw = Rect::from_center_size(pos2(r.max.x - 16.0, r.center().y), vec2(30.0, 15.0));
        if switch(ui, cx, sw, on, &format!("essentialSound.section.{}.toggle", sec.label()), sec.label()) {
            cx.set(k, json!(!on), true);
            return open;
        }
    }
    if resp.clicked() {
        cx.actions.push(("ui.twirl".into(), json!({"key": key, "open": !open})));
    }
    open
}

/// A pill switch. Returns clicked.
fn switch(ui: &mut egui::Ui, cx: &mut Ctx, r: Rect, on: bool, id: &str, label: &str) -> bool {
    let resp = ui.interact(r.expand(3.0), egui::Id::new(("es-sw", id)), Sense::click());
    let p = ui.painter();
    let rad = r.height() / 2.0;
    if on {
        p.rect_filled(r, rad, Color32::from_gray(0xd4));
        p.circle_filled(pos2(r.max.x - rad, r.center().y), rad - 3.0, Color32::from_gray(0x1d));
    } else {
        p.rect_stroke(r, rad, Stroke::new(1.5, Color32::from_gray(if resp.hovered() { 0xb0 } else { 0x8a })), StrokeKind::Inside);
        p.circle_stroke(pos2(r.min.x + rad, r.center().y), rad - 3.0, Stroke::new(1.5, Color32::from_gray(0xd4)));
    }
    cx.auto.add(id, r, label);
    resp.clicked()
}

fn switch_row(ui: &mut egui::Ui, cx: &mut Ctx, label: &str, key: &str, on: bool, active: bool) {
    let t = cx.t;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let sw = Rect::from_min_size(pos2(r.min.x, r.center().y - 7.5), vec2(30.0, 15.0));
    if switch(ui, cx, sw, on, &format!("essentialSound.{key}"), label) && active {
        cx.set(key, json!(!on), true);
    }
    ui.painter().text(pos2(sw.max.x + 12.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.5), if active { t.text } else { t.text_faint });
}

/// Switch + label row and its 0…10 amount slider.
fn slot(ui: &mut egui::Ui, cx: &mut Ctx, label: &str, key: &str, s: Slot, active: bool) {
    switch_row(ui, cx, label, &format!("{key}.on"), s.on, active);
    slider(ui, cx, "", &format!("{key}.amount"), s.amount, (0.0, 10.0), 1, "", active && s.on);
}

/// Label + value line, then a track with a ring knob.
#[allow(clippy::too_many_arguments)]
fn slider(ui: &mut egui::Ui, cx: &mut Ctx, label: &str, key: &str, v: f64, range: (f64, f64), decimals: usize, suffix: &str, active: bool) {
    let t = cx.t;
    let h = if label.is_empty() { 26.0 } else { 46.0 };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let col = if active { t.text_dim } else { t.text_faint };
    let ty = if label.is_empty() {
        let vx = r.max.x;
        ui.painter().text(pos2(vx, r.center().y), Align2::RIGHT_CENTER, format!("{v:.decimals$}{suffix}"), Tokens::ui(11.5), col);
        r.center().y
    } else {
        ui.painter().text(pos2(r.min.x, r.min.y + 10.0), Align2::LEFT_CENTER, label, Tokens::ui(12.5), if active { t.text } else { t.text_faint });
        ui.painter().text(pos2(r.max.x, r.min.y + 10.0), Align2::RIGHT_CENTER, format!("{v:.decimals$}{suffix}"), Tokens::ui(11.5), col);
        r.min.y + 32.0
    };
    let right = if label.is_empty() { r.max.x - 40.0 } else { r.max.x };
    let track = Rect::from_min_max(pos2(r.min.x + if label.is_empty() { 42.0 } else { 0.0 }, ty - 6.0), pos2(right, ty + 6.0));
    slider_track(ui, cx, track, key, v, range, active);
}

/// The bare slider: 1 pt track, hollow ring knob. Sends `essentialSound.set` while dragged.
fn slider_track(ui: &mut egui::Ui, cx: &mut Ctx, track: Rect, key: &str, v: f64, (lo, hi): (f64, f64), active: bool) {
    let t = cx.t;
    let y = track.center().y;
    let f = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    let kx = track.min.x + f * track.width();
    let resp = ui.interact(track.expand2(vec2(6.0, 4.0)), egui::Id::new(("es-slider", key)), Sense::click_and_drag());
    let line = if active { Color32::from_gray(0x8a) } else { Color32::from_gray(0x4a) };
    ui.painter().line_segment([pos2(track.min.x, y), pos2(kx - 7.0, y)], Stroke::new(1.5, line));
    ui.painter().line_segment([pos2(kx + 7.0, y), pos2(track.max.x, y)], Stroke::new(1.5, line));
    ui.painter().circle_filled(pos2(kx, y), 6.0, t.panel_bg);
    ui.painter().circle_stroke(
        pos2(kx, y),
        6.0,
        Stroke::new(
            1.6,
            if resp.dragged() {
                t.hot_text
            } else if active {
                Color32::from_gray(0xd4)
            } else {
                Color32::from_gray(0x6a)
            },
        ),
    );
    // The id is the knob: a click on it keeps the value and a drag from it moves it; `.track` is the
    // whole slider, for a click or drag to a position along it.
    cx.auto.add(&format!("essentialSound.{key}"), Rect::from_center_size(pos2(kx, y), vec2(12.0, 12.0)), key);
    cx.auto.add(&format!("essentialSound.{key}.track"), track, key);
    if !active {
        return;
    }
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
        // a click on the knob itself leaves the value alone
        && !(resp.clicked() && (p.x - kx).abs() <= 6.0)
    {
        let nf = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
        let mut nv = lo + nf * (hi - lo);
        nv = (nv * 10.0).round() / 10.0;
        if (nv - v).abs() > 1e-9 {
            // Fades are edited in ms and stored in seconds.
            let send = if key == "ducking.fadeS" { (nv / 1000.0 * 100.0).round() / 100.0 } else { nv };
            cx.set(key, json!(send), resp.drag_started() || resp.clicked());
        }
    }
}

fn combo(ui: &mut egui::Ui, cx: &mut Ctx, key: &str, names: &[&str], current: &str, active: bool) {
    let mut picked = None;
    ui.add_enabled_ui(active, |ui| {
        let cb = egui::ComboBox::from_id_salt(("es-combo", key)).selected_text(current).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
            for n in names {
                let o = ui.selectable_label(*n == current, *n);
                cx.auto.add(&format!("essentialSound.{key}.option.{n}"), o.rect, n);
                if o.clicked() {
                    picked = Some(n.to_string());
                }
            }
        });
        cx.auto.add(&format!("essentialSound.{key}"), cb.response.rect, key);
    });
    if let Some(n) = picked {
        cx.set(key, json!(n), true);
    }
}

/// Segmented choice (50 Hz | 60 Hz, Low Tone | High Tone).
fn choice_row(ui: &mut egui::Ui, cx: &mut Ctx, key: &str, opts: &[&str], sel: usize, active: bool, value: impl Fn(usize) -> Value) {
    let t = cx.t;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let w = 84.0;
    for (i, o) in opts.iter().enumerate() {
        let br = Rect::from_min_size(pos2(r.min.x + 42.0 + i as f32 * (w + 4.0), r.min.y + 3.0), vec2(w, 22.0));
        let resp = ui.interact(br, egui::Id::new(("es-choice", key, i)), Sense::click());
        let on = i == sel;
        ui.painter().rect_filled(
            br,
            4.0,
            if on {
                Color32::from_gray(0x4b)
            } else if resp.hovered() && active {
                t.hover
            } else {
                t.field_bg
            },
        );
        ui.painter().text(br.center(), Align2::CENTER_CENTER, *o, Tokens::ui(11.5), if active { t.text } else { t.text_faint });
        cx.auto.add(&format!("essentialSound.{key}.{i}"), br, o);
        if resp.clicked() && active && !on {
            cx.set(key, value(i), true);
        }
    }
}

fn button(ui: &mut egui::Ui, cx: &mut Ctx, r: Rect, text: &str, id: &str) -> bool {
    let t = cx.t;
    let resp = ui.interact(r, egui::Id::new(("es-btn", id)), Sense::click());
    ui.painter().rect_filled(r, 4.0, if resp.hovered() { Color32::from_gray(0x2a) } else { t.field_bg });
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, Color32::from_gray(0x4b)), StrokeKind::Inside);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, Tokens::ui(12.0), t.text);
    cx.auto.add(id, r, text);
    resp.clicked()
}

// ------------------------------------------------------------------------------------- icons

/// Audio-type glyphs, drawn from scratch: speech bubble, eighth note, burst, waves.
pub fn type_icon(p: &Painter, r: Rect, kind: AudioType, col: Color32) {
    let s = Stroke::new(1.4, col);
    let at = |x: f32, y: f32| pos2(r.min.x + x * r.width(), r.min.y + y * r.height());
    match kind {
        AudioType::Dialogue => {
            let b = Rect::from_min_max(at(0.08, 0.12), at(0.92, 0.68));
            p.rect_stroke(b, r.width() * 0.16, s, StrokeKind::Middle);
            p.add(egui::Shape::line(vec![at(0.3, 0.68), at(0.24, 0.9), at(0.48, 0.68)], s));
        }
        AudioType::Music => {
            p.circle_filled(at(0.32, 0.78), r.width() * 0.15, col);
            p.line_segment([at(0.45, 0.78), at(0.45, 0.1)], s);
            p.add(egui::Shape::line(vec![at(0.45, 0.1), at(0.78, 0.3), at(0.78, 0.48)], s));
        }
        AudioType::Sfx => {
            let c = r.center();
            for k in 0..12 {
                let a = k as f32 * std::f32::consts::TAU / 12.0;
                let len = if k % 2 == 0 { 0.5 } else { 0.3 } * r.width();
                p.line_segment([c, c + vec2(a.cos(), a.sin()) * len], s);
            }
        }
        AudioType::Ambience => {
            for (k, y) in [0.3f32, 0.55, 0.8].iter().enumerate() {
                let pts: Vec<_> = (0..=12)
                    .map(|i| {
                        let x = i as f32 / 12.0;
                        at(0.05 + x * 0.9, *y + 0.07 * (x * std::f32::consts::TAU + k as f32).sin())
                    })
                    .collect();
                p.add(egui::Shape::line(pts, s));
            }
        }
    }
}

/// Save preset: a downward arrow into a tray.
fn save_icon(p: &Painter, r: Rect, col: Color32) {
    let s = Stroke::new(1.4, col);
    let at = |x: f32, y: f32| pos2(r.min.x + x * r.width(), r.min.y + y * r.height());
    p.line_segment([at(0.5, 0.05), at(0.5, 0.62)], s);
    p.add(egui::Shape::line(vec![at(0.28, 0.42), at(0.5, 0.64), at(0.72, 0.42)], s));
    p.add(egui::Shape::line(vec![at(0.08, 0.6), at(0.08, 0.92), at(0.92, 0.92), at(0.92, 0.6)], s));
}
