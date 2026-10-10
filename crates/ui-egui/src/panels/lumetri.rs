//! Lumetri Color panel: Basic Correction, Creative, Curves (RGB + hue curves), Color Wheels,
//! HSL Secondary, Vignette — editing the selected clip's Lumetri effect through engine commands.
//!
//! In an HDR sequence (Rec. 2100 PQ / HLG working space) Basic Correction shows HDR White and
//! HDR Specular and Curves shows HDR Range (cd/m²): the sliders and curves then span 0 … that
//! many nits (see `filmcraft_color::grade`). Automation ids for those and HSL Secondary ▸ Refine:
//! `lumetri.param.<id>` (`hdr_white`, `hdr_specular`, `curves_hdr_range`, `hsl_denoise`,
//! `hsl_blur`); Color Match: `lumetri.match.comparisonView` (toggles the Program monitor's
//! Comparison View, whose reference frame Apply Match then uses).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{ClipId, EffectInstance, ParamValue};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

type Actions = Vec<Value>;

fn selected(app: &FilmcraftApp) -> Option<(ClipId, filmcraft_project::TrackItem)> {
    let seq = app.session.active_sequence()?;
    app.session.state.selection.iter().find_map(|c| {
        let (tid, it) = seq.find_item(*c)?;
        (seq.track(tid)?.kind == filmcraft_project::TrackKind::Video).then(|| (*c, it.clone()))
    })
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it)) = selected(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no clip selected)"));
        return;
    };
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 6.0))).id_salt("lumetri2"));
    let Some(idx) = it.effects.iter().position(|e| e.effect == "lumetri") else {
        bui.add_space(8.0);
        bui.label(egui::RichText::new(tlf!("Master · {name}", name = it.name)).color(t.text_dim));
        bui.add_space(8.0);
        if bui.button(tl!("Add Lumetri Color to clip")).clicked() {
            let _ = app.session.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}));
        }
        return;
    };
    let e = it.effects[idx].clone();
    // HDR mode: the sequence's working space is Rec. 2100 PQ or HLG
    let hdr = app.session.active_sequence().is_some_and(|q| q.settings.color.working.is_hdr());
    let mut auto_rows: Vec<(String, Rect, &'static str)> = Vec::new();
    let ph = app.session.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions: Actions = Vec::new();
    // header: clip context + fx toggle
    bui.horizontal(|ui| {
        ui.label(egui::RichText::new(tlf!("Master · {name}", name = it.name)).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let fx = ui.add(egui::Button::new(egui::RichText::new("fx").italics().color(if e.enabled { t.text } else { t.text_faint })).frame(false));
            if fx.clicked() {
                let _ = app.session.execute("effects.toggleEnabled", json!({"clip": clip.0, "index": idx}));
            }
        });
    });
    let set = |actions: &mut Actions, param: &str, v: Value| actions.push(json!({"clip": clip.0, "effect": idx, "param": param, "value": v}));
    let mut sections: Vec<&'static str> = Vec::new();
    let mut lut_actions: Vec<(&'static str, Value)> = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut bui, |ui| {
        if open_section(ui, app, &e, "Basic Correction", "basic_on", "basic", &mut sections) {
            lut_combo(ui, app, &e, "input_lut", tl!("Input LUT"), true, &mut lut_actions);
            sub(ui, &t, tl!("Color"));
            slider(ui, &e, mt, "temperature", tl!("Temperature"), Some(Gradient::Temp), &t, &mut actions, clip, idx);
            slider(ui, &e, mt, "tint", tl!("Tint"), Some(Gradient::Tint), &t, &mut actions, clip, idx);
            slider(ui, &e, mt, "saturation", tl!("Saturation"), None, &t, &mut actions, clip, idx);
            sub(ui, &t, tl!("Light"));
            for (id, label) in [
                ("exposure", tl!("Exposure")),
                ("contrast", tl!("Contrast")),
                ("highlights", tl!("Highlights")),
                ("shadows", tl!("Shadows")),
                ("whites", tl!("Whites")),
                ("blacks", tl!("Blacks")),
            ] {
                slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
            }
            if hdr {
                sub(ui, &t, tl!("HDR"));
                for (id, label) in [("hdr_white", tl!("HDR White")), ("hdr_specular", tl!("HDR Specular"))] {
                    let r = slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
                    auto_rows.push((format!("lumetri.param.{id}"), r, label));
                }
            }
        }
        if open_section(ui, app, &e, "Creative", "creative_on", "creative", &mut sections) {
            let look = match e.param("look").map(|p| &p.value) {
                Some(ParamValue::Choice(c)) => *c as usize,
                _ => 0,
            };
            let opts = filmcraft_project::find_effect("lumetri")
                .and_then(|d| d.param("look"))
                .and_then(|p| if let filmcraft_project::ParamKind::Choice(o) = p.kind { Some(o) } else { None })
                .unwrap_or(&[]);
            ui.horizontal(|ui| {
                ui.add_sized(vec2(110.0, 20.0), egui::Label::new(egui::RichText::new(tl!("Look")).color(t.text_dim)));
                let mut sel = look;
                egui::ComboBox::from_id_salt("lumetri-look")
                    .selected_text(opts.get(look).map_or(tl!("None"), |o| crate::i18n::t(o)))
                    .width((ui.available_width() - 40.0).max(60.0))
                    .show_ui(ui, |ui| {
                        for (i, o) in opts.iter().enumerate() {
                            if ui.selectable_value(&mut sel, i, crate::i18n::t(o)).changed() {
                                set(&mut actions, "look", json!(i));
                            }
                        }
                    });
            });
            lut_combo(ui, app, &e, "look_lut", tl!("Look LUT"), false, &mut lut_actions);
            slider(ui, &e, mt, "look_intensity", tl!("Intensity"), None, &t, &mut actions, clip, idx);
            sub(ui, &t, tl!("Adjustments"));
            for (id, label) in
                [("faded_film", tl!("Faded Film")), ("sharpen", tl!("Sharpen")), ("vibrance", tl!("Vibrance")), ("creative_sat", tl!("Saturation"))]
            {
                slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
            }
        }
        if open_section(ui, app, &e, "Curves", "curves_on", "curves", &mut sections) {
            sub(ui, &t, tl!("RGB Curves"));
            let key = egui::Id::new("lumetri-curve-channel");
            let mut ch: usize = ui.data(|d| d.get_temp(key)).unwrap_or(0);
            ui.horizontal(|ui| {
                for (i, c) in
                    [Color32::WHITE, Color32::from_rgb(230, 70, 70), Color32::from_rgb(80, 200, 90), Color32::from_rgb(80, 130, 250)].iter().enumerate()
                {
                    let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    ui.painter().circle_filled(r.center(), 6.0, *c);
                    if ch == i {
                        ui.painter().circle_stroke(r.center(), 8.5, Stroke::new(1.5, t.hot_text));
                    }
                    if resp.clicked() {
                        ch = i;
                    }
                }
            });
            ui.data_mut(|d| d.insert_temp(key, ch));
            let (id, col) = [
                ("curve_luma", Color32::WHITE),
                ("curve_red", Color32::from_rgb(230, 70, 70)),
                ("curve_green", Color32::from_rgb(80, 200, 90)),
                ("curve_blue", Color32::from_rgb(80, 130, 250)),
            ][ch];
            curve_editor(ui, &e, id, false, col, &t, &mut actions, clip, idx);
            if hdr {
                let r = slider(ui, &e, mt, "curves_hdr_range", tl!("HDR Range"), None, &t, &mut actions, clip, idx);
                auto_rows.push(("lumetri.param.curves_hdr_range".into(), r, "HDR Range"));
            }
            sub(ui, &t, tl!("Hue Saturation Curves"));
            for (id, label) in [
                ("hue_vs_sat", tl!("Hue vs Sat")),
                ("hue_vs_hue", tl!("Hue vs Hue")),
                ("hue_vs_luma", tl!("Hue vs Luma")),
                ("luma_vs_sat", tl!("Luma vs Sat")),
                ("sat_vs_sat", tl!("Sat vs Sat")),
            ] {
                ui.label(egui::RichText::new(label).color(t.text_dim));
                curve_editor(ui, &e, id, true, Color32::WHITE, &t, &mut actions, clip, idx);
            }
        }
        if open_section(ui, app, &e, "Color Wheels & Match", "wheels_on", "wheels", &mut sections) {
            let w = ((ui.available_width() - 24.0) / 3.0).min(120.0);
            ui.horizontal(|ui| {
                for (id, lid, label) in [
                    ("wheel_shadows", "wheel_shadows_l", tl!("Shadows")),
                    ("wheel_midtones", "wheel_midtones_l", tl!("Midtones")),
                    ("wheel_highlights", "wheel_highlights_l", tl!("Highlights")),
                ] {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(label).color(t.text_dim));
                        color_wheel(ui, &e, mt, id, lid, w, &t, &mut actions, clip, idx);
                    });
                }
            });
            match_controls(ui, app, &mut lut_actions);
        }
        if open_section(ui, app, &e, "HSL Secondary", "hsl_on", "hsl", &mut sections) {
            sub(ui, &t, tl!("Key"));
            slider(ui, &e, mt, "hsl_hue", tl!("Hue"), Some(Gradient::Hue), &t, &mut actions, clip, idx);
            for (id, label) in [
                ("hsl_hue_range", tl!("Hue Range")),
                ("hsl_sat_min", tl!("Saturation Min")),
                ("hsl_luma_min", tl!("Luma Min")),
                ("hsl_luma_max", tl!("Luma Max")),
                ("hsl_soft", tl!("Soften")),
            ] {
                slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
            }
            let mask = match e.param("hsl_show_mask").map(|p| &p.value) {
                Some(ParamValue::Choice(c)) => *c as usize,
                _ => 0,
            };
            ui.horizontal(|ui| {
                ui.add_sized(vec2(110.0, 20.0), egui::Label::new(egui::RichText::new(tl!("Show Mask")).color(t.text_dim)));
                let mut sel = mask;
                let modes = [tl!("Off"), tl!("Color/Gray"), tl!("Color/Black"), tl!("White/Black")];
                egui::ComboBox::from_id_salt("hsl-mask").selected_text(modes[mask.min(3)]).show_ui(ui, |ui| {
                    for (i, o) in modes.iter().enumerate() {
                        if ui.selectable_value(&mut sel, i, *o).changed() {
                            set(&mut actions, "hsl_show_mask", json!(i));
                        }
                    }
                });
            });
            sub(ui, &t, tl!("Refine"));
            for (id, label) in [("hsl_denoise", tl!("Denoise")), ("hsl_blur", tl!("Blur"))] {
                let r = slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
                auto_rows.push((format!("lumetri.param.{id}"), r, label));
            }
            sub(ui, &t, tl!("Correction"));
            slider(ui, &e, mt, "hsl_temp", tl!("Temperature"), Some(Gradient::Temp), &t, &mut actions, clip, idx);
            slider(ui, &e, mt, "hsl_tint", tl!("Tint"), Some(Gradient::Tint), &t, &mut actions, clip, idx);
            slider(ui, &e, mt, "hsl_sat", tl!("Saturation"), None, &t, &mut actions, clip, idx);
            slider(ui, &e, mt, "hsl_hue_shift", tl!("Hue Shift"), Some(Gradient::Hue), &t, &mut actions, clip, idx);
        }
        if open_section(ui, app, &e, "Vignette", "vignette_on", "vignette", &mut sections) {
            for (id, label) in [
                ("vignette_amount", tl!("Amount")),
                ("vignette_midpoint", tl!("Midpoint")),
                ("vignette_roundness", tl!("Roundness")),
                ("vignette_feather", tl!("Feather")),
            ] {
                slider(ui, &e, mt, id, label, None, &t, &mut actions, clip, idx);
            }
        }
        ui.add_space(20.0);
    });
    for (id, r, label) in auto_rows {
        if r.is_positive() {
            app.auto.add(&id, r, label);
        }
    }
    for a in actions {
        if let Err(err) = app.session.execute("effects.setParam", a) {
            app.ui.status = err.to_string();
        }
    }
    for sec in sections {
        if let Err(err) = app.session.execute("lumetri.setSection", json!({"clip": clip.0, "section": sec})) {
            app.ui.status = err.to_string();
        }
    }
    for (cmd, mut p) in lut_actions {
        if cmd == "view.display" {
            // Comparison View on / off (a Program monitor display mode)
            let d = p["display"].as_str().unwrap_or("composite").to_string();
            if let Some(Err(err)) = crate::panels::monitor_view::route(app, &format!("view.display.{d}"), &json!({"monitor": "program"})) {
                app.ui.status = err;
            }
            continue;
        }
        if p.get("browse").is_some() {
            let (cmd, clip) = (cmd.to_string(), clip.0);
            app.pick_ui(FileDialog::open_file(tl!("LUT"), &["cube", "3dl"]), move |app, paths| {
                let Some(path) = paths.into_iter().next() else { return };
                if let Err(err) = app.session.execute(&cmd, json!({"path": path, "clip": clip})) {
                    app.ui.status = err.to_string();
                }
            });
            continue;
        }
        p["clip"] = json!(clip.0);
        if let Err(err) = app.session.execute(cmd, p) {
            app.ui.status = err.to_string();
        }
    }
}

