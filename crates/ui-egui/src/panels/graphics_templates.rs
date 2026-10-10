//! Essential Graphics ▸ Browse (the Graphics Templates library), the template / responsive-design
//! sections of the Edit tab, and the Export As Motion Graphics Template, Install Motion Graphics
//! Template and Replace Fonts in Projects dialogs (M10.7).
//!
//! The library lists the built-in templates (original FilmCraft designs) and the user's
//! `.fcgt` templates with thumbnails rendered by the engine, a search field and a category
//! filter. Click selects, double-click (or Apply) places the template at the playhead, dragging a
//! card onto the timeline places it there. Everything runs engine commands
//! (`graphics.template.*`, `graphics.setRoll`, `graphics.setResponsiveTime`, `graphics.pin`,
//! `file.replaceFonts`).
//!
//! Automation ids: `essentialGraphics.tab.browse|edit`, `gfxTemplates.search`,
//! `gfxTemplates.category`, `gfxTemplates.item.<template id>`, `gfxTemplates.apply`,
//! `gfxTemplates.install`, `gfxTemplates.remove`; Edit tab: `gfxTemplates.control.<control id>`,
//! `graphics.roll.mode`, `graphics.roll.<startOffScreen|endOffScreen|preroll|easeIn|easeOut|postroll>`,
//! `graphics.time.intro|outro`, `graphics.pin.to`, `graphics.pin.<left|top|right|bottom>`;
//! dialogs: `exportTemplate.name|category|control.<n>|ok|cancel`, `replaceFonts.from|to|ok|cancel`.
//! The UI state (`UiState::gfx_templates`) is serde, so agents can read and set it.

use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::graphic_templates::{LibraryEntry, library, thumbnail_cached};
use filmcraft_project::graphic::{self, layer_display_name, layer_indices};
use filmcraft_project::gtemplate::ControlKind;
use filmcraft_project::{ClipId, ParamValue, RollMode, TrackItem};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// One candidate property in the Export As Motion Graphics Template dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportControl {
    pub layer: usize,
    pub param: String,
    pub name: String,
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportDraft {
    pub clip: u64,
    pub name: String,
    pub category: String,
    pub description: String,
    pub controls: Vec<ExportControl>,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplaceDraft {
    pub from: String,
    pub to: String,
    pub error: String,
}

/// Frontend state (`UiState::gfx_templates`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GfxTemplatesState {
    /// Essential Graphics tab: "browse", "edit", or "" (Edit when a graphic is selected).
    pub tab: String,
    pub query: String,
    /// Category filter ("" = all).
    pub category: String,
    pub selected: Option<String>,
    pub export: Option<ExportDraft>,
    pub replace: Option<ReplaceDraft>,
}

type Elems = Vec<(String, Rect, String)>;

// ------------------------------------------------------------------------------------------------
// Library cache and thumbnails
// ------------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Lib(Arc<(f64, Vec<LibraryEntry>)>);

fn lib_id() -> egui::Id {
    egui::Id::new("gfx-template-library")
}

/// The template library, re-read at most every two seconds (or after [`invalidate`]).
fn cached_library(app: &FilmcraftApp, ctx: &egui::Context) -> Vec<LibraryEntry> {
    let now = ctx.input(|i| i.time);
    if let Some(Lib(l)) = ctx.data(|d| d.get_temp::<Lib>(lib_id()))
        && now - l.0 < 2.0
    {
        return l.1.clone();
    }
    let v = library(&app.session);
    ctx.data_mut(|d| d.insert_temp(lib_id(), Lib(Arc::new((now, v.clone())))));
    v
}

fn invalidate(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Lib>(lib_id()));
}

const THUMB_W: u32 = 192;

fn thumb_texture(ctx: &egui::Context, e: &LibraryEntry) -> egui::TextureHandle {
    let px = thumbnail_cached(&e.template, THUMB_W);
    let key = egui::Id::new(("gfx-thumb", e.template.id.as_str(), Arc::as_ptr(&px) as usize));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return t;
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([px.0 as usize, px.1 as usize], &px.2);
    let t = ctx.load_texture(format!("gfx-thumb-{}", e.template.id), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(key, t.clone()));
    t
}

