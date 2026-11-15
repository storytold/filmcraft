//! Marker editing and project palette creation are separate, undoable operations.
use crate::{
    FilmcraftApp,
    widgets::color_picker::{self, ColorDraft},
};
use egui::{Color32, Sense, Stroke, vec2};
use filmcraft_project::{HexColor, Label, Marker, MarkerKind};
use serde_json::json;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Draft {
    pub project: Arc<()>,
    pub owner: Owner,
    pub time: String,
    pub duration: String,
    pub range: bool,
    pub original_duration: filmcraft_time::Tick,
    pub initial_time: String,
    pub initial_duration: String,
    pub marker: u64,
    pub original: Marker,
    pub kind: MarkerKind,
    pub url: String,
    pub name: String,
    pub comment: String,
    pub author: String,
    pub hex: String,
    pub custom: bool,
    pub preset: Label,
    pub color_editor: Option<ColorDraft>,
    pub error: String,
    focus_name: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Owner {
    Sequence(u64),
    Source(u64),
}
impl Owner {
    pub fn params(self) -> serde_json::Value {
        match self {
            Self::Sequence(id) => json!({"target":"program","sequence":id}),
            Self::Source(id) => json!({"target":"source","item":id}),
        }
    }
    pub fn current(app: &FilmcraftApp, source: bool) -> Option<Self> {
        if source { app.session.state.source_item.map(|id| Self::Source(id.0)) } else { app.session.state.active_sequence.map(|id| Self::Sequence(id.0)) }
    }
    pub fn details(self, app: &FilmcraftApp) -> Option<(String, filmcraft_time::FrameRate, bool, Vec<Marker>)> {
        let id = filmcraft_project::ItemId(match self {
            Self::Sequence(id) | Self::Source(id) => id,
        });
        let item = app.session.project.item(id)?;
        match self {
            Self::Sequence(_) => {
                let q = app.session.project.sequence(id)?;
                Some((tlf!("Sequence: {name}", name = &item.name), q.settings.frame_rate, q.settings.drop_frame, q.markers.clone()))
            }
            Self::Source(_) => {
                let v = filmcraft_engine::clip_ops::source_view(&app.session, id)?;
                Some((
                    if v.media != id { tlf!("Source: {name} (shared source markers)", name = &item.name) } else { tlf!("Source: {name}", name = &item.name) },
                    v.rate,
                    app.session.project.sequence(v.media).is_some_and(|q| q.settings.drop_frame),
                    v.markers,
                ))
            }
        }
    }
}
pub fn open(app: &mut FilmcraftApp, marker: &Marker) {
    if let Some(owner) = Owner::current(app, false) {
        open_owned(app, marker, owner);
    }
}
pub fn open_owned(app: &mut FilmcraftApp, marker: &Marker, owner: Owner) {
    let Some((_, rate, drop, _)) = owner.details(app) else { return };
    let format = |ticks| filmcraft_time::format_time(ticks, rate, drop, filmcraft_time::TimeDisplay::Timecode, 48000).replace(';', ":");
    let time = format(marker.start);
    let duration = format(marker.duration);
    app.ui.marker_properties = Some(Draft {
        project: app.session.project_instance.clone(),
        owner,
        initial_time: time.clone(),
        initial_duration: duration.clone(),
        time,
        duration,
        range: marker.duration > filmcraft_time::Tick::ZERO,
        original_duration: marker.duration,
        marker: marker.id.0,
        original: marker.clone(),
        kind: marker.kind,
        url: marker.url.clone(),
        name: marker.name.clone(),
        comment: marker.comment.clone(),
        author: marker.author.clone(),
        hex: HexColor(marker.custom_color.map_or_else(|| app.session.prefs.labels.rgb(marker.color), |c| c.0)).hex(),
        custom: marker.custom_color.is_some(),
        preset: marker.color,
        color_editor: None,
        error: String::new(),
        focus_name: true,
    });
}

enum Action {
    Done,
    Cancel,
    Delete,
    Navigate(Marker),
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.marker_properties.take() else { return };
    if !Arc::ptr_eq(&draft.project, &app.session.project_instance) {
        return;
    }
    let Some((title, rate, drop, mut markers)) = draft.owner.details(app) else {
        app.ui.status = tl!("Marker owner is unavailable").into();
        return;
    };
    // Commands can edit or remove this marker while its dialog (or native picker) is open.
    // Unrelated edits do not invalidate the draft. Ties use IDs so coincident markers remain reachable.
    markers.sort_by_key(|m| (m.start, m.id.0));
    let index = markers.iter().position(|m| m.id.0 == draft.marker);
    if index.and_then(|i| markers.get(i)) != Some(&draft.original) {
        app.ui.status = tl!("Marker changed or was removed. Reopen it to continue.").into();
        return;
    }
    if draft.color_editor.is_some() {
        color_dialog(app, ctx, &mut draft);
        app.ui.marker_properties = Some(draft);
        return;
    }
    let previous = index.and_then(|i| i.checked_sub(1)).and_then(|i| markers.get(i));
    let next = index.and_then(|i| i.checked_add(1)).and_then(|i| markers.get(i));
    let mut action = None;
    let modal = egui::Modal::new(egui::Id::unique("marker-properties")).show(ctx, |ui| {
        let width = (ctx.content_rect().width() - 40.0).clamp(240.0, 560.0);
        let max_height = (ctx.content_rect().height() - 110.0).max(100.0);
        ui.set_width(width);
        ui.heading(format!("{} @ {}", tl!("Edit Marker"), draft.time));
        ui.label(&title);
        ui.add_space(6.0);
        if width >= 560.0 {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                ui.vertical(|ui| {
                    ui.set_width(width - 112.0);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    field_area(app, ui, &mut draft, width - 112.0, max_height, rate);
                });
                ui.vertical(|ui| {
                    ui.set_width(96.0);
                    action = actions(app, ui, previous, next, true, valid_times(&draft, rate, drop));
                });
            });
        } else {
            field_area(app, ui, &mut draft, width, (max_height - if width < 400.0 { 150.0 } else { 85.0 }).max(80.0), rate);
            ui.separator();
            action = actions(app, ui, previous, next, false, valid_times(&draft, rate, drop));
        }
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
    });
    if matches!(action, Some(Action::Cancel)) || modal.should_close() {
        return;
    }
    match action {
        Some(Action::Delete) => {
            let mut p = draft.owner.params();
            p["marker"] = json!(draft.marker);
            match app.session.execute("markers.delete", p) {
                Ok(_) => return,
                Err(e) => draft.error = e.to_string(),
            }
        }
        Some(Action::Done | Action::Navigate(_)) => match save(app, &draft) {
            Ok(()) => {
                if let Some(Action::Navigate(marker)) = action {
                    open_owned(app, &marker, draft.owner);
                }
                return;
            }
            Err(e) => draft.error = e,
        },
        _ => {}
    }
    app.ui.marker_properties = Some(draft);
}

