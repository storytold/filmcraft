use super::*;
use filmcraft_project::graphic_design::char_style_at;

// ---------------------------------------------------------------------------------------------
// Properties / Essential Graphics
// ---------------------------------------------------------------------------------------------

const ROW: f32 = 28.0;

fn pv(e: &EffectInstance, id: &str, mt: Tick) -> ParamValue {
    e.params.get(id).map(|p| p.value_at(mt)).or_else(|| e.def().and_then(|d| d.param(id)).map(|d| d.default.clone())).unwrap_or(ParamValue::Float(0.0))
}

/// `#rrggbb`, or `#rrggbbaa` when `with_alpha` and the colour is not fully opaque.
fn color_hex([r, g, b, a]: [u8; 4], with_alpha: bool) -> String {
    if with_alpha && a != 255 { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}") }
}

fn color32(c: [f32; 4]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]))
}

/// The formatting knobs of an inline graphics number field.
struct NumberField<'a> {
    id: &'a str,
    speed: f64,
    range: (f64, f64),
    decimals: usize,
    unit: &'a str,
}

struct Ctx<'a> {
    clip: ClipId,
    layer: usize,
    /// The layer's index in the clip's effect list (what `effects.toggleAnimation` addresses).
    eidx: usize,
    e: &'a EffectInstance,
    mt: Tick,
    t: Tokens,
    actions: Vec<(String, Value)>,
    autos: Vec<(String, Rect, String)>,
    /// Characters selected with the Type tool in this layer: character properties apply to them.
    range: Option<(usize, usize)>,
}

/// Character properties a Type-tool selection can carry, with their `graphics.setCharStyle` names.
fn char_style_key(id: &str) -> Option<&'static str> {
    Some(match id {
        "font" => "font",
        "font_style" => "fontStyle",
        "size" => "size",
        "fill_color" => "color",
        "faux_bold" => "bold",
        "faux_italic" => "italic",
        "underline" => "underline",
        "strikethrough" => "strikethrough",
        "tracking" => "tracking",
        "baseline_shift" => "baselineShift",
        "caps" => "caps",
        _ => return None,
    })
}