fn open_section(
    ui: &mut egui::Ui,
    app: &mut FilmcraftApp,
    e: &EffectInstance,
    name: &str,
    param: &str,
    sec: &'static str,
    out: &mut Vec<&'static str>,
) -> bool {
    let on = e.param(param).and_then(|p| p.value.as_bool()).unwrap_or(param != "hsl_on");
    let (open, toggled) = section(ui, app, name, on);
    if toggled {
        out.push(sec);
    }
    open
}

/// LUT menu: None, built-ins, the project's LUT library, Browse… (file dialog → `lut.import`).
fn lut_combo(ui: &mut egui::Ui, app: &mut FilmcraftApp, e: &EffectInstance, param: &str, label: &str, input: bool, out: &mut Vec<(&'static str, Value)>) {
    let t = app.tokens;
    let cur = match e.param(param).map(|p| &p.value) {
        Some(ParamValue::Text(s)) => s.clone(),
        _ => String::new(),
    };
    let cmd: &'static str = if input { "lumetri.setInputLut" } else { "lumetri.setLook" };
    let shown = filmcraft_render::luts::label(Some(&app.session.project), &cur);
    let mut entries: Vec<(String, String)> = vec![(String::new(), tl!("None").into())];
    let builtins: Vec<&filmcraft_render::luts::Builtin> = if input {
        filmcraft_render::luts::input_builtins().collect()
    } else {
        filmcraft_render::luts::builtins().iter().filter(|b| b.id.starts_with("look-")).collect()
    };
    entries.extend(builtins.into_iter().map(|b| (format!("builtin:{}", b.id), b.label.clone())));
    entries.extend(app.session.project.luts.iter().map(|l| (format!("lib:{}", l.id), l.name.clone())));
    let mut rect = Rect::NOTHING;
    ui.horizontal(|ui| {
        ui.add_sized(vec2(110.0, 20.0), egui::Label::new(egui::RichText::new(label).color(t.text_dim)));
        let r = egui::ComboBox::from_id_salt(("lumetri-lut", param)).selected_text(shown).truncate().width((ui.available_width() - 40.0).max(60.0)).show_ui(
            ui,
            |ui| {
                for (r, name) in &entries {
                    if ui.selectable_label(*r == cur, name).clicked() && *r != cur {
                        out.push((cmd, json!({"lut": r})));
                    }
                }
                ui.separator();
                if ui.selectable_label(false, tl!("Browse…")).clicked() {
                    out.push((cmd, json!({"browse": true})));
                }
            },
        );
        rect = r.response.rect;
    });
    app.auto.add(&format!("lumetri.{param}"), rect, label);
}

/// A section header; returns (expanded, switch clicked). `on` is the section's enable switch.
fn section(ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, on: bool) -> (bool, bool) {
    let t = app.tokens;
    let key = format!("lumetri2:{name}");
    let open = !app.ui.collapsed_fx.contains(&key) && (name == "Basic Correction" || app.ui.expanded_fx.contains(&key));
    ui.add_space(2.0);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.min.x + 6.0, r.center().y), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_dim,
    );
    ui.painter().text(pos2(r.min.x + 20.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(name), Tokens::semibold(13.0), t.text);
    // enable switch (section bypass)
    let sw = Rect::from_center_size(pos2(r.max.x - 18.0, r.center().y), vec2(26.0, 14.0));
    let sw_resp = ui.interact(sw.expand(3.0), egui::Id::new(("lumetri-switch", name)), Sense::click());
    ui.painter().rect_filled(sw, 7.0, if on { t.accent } else { Color32::from_rgb(0x55, 0x55, 0x55) });
    ui.painter().circle_filled(pos2(if on { sw.max.x - 7.0 } else { sw.min.x + 7.0 }, sw.center().y), 5.0, Color32::WHITE);
    app.auto.add(&format!("lumetri.switch.{name}"), sw.expand(3.0), &format!("{name} on/off"));
    app.auto.add(&format!("lumetri.section.{name}"), r, name);
    if sw_resp.clicked() {
        return (open, true);
    }
    if resp.clicked() {
        if open {
            app.ui.collapsed_fx.push(key.clone());
            app.ui.expanded_fx.retain(|k| *k != key);
        } else {
            app.ui.collapsed_fx.retain(|k| *k != key);
            app.ui.expanded_fx.push(key);
        }
    }
    (open, false)
}