fn field_area(app: &mut FilmcraftApp, ui: &mut egui::Ui, draft: &mut Draft, width: f32, height: f32, rate: filmcraft_time::FrameRate) {
    ui.allocate_ui_with_layout(vec2(width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
        egui::ScrollArea::vertical().id_salt("marker-fields").max_height(height).show(ui, |ui| {
            ui.set_width(width - 12.0);
            fields(app, ui, draft, rate);
        });
    });
}

fn action_button(ui: &mut egui::Ui, label: &str, width: f32, enabled: bool, primary: bool) -> egui::Response {
    ui.add_enabled(enabled, |ui: &mut egui::Ui| {
        let text = if primary { egui::RichText::new(label).color(ui.visuals().selection.stroke.color) } else { egui::RichText::new(label) };
        let mut button = egui::Button::new(text).corner_radius(14.0);
        if primary {
            button = button.fill(ui.visuals().selection.bg_fill);
        }
        ui.add_sized(vec2(width, 28.0), button)
    })
}

fn actions(app: &mut FilmcraftApp, ui: &mut egui::Ui, previous: Option<&Marker>, next: Option<&Marker>, vertical: bool, can_save: bool) -> Option<Action> {
    let mut action = None;
    let mut buttons = |ui: &mut egui::Ui| {
        let width = if vertical { 80.0 } else { 96.0 };
        for (id, label, value) in [("apply", tl!("Done"), Action::Done), ("cancel", tl!("Cancel"), Action::Cancel)] {
            let response =
                action_button(ui, label, width, id != "apply" || can_save, id == "apply").on_disabled_hover_text(tl!("Enter a valid HH:MM:SS:FF timecode"));
            app.auto.add_response(&format!("markerProperties.{id}"), &response, label);
            if response.clicked() {
                action = Some(value);
            }
            if vertical {
                ui.add_space(if id == "apply" { 4.0 } else { 16.0 });
            }
        }
        for (id, label, marker) in [("previous", tl!("Previous"), previous), ("next", tl!("Next"), next)] {
            let response = action_button(ui, label, width, marker.is_some(), false);
            app.auto.add_response(&format!("markerProperties.{id}"), &response, label);
            if response.clicked()
                && let Some(marker) = marker
            {
                action = Some(Action::Navigate(marker.clone()));
            }
            if vertical {
                ui.add_space(4.0);
            }
        }
        let response = action_button(ui, tl!("Delete"), width, true, false);
        app.auto.add_response("markerProperties.delete", &response, tl!("Delete"));
        if response.clicked() {
            action = Some(Action::Delete);
        }
    };
    if vertical {
        ui.with_layout(egui::Layout::top_down(egui::Align::Center), &mut buttons);
    } else {
        ui.horizontal_wrapped(&mut buttons);
    }
    action
}