impl Ctx<'_> {
    /// What a control shows for `id`: with a Type-tool selection, a character property reads the
    /// selected characters' own style (what [`Ctx::set`] writes there), so the Fill colour and the
    /// other character controls follow an edit made with text selected (#1996); otherwise, and for
    /// characters without an override, the layer's value. The style is read at the start of the
    /// selection, so a selection with mixed styles shows its first character's.
    fn value(&self, id: &str) -> ParamValue {
        let layer = pv(self.e, id, self.mt);
        let Some((a, _)) = self.range else { return layer };
        let Some(runs) = self.e.layer.as_ref().map(|x| x.runs.as_slice()) else { return layer };
        let s = char_style_at(runs, a);
        let own = match id {
            "font" => s.font.map(ParamValue::Text),
            "font_style" => s.font_style.map(ParamValue::Text),
            "size" => s.size.map(|v| ParamValue::Float(v as f64)),
            "fill_color" => s.fill.map(ParamValue::Color),
            "faux_bold" => s.faux_bold.map(ParamValue::Bool),
            "faux_italic" => s.faux_italic.map(ParamValue::Bool),
            "underline" => s.underline.map(ParamValue::Bool),
            "strikethrough" => s.strikethrough.map(ParamValue::Bool),
            "tracking" => s.tracking.map(|v| ParamValue::Float(v as f64)),
            "baseline_shift" => s.baseline_shift.map(|v| ParamValue::Float(v as f64)),
            "caps" => s.caps.map(ParamValue::Choice),
            _ => None,
        };
        own.unwrap_or(layer)
    }
    /// Whether the Style row's button for the boolean property `id` (bold, italic, underline) is on.
    fn flag(&self, id: &str) -> bool {
        self.value(id).as_bool().unwrap_or(false)
    }
    /// The caps the All Caps / Small Caps buttons show: 0 none, 1 all caps, 2 small caps.
    fn caps(&self) -> u32 {
        match self.value("caps") {
            ParamValue::Choice(c) => c,
            _ => 0,
        }
    }
    fn set(&mut self, id: &str, v: Value) {
        if let (Some((a, b)), Some(k)) = (self.range, char_style_key(id)) {
            self.actions.push(("graphics.setCharStyle".into(), json!({"clip": self.clip.0, "layer": self.layer, "start": a, "end": b, "style": {k: v}})));
            return;
        }
        self.actions.push(("graphics.set".into(), json!({"clip": self.clip.0, "layer": self.layer, "props": {id: v}})));
    }
    fn auto(&mut self, id: &str, r: Rect, label: &str) {
        self.autos.push((format!("graphics.prop.{id}"), r, label.to_string()));
    }
    fn row(&self, ui: &mut egui::Ui, label: &str) -> (Rect, egui::Ui) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), self.t.text_dim);
        let v = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(r.min.x + 130.0, r.min.y + 3.0), pos2(r.max.x - 6.0, r.max.y - 3.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        (r, v)
    }
    /// The stopwatch in front of an animatable property's label (#2358): turns keyframing on or
    /// off at the playhead, like the one in Effect Controls. While a property animates, changing
    /// it here adds or updates a keyframe at the playhead instead of changing it everywhere. A
    /// Type-tool selection edits the selected characters, which have no keyframes: no stopwatch.
    fn stopwatch(&mut self, ui: &egui::Ui, row: Rect, id: &str) {
        let animatable = self.e.def().and_then(|d| d.param(id)).is_some_and(|d| d.animatable);
        if !animatable || (self.range.is_some() && char_style_key(id).is_some()) {
            return;
        }
        let animated = self.e.params.get(id).is_some_and(|p| p.is_animated());
        let sw = Rect::from_center_size(pos2(row.min.x + 8.0, row.center().y), vec2(12.0, 12.0));
        let resp = ui.interact(sw.expand(2.0), egui::Id::unique(("gfxsw", self.clip.0, self.layer, id)), Sense::click()).on_hover_text(tl!("Toggle animation"));
        let tint = if animated {
            self.t.accent
        } else if resp.hovered() {
            self.t.text
        } else {
            self.t.text_faint
        };
        icons::paint(ui.painter(), sw, Icon::Stopwatch, tint);
        self.autos.push((format!("graphics.prop.{id}.stopwatch"), sw, "Toggle animation".to_string()));
        if resp.clicked() {
            self.actions.push(("effects.toggleAnimation".into(), json!({"clip": self.clip.0, "effect": self.eidx, "param": id})));
        }
    }
    /// Draw a labeled hot number for this clip/layer/`id`, register it for automation and emit the changed value through `Ctx::set`; `range` bounds typed and
    /// dragged input.
    fn number(&mut self, ui: &mut egui::Ui, label: &str, field: NumberField<'_>) {
        let NumberField { id, speed, range, decimals: dec, unit } = field;
        let v = self.value(id).as_f64().unwrap_or(0.0);
        let (r, mut vui) = self.row(ui, label);
        let (resp, nv) = crate::widgets::hot_number(
            &mut vui,
            label,
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id)),
            v,
            HotNumberOptions::new(speed, range, dec, unit, &self.t),
        );
        self.auto(id, resp.rect, label);
        self.stopwatch(ui, r, id);
        if let Some(nv) = nv {
            self.set(id, json!(nv));
        }
    }
    /// Draw an inline numeric field without a label row or stopwatch, register it for automation
    /// and emit the edited value through `Ctx::set` when it changes.
    fn number_in(&mut self, vui: &mut egui::Ui, label: &str, field: NumberField<'_>) {
        let NumberField { id, speed, range, decimals, unit } = field;
        let v = self.value(id).as_f64().unwrap_or(0.0);
        let (resp, nv) = crate::widgets::hot_number(
            vui,
            label,
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id)),
            v,
            HotNumberOptions::new(speed, range, decimals, unit, &self.t),
        );
        self.auto(id, resp.rect, id);
        if let Some(nv) = nv {
            self.set(id, json!(nv));
        }
    }
    /// Draw X and Y hot numbers for this clip/layer/`id` with independent ids and ±100,000 bounds, register both and emit both components when either changes.
    fn point(&mut self, ui: &mut egui::Ui, label: &str, id: &str) {
        let v = pv(self.e, id, self.mt).as_vec2().unwrap_or_default();
        let (r, mut vui) = self.row(ui, label);
        self.stopwatch(ui, r, id);
        let (rx, nx) = crate::widgets::hot_number(
            &mut vui,
            &format!("{label} X"),
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id, "x")),
            v.x,
            HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, " X", &self.t),
        );
        vui.add_space(10.0);
        let (ry, ny) = crate::widgets::hot_number(
            &mut vui,
            &format!("{label} Y"),
            egui::Id::unique(("gfxp", self.clip.0, self.layer, id, "y")),
            v.y,
            HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, " Y", &self.t),
        );
        self.auto(&format!("{id}.x"), rx.rect, label);
        self.auto(&format!("{id}.y"), ry.rect, label);
        if nx.is_some() || ny.is_some() {
            self.set(id, json!([nx.unwrap_or(v.x), ny.unwrap_or(v.y)]));
        }
    }
    fn check(&mut self, vui: &mut egui::Ui, id: &str, label: &str) -> bool {
        let mut b = self.value(id).as_bool().unwrap_or(false);
        let r = vui.checkbox(&mut b, label);
        self.auto(id, r.rect, label);
        if r.changed() {
            self.set(id, json!(b));
        }
        b
    }
    fn color(&mut self, vui: &mut egui::Ui, id: &str) {
        let c = self.value(id).as_color().unwrap_or([1.0; 4]);
        let mut c32 = color32(c);
        // gradient stops carry opacity (#3094); the other colours stay opaque (the layer has its own opacity)
        let alpha = if id.starts_with("gradient_") { egui::color_picker::Alpha::OnlyBlend } else { egui::color_picker::Alpha::Opaque };
        let r = egui::color_picker::color_edit_button_srgba(vui, &mut c32, alpha);
        self.auto(id, r.rect, id);
        if r.changed() {
            self.set(id, json!(color_hex(c32.to_srgba_unmultiplied(), alpha != egui::color_picker::Alpha::Opaque)));
        }
    }
    /// Draw a combo box for a choice parameter, register it for automation and emit the selected
    /// index through `Ctx::set` when it changes. An out-of-range current value displays as an
    /// empty selection.
    fn choice(&mut self, vui: &mut egui::Ui, id: &str, label: &str, opts: &[&str], width: f32) {
        let cur = match self.value(id) {
            ParamValue::Choice(c) => c as usize,
            _ => 0,
        };
        let mut sel = cur;
        let r = craft_ui::ComboBox::from_id_salt(("gfxc", self.clip.0, self.layer, id))
            .selected_text(opts.get(cur).map_or("", |o| crate::i18n::t(o)))
            .width(width)
            .show_ui(vui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    ui.selectable_value(&mut sel, i, crate::i18n::t(o));
                }
            });
        let response = crate::widgets::accessible_name(r.response, label);
        self.auto(id, response.rect, id);
        if sel != cur {
            self.set(id, json!(sel));
        }
    }
}

fn section(ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, t: &Tokens) -> bool {
    section_header(ui, app, name, t).0
}