fn sub(ui: &mut egui::Ui, t: &Tokens, name: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(name).color(t.text).strong().size(12.0));
}

#[derive(Clone, Copy)]
enum Gradient {
    Temp,
    Tint,
    Hue,
}

/// Spectrum slider: 1 pt track (or gradient), hollow ring knob, scrubby value at the right.
#[allow(clippy::too_many_arguments)]
fn slider(
    ui: &mut egui::Ui,
    e: &EffectInstance,
    mt: Tick,
    id: &str,
    label: &str,
    grad: Option<Gradient>,
    t: &Tokens,
    actions: &mut Actions,
    clip: ClipId,
    idx: usize,
) -> Rect {
    let Some(pd) = e.def().and_then(|d| d.param(id)) else { return Rect::NOTHING };
    let filmcraft_project::ParamKind::Float { min, max, soft_min, soft_max, decimals, .. } = pd.kind else { return Rect::NOTHING };
    let v = e.param(id).map(|p| p.f64_at(mt)).unwrap_or(pd.default.as_f64().unwrap_or(0.0));
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(row.min.x + 4.0, row.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), t.text_dim);
    let track = Rect::from_min_max(pos2(row.min.x + 112.0, row.center().y - 1.0), pos2(row.max.x - 58.0, row.center().y + 1.0));
    match grad {
        Some(g) => {
            let n = 24;
            for i in 0..n {
                let f0 = i as f32 / n as f32;
                let c = match g {
                    Gradient::Temp => lerp_col(Color32::from_rgb(70, 120, 230), Color32::from_rgb(240, 150, 50), f0),
                    Gradient::Tint => lerp_col(Color32::from_rgb(60, 190, 80), Color32::from_rgb(220, 70, 200), f0),
                    Gradient::Hue => {
                        let rgb = filmcraft_color::hsl_to_rgb(f0, 0.8, 0.5);
                        Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8)
                    }
                };
                let x0 = track.min.x + track.width() * f0;
                ui.painter().rect_filled(Rect::from_min_max(pos2(x0, track.min.y - 1.0), pos2(x0 + track.width() / n as f32 + 0.5, track.max.y + 1.0)), 0.0, c);
            }
        }
        None => {
            ui.painter().rect_filled(track, 0.0, Color32::from_rgb(0x6e, 0x6e, 0x6e));
        }
    }
    let f = ((v - soft_min) / (soft_max - soft_min)).clamp(0.0, 1.0) as f32;
    let kx = track.min.x + f * track.width();
    let hit = Rect::from_min_max(pos2(track.min.x - 6.0, row.min.y), pos2(track.max.x + 6.0, row.max.y));
    let resp = ui.interact(hit, egui::Id::new(("lslider", clip.0, id)), Sense::click_and_drag());
    ui.painter().circle_filled(pos2(kx, track.center().y), 5.0, t.panel_bg);
    ui.painter().circle_stroke(
        pos2(kx, track.center().y),
        5.0,
        Stroke::new(1.5, if resp.dragged() { t.hot_text } else { Color32::from_rgb(0xb0, 0xb0, 0xb0) }),
    );
    let mut out = None;
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let nf = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
        out = Some(soft_min + nf * (soft_max - soft_min));
    }
    if resp.double_clicked() {
        out = pd.default.as_f64();
    }
    let mut vui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(row.max.x - 52.0, row.min.y + 3.0), pos2(row.max.x, row.max.y - 3.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let (_, nv) =
        crate::widgets::hot_number(&mut vui, egui::Id::new(("lval", clip.0, id)), v, (soft_max - soft_min) / 300.0, (min, max), decimals as usize, "", t);
    if let Some(nv) = out.or(nv) {
        actions.push(json!({"clip": clip.0, "effect": idx, "param": id, "value": (nv * 10f64.powi(decimals as i32)).round() / 10f64.powi(decimals as i32)}));
    }
    row
}