/// Keep typing, paste and text Undo on the same bounded numeric/colon buffer.
struct TimecodeBuffer<'a>(&'a mut String, usize);
impl egui::TextBuffer for TimecodeBuffer<'_> {
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<TimecodeBuffer<'static>>()
    }
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.0
    }
    fn insert_text(&mut self, text: &str, at: egui::epaint::text::CharIndex) -> usize {
        let room = self.1.saturating_sub(self.0.chars().count());
        let filtered: String = text.chars().filter(|c| c.is_ascii_digit() || *c == ':').take(room).collect();
        egui::TextBuffer::insert_text(self.0, &filtered, at)
    }
    fn delete_char_range(&mut self, range: std::ops::Range<egui::epaint::text::CharIndex>) {
        egui::TextBuffer::delete_char_range(self.0, range);
    }
}

fn timecode_field(ui: &mut egui::Ui, value: &mut String, rate: filmcraft_time::FrameRate, label: &str) -> egui::Response {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let sample = if rate.timecode_base() >= 100 { "00:00:00:000" } else { "00:00:00:00" };
    // egui 0.37 includes frame padding in desired_width; reserve it and the caret explicitly.
    let margin = egui::Margin::symmetric(4, 3);
    let width = ui.painter().layout_no_wrap(sample.into(), font.clone(), ui.visuals().text_color()).size().x + margin.sum().x + 2.0;
    crate::widgets::accessible_name(
        ui.add(egui::TextEdit::singleline(&mut TimecodeBuffer(value, sample.len())).font(font).margin(margin).desired_width(width)),
        label,
    )
}