/// A collapsible section's header row: whether the section is open, and the row.
fn section_header(ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, t: &Tokens) -> (bool, Rect) {
    ui.add_space(4.0);
    let key = format!("gfx:{name}");
    let open = !app.ui.collapsed_fx.contains(&key);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.min.x + 6.0, r.center().y), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_dim,
    );
    ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(name), Tokens::semibold(13.0), t.text);
    app.auto.add(&format!("graphics.section.{}", name.replace(' ', "")), r, name);
    if resp.clicked() {
        if open {
            app.ui.collapsed_fx.push(key);
        } else {
            app.ui.collapsed_fx.retain(|k| *k != key);
        }
    }
    (open, r)
}

/// A small code-drawn alignment glyph (original artwork: bars against an edge line).
fn align_glyph(p: &egui::Painter, r: Rect, how: &str, c: Color32) {
    let s = Stroke::new(1.2, c);
    let (x0, x1, y0, y1) = (r.min.x + 2.0, r.max.x - 2.0, r.min.y + 2.0, r.max.y - 2.0);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let bar = |a: Pos2, b: Pos2| p.rect_filled(Rect::from_two_pos(a, b), 0.0, c);
    match how {
        "left" => {
            p.line_segment([pos2(x0, y0), pos2(x0, y1)], s);
            bar(pos2(x0 + 2.0, y0 + 2.0), pos2(x1, y0 + 5.0));
            bar(pos2(x0 + 2.0, y1 - 5.0), pos2(cx + 1.0, y1 - 2.0));
        }
        "right" => {
            p.line_segment([pos2(x1, y0), pos2(x1, y1)], s);
            bar(pos2(x0, y0 + 2.0), pos2(x1 - 2.0, y0 + 5.0));
            bar(pos2(cx - 1.0, y1 - 5.0), pos2(x1 - 2.0, y1 - 2.0));
        }
        "hcenter" => {
            p.line_segment([pos2(cx, y0), pos2(cx, y1)], s);
            bar(pos2(x0 + 1.0, y0 + 2.0), pos2(x1 - 1.0, y0 + 5.0));
            bar(pos2(cx - 3.0, y1 - 5.0), pos2(cx + 3.0, y1 - 2.0));
        }
        "top" => {
            p.line_segment([pos2(x0, y0), pos2(x1, y0)], s);
            bar(pos2(x0 + 2.0, y0 + 2.0), pos2(x0 + 5.0, y1));
            bar(pos2(x1 - 5.0, y0 + 2.0), pos2(x1 - 2.0, cy + 1.0));
        }
        "bottom" => {
            p.line_segment([pos2(x0, y1), pos2(x1, y1)], s);
            bar(pos2(x0 + 2.0, y0), pos2(x0 + 5.0, y1 - 2.0));
            bar(pos2(x1 - 5.0, cy - 1.0), pos2(x1 - 2.0, y1 - 2.0));
        }
        "vcenter" => {
            p.line_segment([pos2(x0, cy), pos2(x1, cy)], s);
            bar(pos2(x0 + 2.0, y0 + 1.0), pos2(x0 + 5.0, y1 - 1.0));
            bar(pos2(x1 - 5.0, cy - 3.0), pos2(x1 - 2.0, cy + 3.0));
        }
        "dh" => {
            for x in [x0 + 1.0, cx - 1.5, x1 - 4.0] {
                bar(pos2(x, y0 + 3.0), pos2(x + 3.0, y1 - 3.0));
            }
        }
        "dv" => {
            for y in [y0 + 1.0, cy - 1.5, y1 - 4.0] {
                bar(pos2(x0 + 3.0, y), pos2(x1 - 3.0, y + 3.0));
            }
        }
        // paragraph alignment: four text lines
        a @ ("pl" | "pc" | "pr" | "pj") => {
            for (i, w) in [1.0f32, 0.6, 0.85, 0.5].iter().enumerate() {
                let w = if a == "pj" && i < 3 { 1.0 } else { *w } * (x1 - x0);
                let y = y0 + 1.0 + i as f32 * 3.2;
                let xa = match a {
                    "pc" => cx - w / 2.0,
                    "pr" => x1 - w,
                    _ => x0,
                };
                p.line_segment([pos2(xa, y), pos2(xa + w, y)], s);
            }
        }
        _ => {}
    }
}