// ------------------------------------------------------------------------------------------------
// Essential Graphics panel
// ------------------------------------------------------------------------------------------------

fn tab_button(ui: &mut egui::Ui, r: Rect, label: &str, on: bool, t: &Tokens) -> egui::Response {
    let resp = ui.interact(r, egui::Id::new(("eg-tab", label)), Sense::click());
    if on {
        ui.painter().line_segment([pos2(r.min.x + 6.0, r.max.y - 1.5), pos2(r.max.x - 6.0, r.max.y - 1.5)], Stroke::new(2.0, t.accent));
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(12.0), if on { t.text } else { t.text_dim });
    resp
}

/// The Essential Graphics panel: Browse and Edit tabs.
pub fn essential_graphics(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let tab = match app.ui.gfx_templates.tab.as_str() {
        "browse" => "browse",
        "edit" => "edit",
        _ if crate::panels::graphics::graphic_selected(app) => "edit",
        _ => "browse",
    };
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
    ui.painter().line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0, t.separator));
    let mut elems: Elems = Vec::new();
    for (i, (id, label)) in [("browse", tl!("Browse")), ("edit", tl!("Edit"))].into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(head.min.x + 8.0 + i as f32 * 80.0, head.min.y), vec2(76.0, 30.0));
        if tab_button(ui, r, label, tab == id, &t).clicked() {
            app.ui.gfx_templates.tab = id.into();
        }
        elems.push((format!("essentialGraphics.tab.{id}"), r, label.into()));
    }
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y + 1.0), rect.max);
    if tab == "edit" {
        crate::panels::graphics::properties(app, ui, body);
    } else {
        browse(app, ui, body);
    }
}