fn parse_marker_timecode(value: &str, rate: filmcraft_time::FrameRate, drop: bool) -> Result<i64, String> {
    let error = || tl!("Enter a valid HH:MM:SS:FF timecode").to_string();
    let length = if rate.timecode_base() >= 100 { 12 } else { 11 };
    if value.len() != length || !value.bytes().enumerate().all(|(i, b)| if matches!(i, 2 | 5 | 8) { b == b':' } else { b.is_ascii_digit() }) {
        return Err(error());
    }
    let frame = filmcraft_time::parse_timecode(value, rate, drop, 0).map_err(|_| error())?;
    // Reject overflowed fields and skipped drop-frame labels rather than normalizing them.
    let formatted = filmcraft_time::format_timecode_frames(frame, rate, drop);
    if !formatted.split([':', ';']).map(|s| s.parse::<u32>().ok()).eq(value.split(':').map(|s| s.parse::<u32>().ok())) {
        return Err(error());
    }
    Ok(frame)
}

fn valid_times(draft: &Draft, rate: filmcraft_time::FrameRate, drop: bool) -> bool {
    // Imported timings can exceed the editable timecode width or contain subframes.
    // Metadata-only edits retain those exact original ticks; edited fields stay strict.
    (draft.time == draft.initial_time || parse_marker_timecode(&draft.time, rate, drop).is_ok())
        && (!draft.range
            || (draft.duration == draft.initial_duration && draft.original_duration > filmcraft_time::Tick::ZERO)
            || parse_marker_timecode(&draft.duration, rate, drop).is_ok_and(|f| f > 0))
}

fn field_row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    field_row_height(ui, label, 0.0, contents);
}

fn field_row_height(ui: &mut egui::Ui, label: &str, min_height: f32, contents: impl FnOnce(&mut egui::Ui)) {
    // Match a TextEdit's first line and its two-pixel top/bottom margin, including Notes.
    let label_height = (ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().extra_text_line_spacing + 4.0).max(min_height);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(76.0, label_height), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(label);
        });
        ui.vertical(contents);
    });
}

