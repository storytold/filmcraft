//! Colour dialogs: Clip ▸ Modify ▸ Interpret Footage… opens Modify Clip, with an Interpret Footage
//! tab (Frame Rate, Field Order, Alpha Channel) and a Color tab (the media
//! colour space), and Sequence ▸ Color Management… (working space, wide gamut, Auto Tone Map
//! Media). Each applies one engine command on OK, so it is a single undo step. With several
//! items selected, Modify Clip changes all of them, but only in the settings changed in the dialog;
//! a group whose setting differs between the items says so. Automation ids:
//! `colorDialog.tab.interpret`, `colorDialog.tab.color`, `colorDialog.rate.file`,
//! `colorDialog.rate.assume`, `colorDialog.rate.value` (typed), `colorDialog.rate.list` and, while
//! it is open, `colorDialog.rate.<fps>` (e.g. `colorDialog.rate.23.976`), `colorDialog.duration`,
//! `colorDialog.fields.file`, `colorDialog.alpha.ignore`, `colorDialog.alpha.invert`,
//! `colorDialog.space.list` and, while it is open, `colorDialog.space.auto` and
//! `colorDialog.space.<id>`, `colorDialog.working.<id>`, `colorDialog.wideGamut`,
//! `colorDialog.autoToneMap`, `colorDialog.ok`, `colorDialog.cancel`.

use filmcraft_color::{ColorSpace, WorkingSpace};
use filmcraft_project::MediaClip;
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::ColorDialog;

/// Width of the option column (the radio buttons) of the Interpret Footage groups.
const OPTION_W: f32 = 215.0;

/// The media of `items` (bins, sequences and missing items left out).
fn medias(app: &FilmcraftApp, items: &[u64]) -> Vec<MediaClip> {
    items.iter().filter_map(|i| app.session.project.item(filmcraft_project::ItemId(*i))).filter_map(|i| i.as_media()).cloned().collect()
}

/// How `m` is interpreted, as `clip.interpretFootage` parameters.
fn settings(m: &MediaClip) -> Value {
    json!({
        "colorSpace": m.interpret.color_space.map_or("auto", |c| c.id()),
        "frameRate": m.conform().map_or(json!("file"), |c| json!(c.assumed.as_f64())),
        "ignoreAlpha": m.interpret.ignore_alpha,
        "invertAlpha": m.interpret.invert_alpha,
    })
}

/// The settings the items differ in (`clip.interpretFootage` parameter names).
fn mixed_settings(ms: &[MediaClip]) -> Vec<String> {
    let all: Vec<Value> = ms.iter().map(settings).collect();
    let Some(first) = all.first() else { return Vec::new() };
    ["colorSpace", "frameRate", "ignoreAlpha", "invertAlpha"].into_iter().filter(|k| all.iter().any(|v| v[*k] != first[*k])).map(str::to_string).collect()
}

/// `values` as one label, or "Various" when they differ.
fn one_or_various(values: impl IntoIterator<Item = String>) -> String {
    let mut it = values.into_iter();
    let Some(first) = it.next() else { return String::new() };
    if it.all(|v| v == first) { first } else { tl!("Various").to_string() }
}

fn touch(touched: &mut Vec<String>, key: &str) {
    if !touched.iter().any(|k| k == key) {
        touched.push(key.to_string());
    }
}

pub fn open_interpret(app: &mut FilmcraftApp, params: &Value) {
    let items: Vec<u64> = match params.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).collect(),
        None => {
            let s = &app.session;
            let mut v: Vec<u64> = s.state.project_selection.iter().map(|i| i.0).collect();
            if v.is_empty()
                && let Some(q) = s.active_sequence()
            {
                v = s.state.selection.iter().filter_map(|c| q.find_item(*c).map(|(_, it)| it.item.0)).collect();
                v.dedup();
            }
            v
        }
    };
    let current = items
        .first()
        .and_then(|i| app.session.execute("media.colorInfo", json!({"item": i})).ok())
        .and_then(|v| v["override"].as_str().map(str::to_string))
        .unwrap_or_else(|| "auto".into());
    let ms = medias(app, &items);
    let mixed = mixed_settings(&ms);
    let m = ms.first();
    let assume = m.and_then(MediaClip::conform).map(|c| c.assumed);
    let file_rate = m.map(|m| m.info.frame_rate());
    let tab = match params.get("tab").and_then(Value::as_str) {
        Some("color") => "color",
        _ => "interpret",
    };
    app.ui.color_dialog = Some(ColorDialog::Interpret {
        items,
        color_space: current,
        assume_rate: assume.map(FrameRate::as_f64),
        tab: tab.into(),
        rate_text: assume.or(file_rate).map(FrameRate::label).unwrap_or_default(),
        ignore_alpha: m.is_some_and(|m| m.interpret.ignore_alpha),
        invert_alpha: m.is_some_and(|m| m.interpret.invert_alpha),
        mixed,
        touched: Vec::new(),
    });
}