fn glyph_button(ui: &mut egui::Ui, how: &str, tip: &str, on: bool, t: &Tokens) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
    if on {
        ui.painter().rect_filled(r, 3.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    align_glyph(ui.painter(), Rect::from_center_size(r.center(), vec2(16.0, 14.0)), how, if resp.hovered() || on { t.text } else { t.text_dim });
    resp.on_hover_text(tip)
}

/// The stroke some letter buttons draw over their letter: slanted for Faux Italic, through it for
/// Strikethrough.
fn letter_button_mark(ui: &egui::Ui, id: &str, r: &egui::Response, on: bool, t: &Tokens) {
    let c = r.rect.center();
    match id {
        "faux_italic" => {
            ui.painter().line_segment([c + vec2(-5.0, 6.0), c + vec2(5.0, -6.0)], Stroke::new(0.8, t.text_faint));
        }
        "strikethrough" => {
            let col = if on || r.hovered() { t.text } else { t.text_dim };
            ui.painter().line_segment([c + vec2(-6.0, 0.5), c + vec2(6.0, 0.5)], Stroke::new(1.0, col));
        }
        _ => {}
    }
}

fn letter_button(ui: &mut egui::Ui, text: &str, tip: &str, on: bool, t: &Tokens, font: egui::FontId) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
    if on {
        ui.painter().rect_filled(r, 3.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, text, font, if on || resp.hovered() { t.text } else { t.text_dim });
    resp.on_hover_text(tip)
}

/// The graphic editor (Properties panel / Essential Graphics ▸ Edit).
/// The Layers section of the Essential Graphics panel: new-layer buttons, arrange and delete,
/// and the front-first list of the clip's layers.
#[allow(clippy::too_many_arguments)]
fn layers_section(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    clip: ClipId,
    it: &TrackItem,
    sel: Option<usize>,
    autos: &mut Vec<(String, Rect, String)>,
    actions: &mut Vec<(String, Value)>,
) {
    let t = app.tokens;
    let idx = layer_indices(&it.effects);
    // ---- layers (front first)
    if section(ui, app, "Layers", &t) {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            for (label, key, tip, cmd, p) in [
                ("T", "NewTextLayer", tl!("New Text Layer"), "graphics.newText", json!({"clip": clip.0, "text": tl!("New Text")})),
                ("", "NewRectangle", tl!("New Rectangle"), "graphics.newShape", json!({"clip": clip.0, "shape": "rectangle"})),
                ("", "NewEllipse", tl!("New Ellipse"), "graphics.newShape", json!({"clip": clip.0, "shape": "ellipse", "size": [200, 200]})),
            ] {
                let r = letter_button(ui, label, tip, false, &t, Tokens::semibold(13.0));
                if label.is_empty() {
                    let ir = Rect::from_center_size(r.rect.center(), vec2(14.0, 14.0));
                    icons::paint(ui.painter(), ir, if key == "NewRectangle" { Icon::Rectangle } else { Icon::Ellipse }, t.text_dim);
                }
                autos.push((format!("graphics.{key}"), r.rect, tip.into()));
                if r.clicked() {
                    actions.push((cmd.into(), p));
                }
            }
            ui.add_space(8.0);
            if let Some(l) = sel {
                for (label, tip, to) in [("↑", tl!("Bring Forward"), "forward"), ("↓", tl!("Send Backward"), "backward")] {
                    let r = letter_button(ui, label, tip, false, &t, Tokens::ui(13.0));
                    autos.push((format!("graphics.arrange.{to}"), r.rect, tip.into()));
                    if r.clicked() {
                        actions.push(("graphics.arrangeLayer".into(), json!({"clip": clip.0, "layer": l, "to": to})));
                    }
                }
                let (dr, dresp) = ui.allocate_exact_size(vec2(24.0, 22.0), Sense::click());
                icons::paint(ui.painter(), dr.shrink(4.0), Icon::Trash, if dresp.hovered() { t.text } else { t.text_dim });
                autos.push(("graphics.deleteLayer".into(), dr, "Delete Layer".into()));
                if dresp.on_hover_text(tl!("Delete Layer")).clicked() {
                    actions.push(("graphics.deleteLayer".into(), json!({"clip": clip.0, "layer": l})));
                }
            }
        });
        for (li, &ei) in idx.iter().enumerate().rev() {
            let e = &it.effects[ei];
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let on = sel == Some(li);
            if on {
                ui.painter().rect_filled(r, 2.0, Color32::from_rgb(0x2e, 0x3d, 0x55));
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 2.0, t.hover);
            }
            let kind = if e.effect == graphic::TEXT_LAYER { "T" } else { "◆" };
            ui.painter().text(pos2(r.min.x + 22.0, r.center().y), Align2::CENTER_CENTER, kind, Tokens::semibold(12.0), t.text_dim);
            ui.painter().text(pos2(r.min.x + 38.0, r.center().y), Align2::LEFT_CENTER, layer_display_name(e, li), Tokens::ui(12.0), t.text);
            // visibility
            let er = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(16.0, 16.0));
            let eresp = ui.interact(er, egui::Id::unique(("gfx-eye", clip.0, li)), Sense::click());
            icons::paint(ui.painter(), er.shrink(2.0), if e.enabled { Icon::Eye } else { Icon::EyeOff }, t.text_dim);
            autos.push((format!("graphics.layers.{li}.visible"), er, "Toggle visibility".into()));
            autos.push((format!("graphics.layers.{li}"), r, layer_display_name(e, li)));
            if eresp.clicked() {
                actions.push(("graphics.set".into(), json!({"clip": clip.0, "layer": li, "props": {"enabled": !e.enabled}})));
            } else if resp.clicked() {
                actions.push(("graphics.selectLayer".into(), json!({"clip": clip.0, "layers": [li]})));
            }
        }
    }
}

/// The Text section header and its wrench: opens the Text Properties dialog and reports
/// whether the section itself is open.
#[allow(clippy::too_many_arguments)]
fn text_section_header(
    ui: &mut egui::Ui,
    app: &mut FilmcraftApp,
    clip: ClipId,
    l: usize,
    e: &EffectInstance,
    mt: Tick,
    autos: &mut Vec<(String, Rect, String)>,
) -> bool {
    let t = app.tokens;
    let (open, head) = section_header(ui, app, "Text", &t);
    // Text Properties: the layer type (point / paragraph) and text styling
    let wr = Rect::from_center_size(pos2(head.max.x - 16.0, head.center().y), vec2(22.0, 22.0));
    let wresp = ui.interact(wr, egui::Id::unique(("gfx-text-props", clip.0, l)), Sense::click());
    icons::paint(ui.painter(), wr.shrink(4.0), Icon::Wrench, if wresp.hovered() { t.text } else { t.text_dim });
    autos.push(("graphics.textProperties".into(), wr, "Text Properties".into()));
    if wresp.on_hover_text(tl!("Text Properties")).clicked() {
        let flag = |id: &str| pv(e, id, mt).as_bool().unwrap_or(false);
        let ligatures = flag("ligatures");
        app.ui.text_props_dialog = Some(TextPropsDialog {
            clip: clip.0,
            layer: l,
            paragraph: pv(e, "box_width", mt).as_f64().unwrap_or(0.0) > 0.0 && !flag("vertical"),
            vertical: flag("vertical"),
            ligatures,
            ligatures_was: ligatures,
        });
    }
    open
}