fn fields(app: &mut FilmcraftApp, ui: &mut egui::Ui, draft: &mut Draft, rate: filmcraft_time::FrameRate) {
    field_row(ui, tl!("Name"), |ui| {
        let response = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(f32::INFINITY)), tl!("Name"));
        app.auto.add_response("markerProperties.name", &response, tl!("Name"));
        if draft.focus_name {
            response.request_focus();
            draft.focus_name = false;
        }
    });
    ui.add_space(6.0);
    // Useful for multi-user projects: attribution belongs to the marker, not the current OS user.
    field_row(ui, tl!("Author"), |ui| {
        let response = crate::widgets::accessible_name(ui.add(egui::TextEdit::singleline(&mut draft.author).desired_width(f32::INFINITY)), tl!("Author"));
        app.auto.add_response("markerProperties.author", &response, tl!("Author"));
    });
    ui.add_space(10.0);
    field_row_height(ui, tl!("Time"), ui.spacing().interact_size.y, |ui| {
        ui.horizontal_wrapped(|ui| {
            let response = timecode_field(ui, &mut draft.time, rate, tl!("Time"));
            app.auto.add_response("markerProperties.time", &response, tl!("Time"));
            ui.add_space(16.0);
            let response = ui.checkbox(&mut draft.range, tl!("Duration")).on_hover_text(tl!("Range marker"));
            app.auto.add_response("markerProperties.range", &response, tl!("Range marker"));
            if response.changed() && draft.range && draft.original_duration == filmcraft_time::Tick::ZERO && draft.duration == draft.initial_duration {
                draft.duration = filmcraft_time::format_timecode_frames(1, rate, false);
            }
            let response = ui.add_enabled(draft.range, |ui: &mut egui::Ui| timecode_field(ui, &mut draft.duration, rate, tl!("Duration")));
            app.auto.add_response("markerProperties.duration", &response, tl!("Duration"));
        });
    });
    ui.add_space(10.0);
    field_row(ui, tl!("Notes"), |ui| {
        egui::ScrollArea::vertical().id_salt("marker-notes").max_height(120.0).auto_shrink([false, false]).show(ui, |ui| {
            let mut response = crate::widgets::accessible_name(
                ui.add(egui::TextEdit::multiline(&mut draft.comment).desired_width(f32::INFINITY).desired_rows(8)),
                tl!("Notes"),
            );
            // Register the visible editor area so automation can still focus a long scrolled note.
            response.rect = response.rect.intersect(ui.clip_rect());
            app.auto.add_response("markerProperties.comment", &response, tl!("Notes"));
        });
    });
    ui.add_space(14.0);
    ui.group(|ui| {
        ui.set_width(ui.available_width().max(100.0));
        ui.label(tl!("Options"));
        ui.label(tl!("Color"));
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for preset in Label::ALL {
                    let response =
                        swatch(ui, app.session.prefs.labels.rgb(preset), !draft.custom && draft.preset == preset).on_hover_text(crate::i18n::t(preset.name()));
                    app.auto.add_response(&format!("markerProperties.preset.{}", preset.name()), &response, crate::i18n::t(preset.name()));
                    if response.clicked() {
                        draft.preset = preset;
                        draft.custom = false;
                        draft.hex = HexColor(app.session.prefs.labels.rgb(preset)).hex();
                    }
                }
            });
            let mut colors = super::custom_colors::available(app);
            if draft.custom
                && let Some(color) = HexColor::parse(&draft.hex)
                && !colors.contains(&color)
            {
                colors.push(color);
            }
            egui::ScrollArea::vertical().id_salt("marker-custom-colors").max_height(100.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for color in colors {
                        let hex = color.hex();
                        let response =
                            swatch(ui, color.0, draft.custom && draft.hex.eq_ignore_ascii_case(&hex)).on_hover_text(super::custom_colors::name(app, color));
                        app.auto.add_response(&format!("markerProperties.custom.{hex}"), &response, &hex);
                        if response.clicked() {
                            draft.hex = hex;
                            draft.custom = true;
                        }
                    }
                    let response = add_color_swatch(ui).on_hover_text(tl!("Add custom color"));
                    app.auto.add_response("markerProperties.addColor", &response, tl!("Add custom color"));
                    if response.clicked() {
                        draft.error.clear();
                        draft.color_editor = Some(ColorDraft::new(HexColor::parse(&draft.hex).map_or(app.session.prefs.labels.rgb(draft.preset), |c| c.0)));
                    }
                });
            });
        });
        ui.add_space(10.0);
        let group = ui.vertical(|ui| {
            for kind in [MarkerKind::Comment, MarkerKind::Chapter, MarkerKind::Segmentation, MarkerKind::WebLink] {
                ui.horizontal_wrapped(|ui| {
                    let label = kind_label(kind);
                    let response = ui.radio_value(&mut draft.kind, kind, label);
                    app.auto.add_response(&format!("markerProperties.kind.{}", kind_id(kind)), &response, label);
                    if kind == MarkerKind::WebLink && draft.kind == MarkerKind::WebLink {
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut draft.url)
                                .desired_width((ui.available_size_before_wrap().x - ui.spacing().item_spacing.x - 24.0).max(60.0))
                                .char_limit(8192)
                                .hint_text("https://"),
                        );
                        app.auto.add_response("markerProperties.url", &response, tl!("URL"));
                        let address = filmcraft_project::marker_web_url(&draft.url);
                        let response = ui.add_enabled(address.is_some(), |ui: &mut egui::Ui| {
                            let response = ui.add_sized(vec2(24.0, 24.0), egui::Button::new(""));
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Open Link")));
                            let color = ui.style().interact(&response).fg_stroke.color;
                            crate::icons::paint(ui.painter(), response.rect.shrink(4.0), crate::icons::Icon::ExternalLink, color);
                            response
                        });
                        let response = response.on_hover_text(tl!("Opens this address in your web browser"));
                        app.auto.add_response("markerProperties.openLink", &response, tl!("Open Link"));
                        if response.clicked()
                            && let Some(address) = address
                        {
                            crate::links::open(app, ui.ctx(), &address);
                            if let Some(failure) = app.ui.failed_link.as_ref().filter(|failure| failure.url == address) {
                                draft.error.clone_from(&failure.error);
                            }
                        }
                    }
                });
                if kind == MarkerKind::WebLink
                    && draft.kind == MarkerKind::WebLink
                    && !draft.url.trim().is_empty()
                    && filmcraft_project::marker_web_url(&draft.url).is_none()
                {
                    ui.colored_label(ui.visuals().error_fg_color, tl!("Enter a valid HTTP or HTTPS URL without credentials"));
                }
            }
        });
        app.auto.add_response("markerProperties.kind", &group.response, tl!("Type"));
    });
}