/// The rate typed in Assume this frame rate (`25`, `23.976`, `24000/1001`), if usable.
fn parse_rate(text: &str) -> Option<FrameRate> {
    filmcraft_engine::export_tools::parse_rate(&json!(text.trim())).filter(|r| r.is_usable())
}

/// The field order the file's pixel format names (MPEG-2 and MXF report interlaced pictures).
fn file_field_order(m: &MediaClip) -> &'static str {
    let f = m.info.video.as_ref().map(|v| v.pixel_format.to_ascii_lowercase()).unwrap_or_default();
    if f.contains("upper field first") || f.contains("top field first") {
        tl!("Upper Field First")
    } else if f.contains("lower field first") || f.contains("bottom field first") {
        tl!("Lower Field First")
    } else {
        tl!("No Fields (Progressive Scan)")
    }
}

/// A titled group of the Interpret Footage tab; `mixed` notes that the selected items differ in it.
fn group(ui: &mut egui::Ui, title: &str, mixed: bool, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
        if mixed {
            ui.label(egui::RichText::new(tl!("(differs between the selected items)")).weak().italics());
        }
    });
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui);
    });
    ui.add_space(6.0);
}

/// The Interpret Footage tab's choices (a view of the dialog's draft).
struct Choices<'a> {
    assume: &'a mut Option<f64>,
    rate_text: &'a mut String,
    ignore_alpha: &'a mut bool,
    invert_alpha: &'a mut bool,
    mixed: &'a [String],
    /// The settings changed in the dialog (`clip.interpretFootage` parameter names).
    touched: &'a mut Vec<String>,
}