fn lerp_col(a: Color32, b: Color32, f: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Curve editor: drag points, click the curve to add, double-click a point to remove.
#[allow(clippy::too_many_arguments)]
fn curve_editor(ui: &mut egui::Ui, e: &EffectInstance, id: &str, hue: bool, col: Color32, t: &Tokens, actions: &mut Actions, clip: ClipId, idx: usize) {
    let pts: Vec<[f32; 2]> = e.param(id).and_then(|p| p.value.as_curve().map(|c| c.to_vec())).unwrap_or_default();
    let w = ui.available_width().min(260.0);
    let h = if hue { w * 0.45 } else { w };
    let (area, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
    let p = ui.painter();
    p.rect_filled(area, 2.0, Color32::from_rgb(0x16, 0x16, 0x16));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [pos2(area.min.x + area.width() * f, area.min.y), pos2(area.min.x + area.width() * f, area.max.y)],
            Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2a, 0x2a)),
        );
        p.line_segment(
            [pos2(area.min.x, area.min.y + area.height() * f), pos2(area.max.x, area.min.y + area.height() * f)],
            Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2a, 0x2a)),
        );
    }
    if hue {
        let n = 36;
        for i in 0..n {
            let f0 = i as f32 / n as f32;
            let rgb = filmcraft_color::hsl_to_rgb(f0, 0.8, 0.5);
            let c = Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8);
            p.rect_filled(
                Rect::from_min_max(
                    pos2(area.min.x + area.width() * f0, area.max.y - 6.0),
                    pos2(area.min.x + area.width() * (f0 + 1.0 / n as f32) + 0.5, area.max.y),
                ),
                0.0,
                c,
            );
        }
    }
    let to_screen = |q: [f32; 2]| pos2(area.min.x + q[0] * area.width(), area.max.y - q[1] * area.height());
    let from_screen = |s: Pos2| [((s.x - area.min.x) / area.width()).clamp(0.0, 1.0), ((area.max.y - s.y) / area.height()).clamp(0.0, 1.0)];
    // curve polyline
    let draw_pts: Vec<[f32; 2]> = if hue && pts.is_empty() { vec![[0.0, 0.5], [1.0, 0.5]] } else { pts.clone() };
    let lut = if hue && !pts.is_empty() {
        let mut ext = Vec::new();
        for off in [-1.0f32, 0.0, 1.0] {
            for q in &pts {
                ext.push([(q[0] + off + 1.0) / 3.0, q[1]]);
            }
        }
        let l = filmcraft_render::effects::curve_lut(&ext, 3 * 128);
        l[128..256].to_vec()
    } else {
        filmcraft_render::effects::curve_lut(&draw_pts, 128)
    };
    let line: Vec<Pos2> = lut.iter().enumerate().map(|(i, y)| to_screen([i as f32 / 127.0, y.clamp(0.0, 1.0)])).collect();
    p.add(egui::Shape::line(line, Stroke::new(1.5, col)));
    let drag_key = egui::Id::new(("curve-drag", clip.0, id));
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_key));
    let mut new_pts = pts.clone();
    let mut changed = false;
    for (i, q) in pts.iter().enumerate() {
        let sp = to_screen(*q);
        p.circle_filled(sp, 4.0, if dragging == Some(i) { t.hot_text } else { col });
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let near =
            pts.iter().enumerate().map(|(i, q)| (i, to_screen(*q).distance(pos))).filter(|(_, d)| *d < 9.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        dragging = near.or_else(|| {
            new_pts.push(from_screen(pos));
            changed = true;
            Some(new_pts.len() - 1)
        });
    }
    if resp.dragged()
        && let (Some(i), Some(pos)) = (dragging, resp.interact_pointer_pos())
        && i < new_pts.len()
    {
        new_pts[i] = from_screen(pos);
        changed = true;
    }
    if resp.drag_stopped() {
        dragging = None;
    }
    if resp.double_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(i) = pts.iter().position(|q| to_screen(*q).distance(pos) < 9.0)
    {
        let endpoint = !hue && (i == 0 || i == pts.len() - 1);
        if !endpoint {
            new_pts.remove(i);
            changed = true;
        }
    } else if resp.double_clicked() && !hue {
        new_pts = vec![[0.0, 0.0], [1.0, 1.0]];
        changed = true;
    }
    ui.data_mut(|d| {
        if let Some(i) = dragging {
            d.insert_temp(drag_key, i);
        } else {
            d.remove::<usize>(drag_key);
        }
    });
    if changed {
        let v: Vec<Value> = new_pts.iter().map(|q| json!([q[0], q[1]])).collect();
        actions.push(json!({"clip": clip.0, "effect": idx, "param": id, "value": v}));
    }
    ui.add_space(6.0);
}