/// Navigation commits only this marker; unchanged navigation does not dirty the project or Undo.
fn save(app: &mut FilmcraftApp, draft: &Draft) -> Result<(), String> {
    let params = edit_params(app, draft)?;
    let original = &draft.original;
    let changed = draft.name != original.name
        || draft.author != original.author
        || draft.comment != original.comment
        || draft.kind != original.kind
        || draft.url != original.url
        || draft.time != draft.initial_time
        || draft.range != (original.duration > filmcraft_time::Tick::ZERO)
        || (draft.range && draft.duration != draft.initial_duration)
        || draft.custom != original.custom_color.is_some()
        || if draft.custom { HexColor::parse(&draft.hex) != original.custom_color } else { draft.preset != original.color };
    if changed {
        app.session.execute("markers.edit", params).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn edit_params(app: &FilmcraftApp, draft: &Draft) -> Result<serde_json::Value, String> {
    let Some((_, rate, drop, _)) = draft.owner.details(app) else {
        return Err(tl!("Marker owner is unavailable").into());
    };
    let parse = |value: &str| -> Result<i64, String> {
        let f = parse_marker_timecode(value, rate, drop)?;
        if f < 0 || f > rate.frame_at(filmcraft_time::Tick::MAX) {
            return Err(tl!("Time is outside the supported range").into());
        }
        Ok(f)
    };
    let mut p = draft.owner.params();
    p["marker"] = json!(draft.marker);
    p["name"] = json!(draft.name);
    p["comment"] = json!(draft.comment);
    p["author"] = json!(draft.author);
    p["kind"] = json!(kind_id(draft.kind));
    if draft.url != draft.original.url {
        p["url"] = json!(draft.url);
    }
    p["color"] = json!(if draft.custom { draft.hex.clone() } else { draft.preset.name().to_string() });
    // Do not quantize imported subframe positions when only editing text/color.
    if draft.time != draft.initial_time {
        p["frame"] = json!(parse(&draft.time)?);
    }
    if !draft.range && draft.original_duration != filmcraft_time::Tick::ZERO {
        p["durationFrames"] = json!(0);
    } else if draft.range && (draft.duration != draft.initial_duration || draft.original_duration == filmcraft_time::Tick::ZERO) {
        let duration = parse(&draft.duration)?;
        if duration == 0 {
            return Err(tl!("A range marker needs a positive duration").into());
        }
        p["durationFrames"] = json!(duration);
    }
    Ok(p)
}

fn kind_id(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Comment => "comment",
        MarkerKind::Chapter => "chapter",
        MarkerKind::Segmentation => "segmentation",
        MarkerKind::WebLink => "webLink",
        MarkerKind::FlashCue => "flashCue",
    }
}

fn kind_label(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Comment => tl!("Comment Marker"),
        MarkerKind::Chapter => tl!("Chapter Marker"),
        MarkerKind::Segmentation => tl!("Segmentation Marker"),
        MarkerKind::WebLink => tl!("Web Link"),
        MarkerKind::FlashCue => tl!("Flash Cue Point"),
    }
}