pub fn properties(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it)) = selected_graphic(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a graphic clip, or use the Type tool (T) on the Program monitor"));
        let br = Rect::from_center_size(rect.center() + vec2(0.0, 30.0), vec2(150.0, 26.0));
        let resp = ui.put(br, egui::Button::new(tl!("Create new graphic")));
        app.auto.add("graphics.createNew", br, "Create new graphic");
        if resp.clicked()
            && let Err(e) = app.session.execute("graphics.newText", json!({"text": "New Text"}))
        {
            app.ui.status = e.to_string();
        }
        return;
    };
    let ph = app.session.playhead();
    let mt = it.effect_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let idx = layer_indices(&it.effects);
    let sel = app.session.state.graphic_layers.first().copied().filter(|l| *l < idx.len());
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut autos: Vec<(String, Rect, String)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 6.0))).id_salt("gfx-props"));
    bui.set_clip_rect(rect);
    egui::ScrollArea::vertical().id_salt("gfx-props-scroll").auto_shrink([false, false]).show(&mut bui, |ui| {
        // header
        let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
        let lc = app.session.prefs.labels.rgb(it.label);
        ui.painter().rect_filled(Rect::from_center_size(pos2(hr.min.x + 8.0, hr.center().y), vec2(12.0, 12.0)), 2.0, Color32::from_rgb(lc[0], lc[1], lc[2]));
        ui.painter().text(pos2(hr.min.x + 22.0, hr.center().y), Align2::LEFT_CENTER, &it.name, Tokens::semibold(12.5), t.text);
        // ---- layers (front first)
        layers_section(app, ui, clip, &it, sel, &mut autos, &mut actions);
        // ---- template properties (a graphic made from a graphics template)
        if it.graphic.as_ref().is_some_and(|m| m.template.is_some()) && section(ui, app, "Template Properties", &t) {
            crate::panels::graphics_templates::template_controls(app, ui, clip, &it, mt, &mut autos, &mut actions);
        }
        let Some(l) = sel else {
            if section(ui, app, "Responsive Design - Time", &t) {
                crate::panels::graphics_templates::responsive_time(app, ui, clip, &it, &mut autos, &mut actions);
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tl!("Select a layer to edit its properties.")).color(t.text_faint));
            return;
        };
        let e = &it.effects[idx[l]];
        // a Type-tool selection in this layer: character properties apply to the selected characters
        let range = app.ui.gfx_edit.as_ref().filter(|ed| ed.clip == clip.0 && ed.layer == l && ed.caret != ed.anchor).and_then(|ed| {
            let text = match pv(e, "text", mt) {
                ParamValue::Text(s) => s,
                _ => return None,
            };
            let ch = |b: usize| text.get(..b.min(text.len())).map_or(0, |s| s.chars().count());
            Some((ch(ed.caret.min(ed.anchor)), ch(ed.caret.max(ed.anchor))))
        });
        let mut cx = Ctx { clip, layer: l, eidx: idx[l], e, mt, t, actions: Vec::new(), autos: Vec::new(), range };
        if section(ui, app, "Responsive Design - Position", &t) {
            crate::panels::graphics_templates::responsive_position(app, ui, clip, &it, l, &mut cx.autos, &mut cx.actions);
        }
        // ---- align and transform
        if section(ui, app, "Align and Transform", &t) {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                for (how, tip) in [
                    ("left", tl!("Align Left")),
                    ("hcenter", tl!("Align Center Horizontally")),
                    ("right", tl!("Align Right")),
                    ("top", tl!("Align Top")),
                    ("vcenter", tl!("Align Center Vertically")),
                    ("bottom", tl!("Align Bottom")),
                ] {
                    let r = glyph_button(ui, how, tip, false, &t);
                    cx.autos.push((format!("graphics.align.{how}"), r.rect, tip.into()));
                    if r.clicked() {
                        cx.actions.push(("graphics.align".into(), json!({"clip": clip.0, "align": how})));
                    }
                }
                ui.add_space(6.0);
                for (how, axis, tip) in [("dh", "horizontal", tl!("Distribute Horizontally")), ("dv", "vertical", tl!("Distribute Vertically"))] {
                    let r = glyph_button(ui, how, tip, false, &t);
                    cx.autos.push((format!("graphics.distribute.{axis}"), r.rect, tip.into()));
                    if r.clicked() {
                        cx.actions.push(("graphics.distribute".into(), json!({"clip": clip.0, "axis": axis, "layers": (0..idx.len()).collect::<Vec<_>>()})));
                    }
                }
            });
            cx.point(ui, tl!("Position"), "position");
            cx.point(ui, tl!("Anchor point"), "anchor");
            cx.number(ui, tl!("Scale"), NumberField { id: "scale", speed: 0.5, range: (0.0, 100_000.0), decimals: 0, unit: " %" });
            cx.number(ui, tl!("Rotation"), NumberField { id: "rotation", speed: 0.5, range: (-36_000.0, 36_000.0), decimals: 1, unit: " °" });
            cx.number(ui, tl!("Opacity"), NumberField { id: "opacity", speed: 0.5, range: (0.0, 100.0), decimals: 0, unit: " %" });
        }
        // ---- text
        let text_open = e.effect == graphic::TEXT_LAYER && text_section_header(ui, app, clip, l, e, mt, &mut cx.autos);
        if text_open {
            // source text (multi-line), committed as it is typed
            let mut s = match pv(e, "text", mt) {
                ParamValue::Text(s) => s,
                _ => String::new(),
            };
            let before = s.clone();
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let r = crate::widgets::accessible_name(
                    ui.add(egui::TextEdit::multiline(&mut s).desired_rows(2).desired_width(ui.available_width() - 8.0).font(Tokens::ui(12.0))),
                    "Source Text",
                );
                cx.autos.push(("graphics.sourceText".into(), r.rect, "Source Text".into()));
            });
            if s != before {
                cx.actions.push(("graphics.setText".into(), json!({"clip": clip.0, "layer": l, "text": s, "merge": true})));
            }
            let family = match cx.value("font") {
                ParamValue::Text(f) => f,
                _ => String::new(),
            };
            let style = match cx.value("font_style") {
                ParamValue::Text(f) => f,
                _ => String::new(),
            };
            let fams = filmcraft_text::picker_families();
            let (_, mut vui) = cx.row(ui, tl!("Font"));
            let r = craft_ui::ComboBox::from_id_salt(("gfx-font", clip.0, l)).selected_text(&family).width(170.0).height(400.0).show_ui(&mut vui, |ui| {
                for (f, _) in &fams {
                    if ui.selectable_label(*f == family, f).clicked() {
                        cx.set("font", json!(f));
                    }
                }
            });
            let response = crate::widgets::accessible_name(r.response, "Font");
            cx.auto("font", response.rect, "Font");
            let styles: Vec<String> = fams.iter().find(|(f, _)| f.eq_ignore_ascii_case(&family)).map(|(_, s)| s.clone()).unwrap_or_default();
            let (_, mut vui) = cx.row(ui, tl!("Font Style"));
            let r = craft_ui::ComboBox::from_id_salt(("gfx-style", clip.0, l)).selected_text(&style).width(170.0).show_ui(&mut vui, |ui| {
                for st in styles.iter().map(String::as_str).chain(["Bold", "Italic", "Bold Italic"]).filter({
                    let mut seen = Vec::new();
                    move |x: &&str| {
                        if seen.contains(x) {
                            false
                        } else {
                            seen.push(*x);
                            true
                        }
                    }
                }) {
                    if ui.selectable_label(st == style, st).clicked() {
                        cx.set("font_style", json!(st));
                    }
                }
            });
            let response = crate::widgets::accessible_name(r.response, "Font Style");
            cx.auto("font_style", response.rect, "Font Style");
            cx.number(ui, tl!("Font Size"), NumberField { id: "size", speed: 0.5, range: (1.0, 2000.0), decimals: 0, unit: "" });
            let align = match pv(e, "align", mt) {
                ParamValue::Choice(c) => c,
                _ => 0,
            };
            let (_, mut vui) = cx.row(ui, tl!("Alignment"));
            for (i, (how, tip)) in
                [("pl", tl!("Left Align Text")), ("pc", tl!("Center Align Text")), ("pr", tl!("Right Align Text")), ("pj", tl!("Justify"))].iter().enumerate()
            {
                let r = glyph_button(&mut vui, how, tip, align == i as u32, &t);
                cx.auto(&format!("align.{i}"), r.rect, tip);
                if r.clicked() {
                    cx.set("align", json!(i));
                }
            }
            cx.number(ui, tl!("Tracking"), NumberField { id: "tracking", speed: 1.0, range: (-1000.0, 10_000.0), decimals: 0, unit: "" });
            cx.number(ui, tl!("Leading"), NumberField { id: "leading", speed: 0.5, range: (-5000.0, 5000.0), decimals: 0, unit: "" });
            cx.number(ui, tl!("Baseline Shift"), NumberField { id: "baseline_shift", speed: 0.5, range: (-5000.0, 5000.0), decimals: 0, unit: "" });
            let vertical = pv(e, "vertical", mt).as_bool().unwrap_or(false);
            let (_, mut vui) = cx.row(ui, tl!("Orientation"));
            let mut value = vertical;
            let response = vui.checkbox(&mut value, tl!("Vertical Text"));
            cx.auto("vertical", response.rect, "Vertical Text");
            if response.changed() {
                cx.set("vertical", json!(value));
            }
            cx.number(ui, tl!("Text Box Width"), NumberField { id: "box_width", speed: 2.0, range: (0.0, 100_000.0), decimals: 0, unit: "" });
            cx.number(ui, tl!("Text Box Height"), NumberField { id: "box_height", speed: 2.0, range: (0.0, 100_000.0), decimals: 0, unit: "" });
            let (_, mut vui) = cx.row(ui, tl!("Style"));
            let flag = |id: &str| cx.flag(id);
            let caps = cx.caps();
            for (id, label, tip, on, font) in [
                ("faux_bold", "T", tl!("Faux Bold"), flag("faux_bold"), Tokens::semibold(14.0)),
                ("faux_italic", "T", tl!("Faux Italic"), flag("faux_italic"), egui::FontId::new(14.0, egui::FontFamily::Proportional)),
                ("all_caps", "TT", tl!("All Caps"), caps == 1, Tokens::ui(12.0)),
                ("small_caps", "Tt", tl!("Small Caps"), caps == 2, Tokens::ui(12.0)),
                ("underline", "U", tl!("Underline"), flag("underline"), Tokens::ui(13.0)),
                ("strikethrough", "S", tl!("Strikethrough"), flag("strikethrough"), Tokens::ui(13.0)),
            ] {
                let r = letter_button(&mut vui, label, tip, on, &t, font);
                letter_button_mark(&vui, id, &r, on, &t);
                cx.auto(id, r.rect, tip);
                if r.clicked() {
                    match id {
                        "all_caps" => cx.set("caps", json!(if caps == 1 { 0 } else { 1 })),
                        "small_caps" => cx.set("caps", json!(if caps == 2 { 0 } else { 2 })),
                        _ => cx.set(id, json!(!on)),
                    }
                }
            }
            let (_, mut vui) = cx.row(ui, tl!("OpenType"));
            cx.check(&mut vui, "kerning", tl!("Kerning"));
            cx.check(&mut vui, "ligatures", tl!("Ligatures"));
        }
        if e.effect == graphic::SHAPE_LAYER && section(ui, app, "Shape", &t) {
            let (_, mut vui) = cx.row(ui, tl!("Shape"));
            cx.choice(&mut vui, "shape", tl!("Shape"), graphic::SHAPE_OPTS, 120.0);
            cx.point(ui, tl!("Size"), "size");
            cx.number(ui, tl!("Corner Radius"), NumberField { id: "corner_radius", speed: 0.5, range: (0.0, 10_000.0), decimals: 0, unit: "" });
            cx.number(ui, tl!("Polygon Sides"), NumberField { id: "sides", speed: 0.05, range: (3.0, 64.0), decimals: 0, unit: "" });
        }
        // ---- appearance
        if section(ui, app, "Appearance", &t) {
            let (_, mut vui) = cx.row(ui, tl!("Fill"));
            let fill_on = cx.check(&mut vui, "fill", "");
            cx.color(&mut vui, "fill_color");
            if fill_on {
                vui.add_space(6.0);
                cx.choice(&mut vui, "fill_kind", tl!("Fill Type"), graphic::FILL_KIND_OPTS, 130.0);
            }
            let fill_kind = match pv(e, "fill_kind", mt) {
                ParamValue::Choice(c) => c,
                _ => 0,
            };
            if fill_on && fill_kind == 1 {
                let (_, mut vui) = cx.row(ui, tl!("Gradient"));
                cx.color(&mut vui, "gradient_start");
                cx.color(&mut vui, "gradient_end");
                cx.number(ui, tl!("   Angle"), NumberField { id: "gradient_angle", speed: 0.5, range: (-3600.0, 3600.0), decimals: 0, unit: " °" });
            }
            for (on, col, w, kind, label, width_label, type_label) in [
                ("stroke", "stroke_color", "stroke_width", "stroke_type", tl!("Stroke"), tl!("Stroke Width"), tl!("Stroke Type")),
                ("stroke2", "stroke2_color", "stroke2_width", "stroke2_type", tl!("Stroke 2"), tl!("Stroke 2 Width"), tl!("Stroke 2 Type")),
            ] {
                let (_, mut vui) = cx.row(ui, label);
                cx.check(&mut vui, on, "");
                cx.color(&mut vui, col);
                vui.add_space(6.0);
                cx.number_in(&mut vui, width_label, NumberField { id: w, speed: 0.2, range: (0.0, 1000.0), decimals: 1, unit: "" });
                vui.add_space(6.0);
                cx.choice(&mut vui, kind, type_label, graphic::STROKE_OPTS, 70.0);
            }
            let (_, mut vui) = cx.row(ui, tl!("Background"));
            let bg = cx.check(&mut vui, "background", "");
            cx.color(&mut vui, "background_color");
            if bg {
                cx.number(ui, tl!("   Opacity"), NumberField { id: "background_opacity", speed: 0.5, range: (0.0, 100.0), decimals: 0, unit: " %" });
                cx.number(ui, tl!("   Size"), NumberField { id: "background_size", speed: 0.5, range: (0.0, 1000.0), decimals: 0, unit: "" });
                cx.number(ui, tl!("   Corner Radius"), NumberField { id: "background_radius", speed: 0.5, range: (0.0, 1000.0), decimals: 0, unit: "" });
            }
            let (_, mut vui) = cx.row(ui, tl!("Shadow"));
            let sh = cx.check(&mut vui, "shadow", "");
            cx.color(&mut vui, "shadow_color");
            if sh {
                cx.number(ui, tl!("   Opacity"), NumberField { id: "shadow_opacity", speed: 0.5, range: (0.0, 100.0), decimals: 0, unit: " %" });
                cx.number(ui, tl!("   Angle"), NumberField { id: "shadow_angle", speed: 0.5, range: (-36_000.0, 36_000.0), decimals: 0, unit: " °" });
                cx.number(ui, tl!("   Distance"), NumberField { id: "shadow_distance", speed: 0.5, range: (0.0, 1000.0), decimals: 0, unit: "" });
                cx.number(ui, tl!("   Size"), NumberField { id: "shadow_size", speed: 0.5, range: (0.0, 1000.0), decimals: 0, unit: "" });
                cx.number(ui, tl!("   Blur"), NumberField { id: "shadow_blur", speed: 0.5, range: (0.0, 1000.0), decimals: 0, unit: "" });
            }
        }
        actions.append(&mut cx.actions);
        autos.append(&mut cx.autos);
    });
    for (id, r, label) in autos {
        app.auto.add(&id, r, &label);
    }
    let (down, press) = ui.ctx().input(|i| (i.pointer.any_down(), i.pointer.press_start_time()));
    let key = egui::Id::unique("gfx-props-drag-step");
    let begun = ui.ctx().data(|d| d.get_temp::<Option<f64>>(key)).flatten();
    let mut wrote = false;
    for (cmd, mut p) in actions {
        // a drag of a property is one undo step: the first change of each press begins a new one,
        // the rest fold into it (like Effect Controls, #201 / #887)
        if matches!(cmd.as_str(), "graphics.set" | "graphics.setCharStyle") && down {
            p["merge"] = json!(true);
            p["begin"] = json!(begun != press);
            wrote = true;
        }
        if let Err(e) = app.session.execute(&cmd, p) {
            app.ui.status = e.to_string();
        }
    }
    if down && (wrote || begun.is_some()) {
        ui.ctx().data_mut(|d| d.insert_temp(key, press));
    } else if !down {
        ui.ctx().data_mut(|d| d.remove::<Option<f64>>(key));
    }
}