fn browse(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let lib = cached_library(app, &ctx);
    let mut elems: Elems = Vec::new();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut install = false;
    let mut cats: Vec<String> = lib.iter().map(|e| e.template.category.clone()).collect();
    cats.sort();
    cats.dedup();
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))).id_salt("gfx-browse"));
    b.set_clip_rect(rect);
    let st = &mut app.ui.gfx_templates;
    b.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut st.query).hint_text(tl!("Search templates")).desired_width((ui.available_width() - 230.0).max(110.0)));
        elems.push(("gfxTemplates.search".into(), r.rect, "Search".into()));
        let cur = if st.category.is_empty() { tl!("All Categories").to_string() } else { crate::i18n::t(&st.category).to_string() };
        let r = egui::ComboBox::from_id_salt("gfx-cat").selected_text(cur).width(130.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.category, String::new(), tl!("All Categories"));
            for c in &cats {
                ui.selectable_value(&mut st.category, c.clone(), crate::i18n::t(c));
            }
        });
        elems.push(("gfxTemplates.category".into(), r.response.rect, "Category".into()));
        let r = ui.button(tl!("Install…")).on_hover_text(tl!("Install Motion Graphics Template (.fcgt)"));
        elems.push(("gfxTemplates.install".into(), r.rect, "Install".into()));
        install = r.clicked();
    });
    b.add_space(6.0);
    let q = st.query.to_lowercase();
    let shown: Vec<&LibraryEntry> = lib
        .iter()
        .filter(|e| {
            let tt = &e.template;
            (st.category.is_empty() || tt.category == st.category)
                && (crate::i18n::matches_query(&tt.name, &q)
                    || crate::i18n::matches_query(&tt.category, &q)
                    || crate::i18n::matches_query(&tt.description, &q)
                    || tt.tags.iter().any(|x| crate::i18n::matches_query(x, &q)))
        })
        .collect();
    let selected = st.selected.clone();
    let card = vec2(160.0, 90.0 + 34.0);
    let avail = (b.available_width()).max(card.x);
    let cols = ((avail + 8.0) / (card.x + 8.0)).floor().max(1.0) as usize;
    let mut new_sel: Option<String> = None;
    let mut apply: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("gfx-browse-scroll").max_height((rect.max.y - b.cursor().min.y - 40.0).max(60.0)).auto_shrink([false, false]).show(
        &mut b,
        |ui| {
            if shown.is_empty() {
                ui.label(egui::RichText::new(tl!("No templates match.")).color(t.text_faint));
            }
            for row in shown.chunks(cols) {
                ui.horizontal(|ui| {
                    for e in row {
                        let (r, resp) = ui.allocate_exact_size(card, Sense::click_and_drag());
                        let on = selected.as_deref() == Some(e.template.id.as_str());
                        ui.painter().rect_filled(
                            r,
                            4.0,
                            if on {
                                t.row_selected
                            } else if resp.hovered() {
                                t.hover
                            } else {
                                t.field_bg
                            },
                        );
                        let ir = Rect::from_min_size(r.min + vec2(5.0, 5.0), vec2(card.x - 10.0, 84.0));
                        // dark checker-free backdrop so white titles read
                        ui.painter().rect_filled(ir, 2.0, Color32::from_rgb(0x2a, 0x30, 0x3a));
                        let tex = thumb_texture(ui.ctx(), e);
                        let sz = tex.size_vec2();
                        let k = (ir.width() / sz.x).min(ir.height() / sz.y);
                        let tr = Rect::from_center_size(ir.center(), sz * k);
                        ui.painter().image(tex.id(), tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        ui.painter().text(pos2(r.min.x + 6.0, ir.max.y + 6.0), Align2::LEFT_TOP, crate::i18n::t(&e.template.name), Tokens::ui(11.5), t.text);
                        let category = crate::i18n::t(&e.template.category);
                        let sub = if e.path.is_some() { tlf!("{category} · My Template", category) } else { category.to_string() };
                        ui.painter().text(pos2(r.min.x + 6.0, ir.max.y + 20.0), Align2::LEFT_TOP, sub, Tokens::ui(10.0), t.text_faint);
                        if on {
                            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
                        }
                        elems.push((format!("gfxTemplates.item.{}", e.template.id), r, e.template.name.clone()));
                        let resp = resp.on_hover_text(crate::i18n::t(&e.template.description));
                        if resp.drag_started() {
                            crate::panels::start_drag_template(ui, &e.template.id, &e.template.name);
                        }
                        if resp.double_clicked() {
                            apply = Some(e.template.id.clone());
                        } else if resp.clicked() {
                            new_sel = Some(e.template.id.clone());
                        }
                    }
                });
                ui.add_space(6.0);
            }
        },
    );
    b.add_space(4.0);
    let sel_entry = selected.as_ref().and_then(|s| lib.iter().find(|e| &e.template.id == s));
    b.horizontal(|ui| {
        let r = ui.add_enabled(sel_entry.is_some(), egui::Button::new(tl!("Apply")));
        elems.push(("gfxTemplates.apply".into(), r.rect, "Apply".into()));
        if r.clicked() {
            apply = selected.clone();
        }
        let user = sel_entry.is_some_and(|e| e.path.is_some());
        let r = ui.add_enabled(user, egui::Button::new(tl!("Remove")));
        elems.push(("gfxTemplates.remove".into(), r.rect, "Remove".into()));
        if r.clicked()
            && let Some(id) = &selected
        {
            actions.push(("graphics.template.remove".into(), json!({"template": id})));
        }
        if let Some(e) = sel_entry {
            ui.label(egui::RichText::new(tlf!("{n} editable properties", n = e.template.controls.len())).color(t.text_faint));
        }
    });
    if let Some(s) = new_sel {
        app.ui.gfx_templates.selected = Some(s);
    }
    if let Some(id) = apply {
        app.ui.gfx_templates.selected = Some(id.clone());
        actions.push(("graphics.template.apply".into(), json!({"template": id})));
    }
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if install {
        let _ = route(app, &ctx, "graphics.template.install", &json!({}));
    }
    for (cmd, p) in actions {
        match app.session.execute(&cmd, p) {
            Ok(v) => {
                if cmd == "graphics.template.apply" {
                    // edit the new graphic's properties
                    app.ui.gfx_templates.tab = "edit".into();
                    let _ = v;
                }
                invalidate(&ctx);
            }
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Edit tab sections (called from `graphics::properties`)
// ------------------------------------------------------------------------------------------------

fn hex(c: [f32; 4]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

fn row_label(ui: &mut egui::Ui, label: &str, t: &Tokens) -> egui::Ui {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(label), Tokens::ui(12.0), t.text_dim);
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(r.min.x + 130.0, r.min.y + 3.0), pos2(r.max.x - 6.0, r.max.y - 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    )
}

/// The template's editable properties (Essential Graphics ▸ Edit for a graphic made from a
/// template). Returns false when the graphic has no template.
pub fn template_controls(
    app: &FilmcraftApp,
    ui: &mut egui::Ui,
    clip: ClipId,
    it: &TrackItem,
    mt: Tick,
    autos: &mut Elems,
    actions: &mut Vec<(String, Value)>,
) -> bool {
    let t = app.tokens;
    let Some(link) = it.graphic.as_ref().and_then(|m| m.template.as_ref()) else { return false };
    ui.label(egui::RichText::new(tlf!("   Template: {name}", name = link.name)).color(t.text_faint).size(11.0));
    for c in &link.controls {
        let Some(e) = it.effects.iter().find(|e| graphic::is_layer(e) && filmcraft_project::gtemplate::layer_uid(e) == c.layer) else { continue };
        let id = format!("gfxTemplates.control.{}", c.id);
        let pv = e.params.get(&c.param).map(|p| p.value_at(mt));
        // multi-line text: the label on its own row, the field full width below it
        if c.kind == ControlKind::Text
            && c.param != "enabled"
            && let Some(ParamValue::Text(s0)) = &pv
            && s0.contains('\n')
        {
            ui.label(egui::RichText::new(format!("    {}", crate::i18n::t(&c.name))).color(t.text_dim).size(12.0));
            let mut s = s0.clone();
            let rows = s.lines().count().clamp(2, 8);
            let r = ui
                .horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.add(egui::TextEdit::multiline(&mut s).desired_rows(rows).desired_width(ui.available_width() - 8.0))
                })
                .inner;
            autos.push((id, r.rect, c.name.clone()));
            if s != *s0 {
                actions.push(("graphics.template.set".into(), json!({"clip": clip.0, "control": c.id, "value": s})));
            }
            continue;
        }
        let mut vui = row_label(ui, &c.name, &t);
        let set =
            |v: Value, actions: &mut Vec<(String, Value)>| actions.push(("graphics.template.set".into(), json!({"clip": clip.0, "control": c.id, "value": v})));
        match c.kind {
            _ if c.param == "enabled" => {
                let mut b = e.enabled;
                let r = vui.checkbox(&mut b, "");
                autos.push((id, r.rect, c.name.clone()));
                if r.changed() {
                    set(json!(b), actions);
                }
            }
            ControlKind::Text => {
                let mut s = match pv {
                    Some(ParamValue::Text(s)) => s,
                    _ => String::new(),
                };
                let before = s.clone();
                let r = vui.add(egui::TextEdit::singleline(&mut s).desired_width(vui.available_width() - 4.0));
                autos.push((id, r.rect, c.name.clone()));
                if s != before {
                    set(json!(s), actions);
                }
            }
            ControlKind::Color => {
                let col = pv.and_then(|v| v.as_color()).unwrap_or([1.0; 4]);
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                let mut c32 = Color32::from_rgb(q(col[0]), q(col[1]), q(col[2]));
                let r = egui::color_picker::color_edit_button_srgba(&mut vui, &mut c32, egui::color_picker::Alpha::Opaque);
                autos.push((id, r.rect, c.name.clone()));
                if r.changed() {
                    let [a, b2, cc, _] = c32.to_srgba_unmultiplied();
                    set(json!(hex([a as f32 / 255.0, b2 as f32 / 255.0, cc as f32 / 255.0, 1.0])), actions);
                }
            }
            ControlKind::Slider => {
                let mut v = pv.and_then(|v| v.as_f64()).unwrap_or(0.0);
                let (lo, hi) = (c.min.unwrap_or(0.0), c.max.unwrap_or(100.0));
                let r = vui.add(egui::Slider::new(&mut v, lo..=hi));
                autos.push((id, r.rect, c.name.clone()));
                if r.changed() {
                    set(json!(v), actions);
                }
            }
            ControlKind::Checkbox => {
                let mut b = pv.and_then(|v| v.as_bool()).unwrap_or(false);
                let r = vui.checkbox(&mut b, "");
                autos.push((id, r.rect, c.name.clone()));
                if r.changed() {
                    set(json!(b), actions);
                }
            }
            ControlKind::Font => {
                let fam = match pv {
                    Some(ParamValue::Text(s)) => s,
                    _ => String::new(),
                };
                let r = egui::ComboBox::from_id_salt(("gfx-tpl-font", clip.0, c.id.as_str())).selected_text(&fam).width(170.0).height(360.0).show_ui(
                    &mut vui,
                    |ui| {
                        for (f, _) in filmcraft_text::families() {
                            if ui.selectable_label(f == fam, &f).clicked() {
                                set(json!(f), actions);
                            }
                        }
                    },
                );
                autos.push((id, r.response.rect, c.name.clone()));
            }
            ControlKind::Position => {
                let v = pv.and_then(|v| v.as_vec2()).unwrap_or_default();
                let (mut x, mut y) = (v.x, v.y);
                let rx = vui.add(egui::DragValue::new(&mut x).speed(1.0).prefix("X "));
                let ry = vui.add(egui::DragValue::new(&mut y).speed(1.0).prefix("Y "));
                autos.push((format!("{id}.x"), rx.rect, c.name.clone()));
                autos.push((format!("{id}.y"), ry.rect, c.name.clone()));
                if rx.changed() || ry.changed() {
                    set(json!([x, y]), actions);
                }
            }
        }
    }
    true
}

/// Responsive Design – Time (intro / outro and roll / crawl), shown with no layer selected.
pub fn responsive_time(app: &FilmcraftApp, ui: &mut egui::Ui, clip: ClipId, it: &TrackItem, autos: &mut Elems, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let rate = app.session.sequence_rate();
    let m = it.graphic.as_deref().cloned().unwrap_or_default();
    let frames = |x: Tick| rate.frame_at(rate.snap_nearest(x)) as f64;
    for (key, label, v) in [("intro", tl!("Intro Duration"), m.intro), ("outro", tl!("Outro Duration"), m.outro)] {
        let mut vui = row_label(ui, label, &t);
        let mut f = frames(v);
        let r = vui.add(egui::DragValue::new(&mut f).speed(0.2).range(0.0..=100_000.0).suffix(" fr"));
        autos.push((format!("graphics.time.{key}"), r.rect, label.into()));
        if r.changed() {
            actions.push(("graphics.setResponsiveTime".into(), json!({"clip": clip.0, format!("{key}Frames"): f.round()})));
        }
    }
    let mut vui = row_label(ui, tl!("Roll"), &t);
    let mut mode = m.roll.mode;
    let r = egui::ComboBox::from_id_salt(("gfx-roll", clip.0)).selected_text(crate::i18n::t(mode.label())).width(120.0).show_ui(&mut vui, |ui| {
        for x in RollMode::ALL {
            ui.selectable_value(&mut mode, x, crate::i18n::t(x.label()));
        }
    });
    autos.push(("graphics.roll.mode".into(), r.response.rect, "Roll".into()));
    if mode != m.roll.mode {
        let name = match mode {
            RollMode::Off => "off",
            RollMode::Roll => "roll",
            RollMode::CrawlLeft => "crawlLeft",
            RollMode::CrawlRight => "crawlRight",
        };
        actions.push(("graphics.setRoll".into(), json!({"clip": clip.0, "mode": name})));
    }
    if m.roll.mode == RollMode::Off {
        return;
    }
    let mut vui = row_label(ui, "", &t);
    for (key, label, v) in
        [("startOffScreen", tl!("Start Off Screen"), m.roll.start_off_screen), ("endOffScreen", tl!("End Off Screen"), m.roll.end_off_screen)]
    {
        let mut b = v;
        let r = vui.checkbox(&mut b, label);
        autos.push((format!("graphics.roll.{key}"), r.rect, label.into()));
        if r.changed() {
            actions.push(("graphics.setRoll".into(), json!({"clip": clip.0, key: b})));
        }
    }
    for (key, label, v) in [
        ("preroll", tl!("Preroll"), m.roll.preroll),
        ("easeIn", tl!("Ease In"), m.roll.ease_in),
        ("easeOut", tl!("Ease Out"), m.roll.ease_out),
        ("postroll", tl!("Postroll"), m.roll.postroll),
    ] {
        let mut vui = row_label(ui, label, &t);
        let mut f = frames(v);
        let r = vui.add(egui::DragValue::new(&mut f).speed(0.2).range(0.0..=100_000.0).suffix(" fr"));
        autos.push((format!("graphics.roll.{key}"), r.rect, label.into()));
        if r.changed() {
            actions.push(("graphics.setRoll".into(), json!({"clip": clip.0, format!("{key}Frames"): f.round()})));
        }
    }
}

/// Responsive Design – Position for the selected layer: Pin To and the pinned edges.
pub fn responsive_position(
    app: &FilmcraftApp,
    ui: &mut egui::Ui,
    clip: ClipId,
    it: &TrackItem,
    layer: usize,
    autos: &mut Elems,
    actions: &mut Vec<(String, Value)>,
) {
    let t = app.tokens;
    let idx = layer_indices(&it.effects);
    let Some(&ei) = idx.get(layer) else { return };
    let pin = it.effects[ei].layer.as_ref().and_then(|x| x.pin.clone());
    let target_name = match pin.as_ref().map(|p| p.to) {
        None => tl!("None").to_string(),
        Some(filmcraft_project::PinTarget::Frame) => tl!("Video Frame").to_string(),
        Some(filmcraft_project::PinTarget::Layer(uid)) => (0..idx.len())
            .find(|&i| filmcraft_project::gtemplate::layer_uid(&it.effects[idx[i]]) == uid)
            .map(|i| layer_display_name(&it.effects[idx[i]], i))
            .unwrap_or_default(),
    };
    let mut vui = row_label(ui, tl!("Pin To"), &t);
    let r = egui::ComboBox::from_id_salt(("gfx-pin", clip.0, layer)).selected_text(&target_name).width(150.0).show_ui(&mut vui, |ui| {
        if ui.selectable_label(pin.is_none(), tl!("None")).clicked() {
            actions.push(("graphics.pin".into(), json!({"clip": clip.0, "layer": layer, "to": "none"})));
        }
        let on_frame = matches!(pin.as_ref().map(|p| p.to), Some(filmcraft_project::PinTarget::Frame));
        if ui.selectable_label(on_frame, tl!("Video Frame")).clicked() {
            actions.push(("graphics.pin".into(), json!({"clip": clip.0, "layer": layer, "to": "frame"})));
        }
        for (i, &e) in idx.iter().enumerate() {
            if i != layer {
                let n = layer_display_name(&it.effects[e], i);
                if ui.selectable_label(n == target_name, &n).clicked() {
                    actions.push(("graphics.pin".into(), json!({"clip": clip.0, "layer": layer, "to": i})));
                }
            }
        }
    });
    autos.push(("graphics.pin.to".into(), r.response.rect, "Pin To".into()));
    let Some(p) = pin else { return };
    let mut vui = row_label(ui, tl!("Pinned Edges"), &t);
    let mut edges = [p.left, p.top, p.right, p.bottom];
    let to = match p.to {
        filmcraft_project::PinTarget::Frame => json!("frame"),
        filmcraft_project::PinTarget::Layer(uid) => {
            json!((0..idx.len()).find(|&i| filmcraft_project::gtemplate::layer_uid(&it.effects[idx[i]]) == uid).unwrap_or(0))
        }
    };
    let mut changed = false;
    for (k, name) in ["left", "top", "right", "bottom"].iter().enumerate() {
        let r = vui.toggle_value(&mut edges[k], name[..1].to_ascii_uppercase());
        autos.push((format!("graphics.pin.{name}"), r.rect, format!("Pin {name} edge")));
        changed |= r.changed();
    }
    if changed {
        let list: Vec<&str> = ["left", "top", "right", "bottom"].iter().zip(edges).filter(|(_, on)| *on).map(|(n, _)| *n).collect();
        if list.is_empty() {
            actions.push(("graphics.pin".into(), json!({"clip": clip.0, "layer": layer, "to": "none"})));
        } else {
            actions.push(("graphics.pin".into(), json!({"clip": clip.0, "layer": layer, "to": to, "edges": list})));
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Menu routing and dialogs
// ------------------------------------------------------------------------------------------------

/// Menu / shortcut entry points of this module (None for other ids).
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let empty = params.as_object().is_none_or(|m| m.is_empty());
    match id {
        "graphics.template.install" if params.get("path").is_none() => {
            let path = app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("FilmCraft Graphics Template"), &["fcgt"]))?;
            let r = app.session.execute(id, json!({"path": path})).map_err(|e| e.to_string());
            invalidate(ctx);
            Some(r)
        }
        "graphics.template.export" | "file.exportGraphicsTemplate" if empty => {
            let clip = filmcraft_engine::graphics::target_clip(&app.session, &Value::Null);
            let Some(clip) = clip else { return Some(Err("select a graphic clip".into())) };
            let it = app.session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, i)| i.clone())?;
            let idx = layer_indices(&it.effects);
            let mut controls = Vec::new();
            for (i, &ei) in idx.iter().enumerate() {
                let e = &it.effects[ei];
                let n = layer_display_name(e, i);
                if e.effect == graphic::TEXT_LAYER {
                    controls.push(ExportControl { layer: i, param: "text".into(), name: n.clone(), on: true });
                    controls.push(ExportControl { layer: i, param: "font".into(), name: tlf!("{n} Font", n), on: false });
                    controls.push(ExportControl { layer: i, param: "size".into(), name: tlf!("{n} Size", n), on: false });
                }
                controls.push(ExportControl { layer: i, param: "fill_color".into(), name: tlf!("{n} Color", n), on: false });
                controls.push(ExportControl { layer: i, param: "position".into(), name: tlf!("{n} Position", n), on: false });
                controls.push(ExportControl { layer: i, param: "enabled".into(), name: tlf!("Show {n}", n), on: false });
            }
            app.ui.gfx_templates.export =
                Some(ExportDraft { clip: clip.0, name: it.name.clone(), category: tl!("My Templates").into(), controls, ..Default::default() });
            Some(Ok(json!({"dialog": "exportTemplate"})))
        }
        "file.replaceFonts" if empty => {
            let used = filmcraft_engine::graphic_templates::fonts_used(&app.session);
            let from = used.first().map(|f| f.0.clone()).unwrap_or_default();
            app.ui.gfx_templates.replace = Some(ReplaceDraft { from, to: filmcraft_text::fonts::DEFAULT_FAMILY.into(), error: String::new() });
            Some(Ok(json!({"dialog": "replaceFonts"})))
        }
        _ => None,
    }
}