fn color_dialog(app: &mut FilmcraftApp, ctx: &egui::Context, draft: &mut Draft) {
    let Some(editor) = &mut draft.color_editor else { return };
    let mut add = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::unique("marker-custom-color")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.heading(tl!("Add Custom Color"));
        color_picker::show(ui, &mut app.auto, &mut app.hooks, "markerColor", editor);
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
        ui.separator();
        ui.horizontal(|ui| {
            let response = ui.add_enabled(editor.value().is_some(), egui::Button::new(tl!("Add color")));
            app.auto.add_response("markerColor.add", &response, tl!("Add color"));
            add = response.clicked();
            let response = ui.button(tl!("Cancel"));
            app.auto.add_response("markerColor.cancel", &response, tl!("Cancel"));
            cancel = response.clicked();
        });
    });
    if cancel || modal.should_close() {
        draft.color_editor = None;
        draft.error.clear();
    } else if add && let Some(color) = editor.value() {
        match app.session.execute("colors.addCustom", json!({"color":color})) {
            Ok(_) => {
                draft.hex = color.hex();
                draft.custom = true;
                draft.color_editor = None;
                draft.error.clear();
            }
            Err(e) => draft.error = e.to_string(),
        }
    }
}

fn add_color_swatch(ui: &mut egui::Ui) -> egui::Response {
    let (chip, response) = allocate_swatch(ui);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Add custom color")));
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(chip, 3.0, visuals.bg_fill);
    ui.painter().rect_stroke(chip, 3.0, Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.5)), egui::StrokeKind::Outside);
    let center = chip.center();
    let stroke = Stroke::new(1.25, visuals.text_color());
    ui.painter().line_segment([center - vec2(4.0, 0.0), center + vec2(4.0, 0.0)], stroke);
    ui.painter().line_segment([center - vec2(0.0, 4.0), center + vec2(0.0, 4.0)], stroke);
    response
}

pub(super) fn swatch(ui: &mut egui::Ui, rgb: [u8; 3], selected: bool) -> egui::Response {
    let (chip, response) = allocate_swatch(ui);
    let [r, g, b] = rgb;
    ui.painter().rect_filled(chip, 3.0, Color32::from_rgb(r, g, b));
    ui.painter().rect_stroke(chip, 3.0, Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.5)), egui::StrokeKind::Outside);
    if selected {
        ui.painter().rect_stroke(chip.expand(3.0), 5.0, Stroke::new(2.0, ui.visuals().text_color()), egui::StrokeKind::Outside);
    }
    response
}

fn allocate_swatch(ui: &mut egui::Ui) -> (egui::Rect, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    (rect.shrink(3.0), response)
}

#[cfg(test)]
mod timecode_tests {
    use super::parse_marker_timecode;
    use filmcraft_time::FrameRate;

    #[test]
    fn marker_timecode_requires_complete_fields_and_valid_drop_frame_labels() {
        let rate = FrameRate::FPS_29_97;
        assert_eq!(parse_marker_timecode("00:01:00:02", rate, true), Ok(1800));
        assert!(parse_marker_timecode("00:01:00:00", rate, true).is_err());
        assert_eq!(parse_marker_timecode("00:10:00:00", rate, true), Ok(17982));
        // High frame rates need three frame digits; their existing values remain editable.
        assert_eq!(parse_marker_timecode("00:00:00:119", FrameRate::FPS_120, false), Ok(119));
        assert!(parse_marker_timecode("00:00:00:120", FrameRate::FPS_120, false).is_err());
        for value in ["00:00:0:01", "00;00;00;01", "00:60:00:00", "00:00:60:00", "00:00:00:30", "00:00:00:０1"] {
            assert!(parse_marker_timecode(value, rate, true).is_err(), "{value}");
        }
    }
}