/// Colour wheel: hue ring, draggable puck (offset), lightness slider on the left.
#[allow(clippy::too_many_arguments)]
fn color_wheel(ui: &mut egui::Ui, e: &EffectInstance, mt: Tick, id: &str, lid: &str, size: f32, t: &Tokens, actions: &mut Actions, clip: ClipId, idx: usize) {
    let v = e.param(id).map(|p| p.vec2_at(mt)).unwrap_or_default();
    let (area, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let c = area.center() + vec2(6.0, 0.0);
    let r_out = size / 2.0 - 4.0;
    let r_in = r_out - 10.0;
    let p = ui.painter();
    let n = 72;
    let mut mesh = egui::Mesh::default();
    for i in 0..n {
        let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / n as f32 * std::f32::consts::TAU;
        let rgb = filmcraft_color::hsl_to_rgb(1.0 - (i as f32 / n as f32), 0.75, 0.5);
        let col = Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8);
        let base = mesh.vertices.len() as u32;
        for (a, r) in [(a0, r_in), (a0, r_out), (a1, r_out), (a1, r_in)] {
            mesh.colored_vertex(c + vec2(a.cos(), -a.sin()) * r, col);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    p.add(mesh);
    p.circle_filled(c, r_in - 1.0, Color32::from_rgb(0x16, 0x16, 0x16));
    p.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], Stroke::new(1.0, t.text_faint));
    p.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], Stroke::new(1.0, t.text_faint));
    let puck = c + vec2(v.x as f32, -v.y as f32) * (r_in - 4.0);
    p.circle_stroke(puck, 5.0, Stroke::new(1.5, Color32::WHITE));
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let d = (pos - c) / (r_in - 4.0);
        let len = d.length().min(1.0);
        let dir = if d.length() > 1e-6 { d / d.length() } else { d };
        actions.push(json!({"clip": clip.0, "effect": idx, "param": id, "value": [dir.x * len, -dir.y * len]}));
    }
    if resp.double_clicked() {
        actions.push(json!({"clip": clip.0, "effect": idx, "param": id, "value": [0.0, 0.0]}));
    }
    // lightness slider (vertical) at the wheel's left edge
    let l = e.param(lid).map(|p| p.f64_at(mt)).unwrap_or(0.0) as f32;
    let track = Rect::from_min_max(pos2(area.min.x, area.min.y + 8.0), pos2(area.min.x + 4.0, area.max.y - 8.0));
    p.rect_filled(track, 2.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
    let ky = track.center().y - l / 100.0 * track.height() / 2.0;
    p.circle_stroke(pos2(track.center().x, ky), 4.0, Stroke::new(1.5, Color32::from_rgb(0xb0, 0xb0, 0xb0)));
    let lr = ui.interact(track.expand2(vec2(6.0, 0.0)), egui::Id::new(("wl", clip.0, lid)), Sense::click_and_drag());
    if (lr.dragged() || lr.clicked())
        && let Some(pos) = lr.interact_pointer_pos()
    {
        let nv = ((track.center().y - pos.y) / (track.height() / 2.0) * 100.0).clamp(-100.0, 100.0);
        actions.push(json!({"clip": clip.0, "effect": idx, "param": lid, "value": nv}));
    }
    let _ = StrokeKind::Inside;
}

