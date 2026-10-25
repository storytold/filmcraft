//! Colour dialogs: Clip ▸ Modify ▸ Interpret Footage… (Pixel Aspect Ratio and Color Management)
//! and Sequence ▸ Color Management… (working space, wide gamut, Auto Tone Map Media). Each applies
//! one engine command on OK, so it is a single undo step. Automation ids: `colorDialog.space.<id>`,
//! `colorDialog.par.file`, `colorDialog.par.<num>x<den>` (the [`STANDARD_PIXEL_ASPECTS`]),
//! `colorDialog.working.<id>`, `colorDialog.wideGamut`, `colorDialog.autoToneMap`,
//! `colorDialog.ok`, `colorDialog.cancel`.
//!
//! [`STANDARD_PIXEL_ASPECTS`]: filmcraft_project::STANDARD_PIXEL_ASPECTS

use filmcraft_color::{ColorSpace, WorkingSpace};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::ColorDialog;

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
    // the first item's pixel aspect override, "file" when it uses the file's ratio
    let pixel_aspect = items
        .first()
        .and_then(|i| app.session.project.item(filmcraft_project::ItemId(*i)))
        .and_then(|it| it.as_media())
        .and_then(|m| m.interpret.par.and_then(filmcraft_project::checked_par))
        .map_or_else(|| "file".to_string(), |(n, d)| format!("{n}:{d}"));
    app.ui.color_dialog = Some(ColorDialog::Interpret { items, color_space: current, pixel_aspect, pixel_aspect_set: false, color_space_set: false });
}

/// The `[num, den]` of a dialog choice (`"num:den"`), None for "file" or anything else.
fn par_choice(choice: &str) -> Option<(u32, u32)> {
    let (n, d) = choice.split_once(':')?;
    filmcraft_project::checked_par((n.parse().ok()?, d.parse().ok()?))
}

/// The pixel aspect ratio the first item's file reports (square when it reports none).
fn file_par(app: &FilmcraftApp, items: &[u64]) -> (u32, u32) {
    items
        .first()
        .and_then(|i| app.session.project.item(filmcraft_project::ItemId(*i)))
        .and_then(|it| it.as_media())
        .and_then(|m| m.info.video.as_ref())
        .map_or((1, 1), |v| filmcraft_project::sane_par(v.par))
}

pub fn open_sequence(app: &mut FilmcraftApp) {
    let c = app.session.active_sequence().map(|q| q.settings.color).unwrap_or(filmcraft_color::ColorPipeline::REC709);
    app.ui.color_dialog = Some(ColorDialog::Sequence { working_space: c.working.id().into(), wide_gamut: c.wide_gamut, auto_tone_map: c.auto_tone_map });
}

fn radio(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, selected: bool, label: &str) -> bool {
    let r = ui.radio(selected, label);
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
        ColorDialog::Interpret { items, color_space, pixel_aspect, pixel_aspect_set, color_space_set } => {
            let detected = items
                .first()
                .and_then(|i| app.session.execute("media.colorInfo", json!({"item": i})).ok())
                .and_then(|v| v["detectedLabel"].as_str().map(str::to_string))
                .unwrap_or_else(|| "—".into());
            let from_file = file_par(app, items);
            let n = items.len();
            egui::Window::new(tl!("Interpret Footage"))
                .id(egui::Id::new("Interpret Footage"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new(if n == 1 { tlf!("Pixel Aspect Ratio · {n} item", n) } else { tlf!("Pixel Aspect Ratio · {n} items", n) }).strong(),
                    );
                    let file_label = crate::i18n::t(&filmcraft_project::pixel_aspect_label(from_file)).to_string();
                    if radio(app, ui, "colorDialog.par.file", pixel_aspect == "file", &tlf!("Use Pixel Aspect Ratio from File: {par}", par = file_label)) {
                        *pixel_aspect = "file".into();
                        *pixel_aspect_set = true;
                    }
                    ui.label(tl!("Conform to:"));
                    // the anamorphic squeezes: a 1.33x lens exactly (133:100) and the 4:3 pixel apart
                    for (label, (num, den)) in filmcraft_project::STANDARD_PIXEL_ASPECTS {
                        let choice = format!("{num}:{den}");
                        if radio(app, ui, &format!("colorDialog.par.{num}x{den}"), *pixel_aspect == choice, crate::i18n::t(label)) {
                            *pixel_aspect = choice;
                            *pixel_aspect_set = true;
                        }
                    }
                    if let Some(par) = par_choice(pixel_aspect)
                        && !filmcraft_project::STANDARD_PIXEL_ASPECTS.iter().any(|(_, p)| *p == par)
                    {
                        let label = filmcraft_project::pixel_aspect_label(par);
                        // the first item's own ratio: picking it applies it to every item
                        if radio(app, ui, &format!("colorDialog.par.{}x{}", par.0, par.1), true, &label) {
                            *pixel_aspect_set = true;
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(tl!("Color Management")).strong());
                    ui.label(tlf!("Media colour space (from file metadata): {detected}", detected));
                    ui.add_space(4.0);
                    ui.label(tl!("Override media colour space:"));
                    if radio(app, ui, "colorDialog.space.auto", color_space == "auto", tl!("Use file metadata")) {
                        *color_space = "auto".into();
                        *color_space_set = true;
                    }
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for cs in ColorSpace::ALL {
                            if radio(app, ui, &format!("colorDialog.space.{}", cs.id()), color_space == cs.id(), cs.label()) {
                                *color_space = cs.id().into();
                                *color_space_set = true;
                            }
                        }
                    });
                    let (ok, cancel) = buttons(app, ui);
                    // the dialog shows the first item's choices; each goes to every item only when
                    // the user picked it, and left out, each item keeps its own override
                    if ok && (*color_space_set || *pixel_aspect_set) {
                        let mut p = json!({"items": items});
                        if *color_space_set {
                            p["colorSpace"] = json!(color_space);
                        }
                        if *pixel_aspect_set {
                            p["pixelAspect"] = match par_choice(pixel_aspect) {
                                Some((num, den)) => json!([num, den]),
                                None => json!("file"),
                            };
                        }
                        apply = Some(("clip.interpretFootage", p));
                    }
                    close = ok || cancel;
                });
        }
        ColorDialog::Sequence { working_space, wide_gamut, auto_tone_map } => {
            egui::Window::new(tl!("Sequence Color Management"))
                .id(egui::Id::new("Sequence Color Management"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(340.0)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(tl!("Working Color Space")).strong());
                    for w in WorkingSpace::ALL {
                        if radio(app, ui, &format!("colorDialog.working.{}", w.id()), working_space == w.id(), w.label()) {
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