/// Text Properties (the wrench in the Text section): Text Layer Type and Text Styling.
pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.text_props_dialog.clone() else { return };
    let mut elems: Vec<(String, Rect, &str)> = Vec::new();
    let mut close = crate::widgets::escape_closes(ctx);
    let mut ok = false;
    let name = |paragraph: bool| if paragraph { tl!("Paragraph Text") } else { tl!("Point Text") };
    let id = egui::Id::unique("gfx-text-properties");
    egui::Window::new(tl!("Text Properties")).id(id).collapsible(false).resizable(false).centered(ctx).show(ctx, |ui| {
        ui.label(tl!("Text Layer Type"));
        let r = craft_ui::ComboBox::from_id_salt("gfx-text-type").selected_text(name(d.paragraph)).width(190.0).show_ui(ui, |ui| {
            for paragraph in [false, true] {
                // vertical text has no box to wrap in
                let r = ui.add_enabled_ui(!(paragraph && d.vertical), |ui| ui.selectable_label(d.paragraph == paragraph, name(paragraph))).inner;
                elems.push((format!("graphics.textProperties.type.{}", if paragraph { "paragraph" } else { "point" }), r.rect, name(paragraph)));
                if r.clicked() {
                    d.paragraph = paragraph;
                }
            }
        });
        elems.push(("graphics.textProperties.type".into(), r.response.rect, "Text Layer Type"));
        ui.add_space(6.0);
        ui.label(tl!("Text Styling"));
        let r = ui.checkbox(&mut d.ligatures, tl!("Ligatures"));
        elems.push(("graphics.textProperties.ligatures".into(), r.rect, "Ligatures"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let r = ui.button(tl!("Cancel"));
            elems.push(("graphics.textProperties.cancel".into(), r.rect, "Cancel"));
            close |= r.clicked();
            let r = ui.button(tl!("OK"));
            elems.push(("graphics.textProperties.ok".into(), r.rect, "OK"));
            ok |= r.clicked();
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, l);
    }
    if ok {
        let target = json!({"clip": d.clip, "layer": d.layer});
        let mut run = |cmd: &str, extra: Value| {
            let mut p = target.clone();
            if let (Some(p), Some(x)) = (p.as_object_mut(), extra.as_object()) {
                p.extend(x.clone());
            }
            if let Err(e) = app.session.execute(cmd, p) {
                app.ui.status = e.to_string();
            }
        };
        // a layer already of that type is left alone (no undo step)
        run("graphics.setTextType", json!({"type": if d.paragraph { "paragraph" } else { "point" }}));
        if d.ligatures != d.ligatures_was {
            run("graphics.set", json!({"props": {"ligatures": d.ligatures}}));
        }
        // the text being typed into may have been rewritten
        app.ui.gfx_edit = None;
    }
    app.ui.text_props_dialog = if ok || close { None } else { Some(d) };
}

#[cfg(test)]
mod tests {
    use filmcraft_project::graphic_design::{CharStyle, LayerExtra, apply_char_style};

    use super::*;

    /// With text selected, the Properties controls show the selected characters' own style, not the
    /// layer's (which an edit with a selection leaves unchanged), so the Fill colour and the RGB
    /// fields follow the change (#1996).
    #[test]
    fn controls_read_the_selected_characters_style() {
        let mut e = graphic::new_text_layer("Night Drive", Vec2::new(100.0, 100.0), 72.0);
        let (layer_fill, layer_size) = (pv(&e, "fill_color", Tick(0)).as_color(), pv(&e, "size", Tick(0)).as_f64());
        assert!(layer_fill.is_some_and(|c| c != [1.0, 0.0, 0.0, 1.0]) && layer_size == Some(72.0), "{layer_fill:?} {layer_size:?}");
        let red = CharStyle {
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            size: Some(120.0),
            faux_bold: Some(true),
            underline: Some(true),
            caps: Some(1),
            ..Default::default()
        };
        e.layer = Some(Box::new(LayerExtra { runs: apply_char_style(&[], 11, 6, 11, &red), ..Default::default() }));
        let cx = |range| Ctx {
            clip: ClipId(1),
            layer: 0,
            eidx: 0,
            e: &e,
            mt: Tick(0),
            t: Tokens::for_kind(crate::theme::ThemeKind::Dark),
            actions: Vec::new(),
            autos: Vec::new(),
            range,
        };
        // "Drive" selected: its own colour, size and bold
        let sel = cx(Some((6, 11)));
        assert_eq!(sel.value("fill_color").as_color(), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(sel.value("size").as_f64(), Some(120.0));
        assert_eq!(sel.value("faux_bold").as_bool(), Some(true));
        // the Style row's buttons: T (bold), U and TT on, T (italic) and Tt off
        assert!(sel.flag("faux_bold") && sel.flag("underline") && !sel.flag("faux_italic"));
        assert_eq!(sel.caps(), 1);
        // a property the selection doesn't override, and the layer's position: the layer's values
        assert_eq!(sel.value("tracking").as_f64(), Some(0.0));
        assert_eq!(sel.value("position").as_vec2(), Some(Vec2::new(100.0, 100.0)));
        // "Night" selected, or nothing: the layer's colour and size
        for c in [cx(Some((0, 5))), cx(None)] {
            assert_eq!(c.value("fill_color").as_color(), layer_fill);
            assert_eq!(c.value("size").as_f64(), layer_size);
            assert!(!c.flag("faux_bold") && !c.flag("underline"));
            assert_eq!(c.caps(), 0);
        }
    }

    #[test]
    fn colour_hex_carries_alpha_only_when_asked_and_not_opaque() {
        assert_eq!(color_hex([255, 0, 16, 128], true), "#ff001080");
        assert_eq!(color_hex([255, 0, 16, 255], true), "#ff0010");
        assert_eq!(color_hex([255, 0, 16, 128], false), "#ff0010");
        // what the control writes parses back to the same half-transparent stop
        let c = filmcraft_color::parse_hex(&color_hex([255, 0, 0, 128], true)).unwrap_or_default();
        assert!((c[3] - 128.0 / 255.0).abs() < 1e-6 && c[0] == 1.0);
    }
}