/// Color Match: reference frame (sequence timecode), Face Detection (skin-tone protection),
/// Apply Match (`lumetri.applyMatch`).
fn match_controls(ui: &mut egui::Ui, app: &mut FilmcraftApp, out: &mut Vec<(&'static str, Value)>) {
    let t = app.tokens;
    sub(ui, &t, tl!("Color Match"));
    let key = egui::Id::new("lumetri-match-ref");
    let fd_key = egui::Id::new("lumetri-match-face");
    let mut tc: String = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| "00:00:00:00".to_string());
    let mut face: bool = ui.data(|d| d.get_temp(fd_key)).unwrap_or(true);
    // Comparison View: the Program monitor shows the reference frame next to the current one, and
    // Apply Match uses that reference
    let comparing = app.ui.program.display_mode() == Some(crate::state::DisplayMode::Comparison);
    let b = ui.selectable_label(comparing, tl!("Comparison View"));
    app.auto.add("lumetri.match.comparisonView", b.rect, "Comparison View");
    if b.clicked() {
        out.push(("view.display", json!({"display": if comparing { "composite" } else { "comparison" }})));
    }
    let reference = app.ui.program.compare_ref.filter(|_| comparing).map(filmcraft_time::Tick);
    if let (Some(r), Some(q)) = (reference, app.session.active_sequence()) {
        tc = filmcraft_time::format_time(r, q.settings.frame_rate, q.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
    }
    ui.horizontal(|ui| {
        ui.add_sized(vec2(110.0, 20.0), egui::Label::new(egui::RichText::new(tl!("Reference")).color(t.text_dim)));
        let r = ui.add(egui::TextEdit::singleline(&mut tc).desired_width(96.0));
        app.auto.add("lumetri.match.reference", r.rect, "Reference timecode");
    });
    let r = ui.checkbox(&mut face, tl!("Face Detection (protect skin tones)"));
    app.auto.add("lumetri.match.faceDetection", r.rect, "Face Detection");
    let b = ui.button(tl!("Apply Match"));
    app.auto.add("lumetri.match.apply", b.rect, "Apply Match");
    if b.clicked() {
        let p = match reference {
            Some(r) => json!({"referenceTime": r.0, "faceDetection": face}),
            None => json!({"referenceTimecode": tc, "faceDetection": face}),
        };
        out.push(("lumetri.applyMatch", p));
    }
    ui.data_mut(|d| {
        d.insert_temp(key, tc);
        d.insert_temp(fd_key, face);
    });
}