/// The Interpret Footage tab for the media `ms` (at least one; the draft starts from the first).
fn interpret_tab(app: &mut FilmcraftApp, ui: &mut egui::Ui, ms: &[MediaClip], c: Choices) {
    let Some(m) = ms.first() else { return };
    let moving = |m: &MediaClip| m.info.video.is_some() && !matches!(m.info.kind, filmcraft_media::MediaKind::Still);
    let any_video = ms.iter().any(|m| m.info.video.is_some());
    let has_rate = ms.iter().any(moving);
    let file_rate = ms.iter().find(|m| moving(m)).map_or_else(|| m.info.frame_rate(), |m| m.info.frame_rate());
    let mixed = |k: &str| c.mixed.iter().any(|m| m == k);
    if ms.len() > 1 {
        let n = ms.len();
        ui.label(tlf!("Changes apply to all {n} selected items; settings left as they are stay as each item has them.", n));
        ui.add_space(4.0);
    }
    // what the user does with the rate, not whether the draft changed: in a mixed selection the
    // draft starts as the first item's rate, and choosing it again applies it to all of them
    let mut rate_chosen = false;
    group(ui, tl!("Frame Rate"), mixed("frameRate"), |ui| {
        ui.add_enabled_ui(has_rate, |ui| {
            egui::Grid::new("interpret-rate").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                if radio(app, ui, "colorDialog.rate.file", c.assume.is_none(), tl!("Use Frame Rate from File:"), OPTION_W) {
                    *c.assume = None;
                    rate_chosen = true;
                }
                ui.label(one_or_various(ms.iter().filter(|m| moving(m)).map(|m| format!("{:.4}", m.info.frame_rate().as_f64()))));
                ui.end_row();
                if radio(app, ui, "colorDialog.rate.assume", c.assume.is_some(), tl!("Assume this frame rate:"), OPTION_W) {
                    if c.assume.is_none() {
                        *c.assume = Some(parse_rate(c.rate_text).unwrap_or(file_rate).as_f64());
                    }
                    rate_chosen = true;
                }
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(c.rate_text).desired_width(64.0));
                    app.auto.add("colorDialog.rate.value", r.rect, "Assumed frame rate");
                    // typing a rate chooses Assume this frame rate; text that is not a rate chooses nothing
                    if r.changed()
                        && let Some(rate) = parse_rate(c.rate_text)
                    {
                        *c.assume = Some(rate.as_f64());
                        rate_chosen = true;
                    }
                    let mut picked = None;
                    let list = craft_ui::ComboBox::from_id_salt("interpret-rate-list").selected_text("").width(20.0).show_ui(ui, |ui| {
                        for rate in FrameRate::COMMON {
                            let label = rate.label();
                            let on = c.assume.is_some_and(|f| (f - rate.as_f64()).abs() < 0.0005);
                            let r = ui.selectable_label(on, &label);
                            app.auto.add(&format!("colorDialog.rate.{label}"), r.rect, &label);
                            if r.clicked() {
                                picked = Some(rate);
                            }
                        }
                    });
                    app.auto.add("colorDialog.rate.list", list.response.rect, "Frame rates");
                    if let Some(rate) = picked {
                        *c.assume = Some(rate.as_f64());
                        *c.rate_text = rate.label();
                        rate_chosen = true;
                    }
                    ui.label(tl!("fps"));
                });
                ui.end_row();
                // how long the footage plays at the rate chosen
                let tc = one_or_various(ms.iter().filter(|m| moving(m)).map(|m| {
                    let file = m.info.frame_rate();
                    let playing = c.assume.and_then(|f| parse_rate(&f.to_string())).unwrap_or(file);
                    filmcraft_time::format_timecode_frames(file.frame_at(m.info.duration), playing, playing.supports_drop_frame())
                }));
                ui.label(tl!("Duration:"));
                let r = ui.label(&tc);
                app.auto.add("colorDialog.duration", r.rect, &tc);
                ui.end_row();
            });
        });
    });
    if rate_chosen {
        touch(c.touched, "frameRate");
    }
    group(ui, tl!("Field Order"), false, |ui| {
        let why = tl!("FilmCraft plays every frame as a whole picture; conforming the field order is not supported yet.");
        egui::Grid::new("interpret-fields").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.add_enabled_ui(any_video, |ui| {
                radio(app, ui, "colorDialog.fields.file", true, tl!("Use Field Order from File:"), OPTION_W);
            });
            let fields = one_or_various(ms.iter().filter(|m| m.info.video.is_some()).map(|m| file_field_order(m).to_string()));
            ui.label(&fields);
            ui.end_row();
            ui.add_enabled_ui(false, |ui| {
                ui.allocate_ui_with_layout(egui::vec2(OPTION_W, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.radio(false, tl!("Conform to:")).on_disabled_hover_text(why);
                });
            });
            ui.add_enabled_ui(false, |ui| {
                craft_ui::ComboBox::from_id_salt("interpret-fields-list")
                    .selected_text(&fields)
                    .width(200.0)
                    .show_ui(ui, |_| {})
                    .response
                    .on_disabled_hover_text(why);
            });
            ui.end_row();
        });
    });
    let has_alpha = ms.iter().any(|m| m.info.video.as_ref().is_some_and(|v| v.has_alpha));
    group(ui, tl!("Alpha Channel"), mixed("ignoreAlpha") || mixed("invertAlpha"), |ui| {
        ui.add_enabled_ui(has_alpha, |ui| {
            let r = ui.checkbox(c.ignore_alpha, tl!("Ignore Alpha Channel"));
            app.auto.add("colorDialog.alpha.ignore", r.rect, "Ignore Alpha Channel");
            if r.changed() {
                touch(c.touched, "ignoreAlpha");
            }
            let r = ui.add_enabled(!*c.ignore_alpha, egui::Checkbox::new(c.invert_alpha, tl!("Invert Alpha Channel")));
            app.auto.add("colorDialog.alpha.invert", r.rect, "Invert Alpha Channel");
            if r.changed() {
                touch(c.touched, "invertAlpha");
            }
        });
    });
}

pub fn open_sequence(app: &mut FilmcraftApp) {
    let c = app.session.active_sequence().map(|q| q.settings.color).unwrap_or(filmcraft_color::ColorPipeline::REC709);
    app.ui.color_dialog = Some(ColorDialog::Sequence { working_space: c.working.id().into(), wide_gamut: c.wide_gamut, auto_tone_map: c.auto_tone_map });
}