/// Draw this module's dialogs.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    export_dialog(app, ctx);
    replace_dialog(app, ctx);
}

fn export_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.gfx_templates.export.clone() else { return };
    let mut elems: Elems = Vec::new();
    let mut action: Option<bool> = None;
    crate::dialog_style::Window::new(tl!("Export As Motion Graphics Template"))
        .id(egui::Id::new("Export As Motion Graphics Template"))
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("gfx-export-grid").num_columns(2).show(ui, |ui| {
                ui.label(tl!("Name:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(260.0));
                elems.push(("exportTemplate.name".into(), r.rect, "Name".into()));
                ui.end_row();
                ui.label(tl!("Category:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.category).desired_width(260.0));
                elems.push(("exportTemplate.category".into(), r.rect, "Category".into()));
                ui.end_row();
                ui.label(tl!("Description:"));
                let r = ui.add(egui::TextEdit::singleline(&mut d.description).desired_width(260.0));
                elems.push(("exportTemplate.description".into(), r.rect, "Description".into()));
                ui.end_row();
            });
            ui.separator();
            ui.label(tl!("Editable properties:"));
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (n, c) in d.controls.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let r = ui.checkbox(&mut c.on, "");
                        elems.push((format!("exportTemplate.control.{n}"), r.rect, c.name.clone()));
                        ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(220.0));
                        ui.label(egui::RichText::new(c.param.replace('_', " ")).weak());
                    });
                }
            });
            ui.label(egui::RichText::new(tl!("Saved as a FilmCraft graphics template (.fcgt) in your templates folder.")).weak().size(11.0));
            if !d.error.is_empty() {
                ui.colored_label(Color32::from_rgb(0xff, 0x80, 0x80), &d.error);
            }
            crate::dialog_style::actions(ui, |ui| {
                let ok = ui.add(crate::dialog_style::primary(tl!("Export")));
                elems.push(("exportTemplate.ok".into(), ok.rect, "Export".into()));
                let cancel = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
                elems.push(("exportTemplate.cancel".into(), cancel.rect, "Cancel".into()));
                if ok.clicked() {
                    action = Some(true);
                }
                if cancel.clicked() {
                    action = Some(false);
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    match action {
        Some(true) => {
            let controls: Vec<Value> = d.controls.iter().filter(|c| c.on).map(|c| json!({"layer": c.layer, "param": c.param, "name": c.name})).collect();
            let p = json!({"clip": d.clip, "name": d.name, "category": d.category, "description": d.description, "controls": controls});
            match app.session.execute("graphics.template.export", p) {
                Ok(v) => {
                    app.ui.gfx_templates.export = None;
                    app.ui.status = tlf!("Exported graphics template to {path}", path = v["path"].as_str().unwrap_or_default());
                    invalidate(ctx);
                }
                Err(e) => {
                    d.error = e.to_string();
                    app.ui.gfx_templates.export = Some(d);
                }
            }
        }
        Some(false) => app.ui.gfx_templates.export = None,
        None => app.ui.gfx_templates.export = Some(d),
    }
}

fn replace_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.gfx_templates.replace.clone() else { return };
    let used = filmcraft_engine::graphic_templates::fonts_used(&app.session);
    let mut elems: Elems = Vec::new();
    let mut action: Option<bool> = None;
    crate::dialog_style::Window::new(tl!("Replace Fonts in Projects"))
        .id(egui::Id::new("Replace Fonts in Projects"))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tl!("Fonts used in the project:"));
            egui::Grid::new("gfx-fonts-used").striped(true).num_columns(3).show(ui, |ui| {
                for (f, st, n) in &used {
                    let missing = filmcraft_text::resolve(f, st).missing;
                    ui.label(if missing {
                        egui::RichText::new(tlf!("{f} (missing)", f)).color(Color32::from_rgb(0xff, 0xa0, 0x60))
                    } else {
                        egui::RichText::new(f)
                    });
                    ui.label(st);
                    ui.label(format!("{n}"));
                    ui.end_row();
                }
            });
            ui.separator();
            let mut fams: Vec<String> = used.iter().map(|u| u.0.clone()).collect();
            fams.dedup();
            ui.horizontal(|ui| {
                ui.label(tl!("Replace:"));
                let r = egui::ComboBox::from_id_salt("gfx-rf-from").selected_text(&d.from).width(160.0).show_ui(ui, |ui| {
                    for f in &fams {
                        ui.selectable_value(&mut d.from, f.clone(), f);
                    }
                });
                elems.push(("replaceFonts.from".into(), r.response.rect, "Replace".into()));
                ui.label(tl!("with:"));
                let r = egui::ComboBox::from_id_salt("gfx-rf-to").selected_text(&d.to).width(160.0).height(360.0).show_ui(ui, |ui| {
                    for (f, _) in filmcraft_text::families() {
                        ui.selectable_value(&mut d.to, f.clone(), &f);
                    }
                });
                elems.push(("replaceFonts.to".into(), r.response.rect, "With".into()));
            });
            if !d.error.is_empty() {
                ui.colored_label(Color32::from_rgb(0xff, 0x80, 0x80), &d.error);
            }
            crate::dialog_style::actions(ui, |ui| {
                let ok = ui.add(crate::dialog_style::primary(tl!("OK")));
                elems.push(("replaceFonts.ok".into(), ok.rect, "OK".into()));
                let cancel = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
                elems.push(("replaceFonts.cancel".into(), cancel.rect, "Cancel".into()));
                if ok.clicked() {
                    action = Some(true);
                }
                if cancel.clicked() {
                    action = Some(false);
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    match action {
        Some(true) => match app.session.execute("file.replaceFonts", json!({"from": d.from, "to": d.to})) {
            Ok(v) => {
                app.ui.gfx_templates.replace = None;
                app.ui.status = tlf!("Replaced {n} font uses", n = v["replaced"]);
            }
            Err(e) => {
                d.error = e.to_string();
                app.ui.gfx_templates.replace = Some(d);
            }
        },
        Some(false) => app.ui.gfx_templates.replace = None,
        None => app.ui.gfx_templates.replace = Some(d),
    }
}