/// A radio button registered under `id`; `width` > 0 gives it a column of that width.
fn radio(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, selected: bool, label: &str, width: f32) -> bool {
    let r = if width > 0.0 {
        ui.allocate_ui_with_layout(egui::vec2(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.radio(selected, label)).inner
    } else {
        ui.radio(selected, label)
    };
    app.auto.add(id, r.rect, label);
    r.clicked()
}

fn buttons(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> (bool, bool) {
    let mut out = (false, false);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let c = ui.button(tl!("Cancel"));
        app.auto.add("colorDialog.cancel", c.rect, "Cancel");
        let o = ui.button(tl!("OK"));
        app.auto.add("colorDialog.ok", o.rect, "OK");
        out = (o.clicked(), c.clicked());
    });
    out
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.color_dialog.clone() else { return };
    let mut open = true;
    let mut apply: Option<(&'static str, Value)> = None;
    let mut close = false;
    match &mut d {
        ColorDialog::Interpret { items, color_space, assume_rate, tab, rate_text, ignore_alpha, invert_alpha, mixed, touched } => {
            let ms = medias(app, items);
            let detected = items
                .first()
                .and_then(|i| app.session.execute("media.colorInfo", json!({"item": i})).ok())
                .and_then(|v| v["detectedLabel"].as_str().map(str::to_string))
                .unwrap_or_else(|| "—".into());
            let n = items.len();
            egui::Window::new(tl!("Modify Clip"))
                .id(egui::Id::unique("Interpret Footage"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(520.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (id, label) in [("color", tl!("Color")), ("interpret", tl!("Interpret Footage"))] {
                            let r = ui.selectable_label(tab == id || (tab.is_empty() && id == "interpret"), label);
                            app.auto.add(&format!("colorDialog.tab.{id}"), r.rect, label);
                            if r.clicked() {
                                *tab = id.into();
                            }
                        }
                    });
                    ui.separator();
                    if tab == "color" {
                        ui.label(
                            egui::RichText::new(if n == 1 { tlf!("Color Management · {n} item", n) } else { tlf!("Color Management · {n} items", n) }).strong(),
                        );
                        ui.label(tlf!("Media colour space (from file metadata): {detected}", detected));
                        if mixed.iter().any(|k| k == "colorSpace") {
                            ui.label(egui::RichText::new(tl!("(differs between the selected items)")).weak().italics());
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(tl!("Override media colour space:"));
                            let auto = tl!("Use file metadata");
                            let shown = ColorSpace::ALL.iter().find(|cs| cs.id() == color_space).map_or(auto, |cs| cs.label());
                            let mut picked = None;
                            let list =
                                craft_ui::ComboBox::from_id_salt("interpret-color-space").selected_text(shown).width(240.0).height(360.0).show_ui(ui, |ui| {
                                    let r = ui.selectable_label(color_space == "auto", auto);
                                    app.auto.add("colorDialog.space.auto", r.rect, auto);
                                    if r.clicked() {
                                        picked = Some("auto");
                                    }
                                    for cs in ColorSpace::ALL {
                                        let r = ui.selectable_label(color_space == cs.id(), cs.label());
                                        app.auto.add(&format!("colorDialog.space.{}", cs.id()), r.rect, cs.label());
                                        if r.clicked() {
                                            picked = Some(cs.id());
                                        }
                                    }
                                });
                            app.auto.add("colorDialog.space.list", list.response.rect, "Media colour space");
                            if let Some(id) = picked {
                                *color_space = id.into();
                                touch(touched, "colorSpace");
                            }
                        });
                    } else if !ms.is_empty() {
                        let choices = Choices { assume: assume_rate, rate_text, ignore_alpha, invert_alpha, mixed, touched };
                        interpret_tab(app, ui, &ms, choices);
                    }
                    let (ok, cancel) = buttons(app, ui);
                    // only the settings changed here: the others stay as each item has them
                    if ok && !touched.is_empty() {
                        let draft = json!({
                            "colorSpace": color_space,
                            "frameRate": assume_rate.map_or(json!("file"), |f| json!(f)),
                            "ignoreAlpha": *ignore_alpha,
                            "invertAlpha": *invert_alpha,
                        });
                        let mut p = json!({"items": items});
                        for k in touched.iter() {
                            p[k.as_str()] = draft[k.as_str()].clone();
                        }
                        apply = Some(("clip.interpretFootage", p));
                    }
                    close = ok || cancel;
                });
        }
        ColorDialog::Sequence { working_space, wide_gamut, auto_tone_map } => {
            egui::Window::new(tl!("Sequence Color Management"))
                .id(egui::Id::unique("Sequence Color Management"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(340.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(tl!("Working Color Space")).strong());
                    for w in WorkingSpace::ALL {
                        if radio(app, ui, &format!("colorDialog.working.{}", w.id()), working_space == w.id(), w.label(), 0.0) {
                            *working_space = w.id().into();
                        }
                    }
                    ui.add_space(4.0);
                    let r = ui.checkbox(wide_gamut, tl!("Wide gamut color (composite in BT.2020)"));
                    app.auto.add("colorDialog.wideGamut", r.rect, "Wide gamut");
                    let r = ui.checkbox(auto_tone_map, tl!("Auto tone map media (HDR and log into SDR)"));
                    app.auto.add("colorDialog.autoToneMap", r.rect, "Auto tone map");
                    let (ok, cancel) = buttons(app, ui);
                    if ok {
                        apply =
                            Some(("sequence.colorSettings", json!({"workingSpace": working_space, "wideGamut": *wide_gamut, "autoToneMap": *auto_tone_map})));
                    }
                    close = ok || cancel;
                });
        }
    }
    app.ui.color_dialog = if close || !open { None } else { Some(d) };
    if let Some((cmd, p)) = apply
        && let Err(e) = app.session.execute(cmd, p)
    {
        app.ui.status = e.to_string();
    }
}
