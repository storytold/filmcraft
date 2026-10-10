//! Panel bodies. `show` dispatches on [`PanelKind`]; drag-and-drop between panels (project items,
//! effects) is carried in egui temp data so the timeline/monitors can accept drops.

pub mod audio_fx_editor;
pub mod clip_dialogs;
pub mod color_dialogs;
pub mod dialogs;
pub mod effect_controls;
pub mod effects;
pub mod essential_sound;
pub mod events;
pub mod export_mode;
pub mod file_dialogs;
pub mod frame_export;
pub mod graphics;
pub mod graphics_templates;
pub mod import_mode;
pub mod interchange_export;
pub mod keyboard;
pub mod lumetri;
pub mod masks;
pub mod media_browser;
pub mod media_dialogs;
pub mod menu_dialogs;
pub mod metadata;
pub mod meters;
pub mod misc;
pub mod mixer;
pub mod monitor;
pub mod monitor_view;
pub mod multicam;
pub mod panel_state;
pub mod presets;
pub mod project;
pub mod project_dialogs;
pub mod project_views;
pub mod reference;
pub mod remix;
pub mod scopes;
pub mod sequence_settings;
pub mod settings;
pub mod shortcuts_dialog;
pub mod text;
pub mod timecode;
pub mod timeline;
pub mod timeline_automation;
pub mod timeline_captions;
pub mod timeline_hit;
pub mod timeline_volume;
pub mod tools;
pub mod transition_controls;
pub mod trim_monitor;
pub mod tts;
pub mod voiceover;
pub mod workspaces;

use egui::{Align2, Color32, Rect};
use filmcraft_project::ItemId;

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::theme::Tokens;

/// ` · 2:05 left`: the time a job has left, as it follows a percentage.
pub fn left_text(d: std::time::Duration) -> String {
    tlf!(" · {time} left", time = filmcraft_engine::export::format_eta(d))
}

/// [`left_text`] for a job or queue item whose JSON has `etaSeconds`, nothing while it is null.
pub fn eta_suffix(job: &serde_json::Value) -> String {
    job["etaSeconds"].as_f64().and_then(|s| std::time::Duration::try_from_secs_f64(s).ok()).map(left_text).unwrap_or_default()
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, p: PanelKind, rect: Rect) {
    match p {
        PanelKind::Program if !app.session.state.edit_points.is_empty() => trim_monitor::show(app, ui, rect),
        PanelKind::Program => monitor::show(app, ui, rect, monitor::Which::Program),
        PanelKind::Source => monitor::show(app, ui, rect, monitor::Which::Source),
        PanelKind::Timeline => timeline::show(app, ui, rect),
        PanelKind::Project => project::show(app, ui, rect),
        PanelKind::Tools => tools::show(app, ui, rect),
        PanelKind::Effects => effects::show(app, ui, rect),
        PanelKind::EffectControls => effect_controls::show(app, ui, rect),
        PanelKind::AudioMeters => meters::show(app, ui, rect),
        PanelKind::LumetriColor => lumetri::show(app, ui, rect),
        PanelKind::Properties if graphics::graphic_selected(app) => graphics::properties(app, ui, rect),
        PanelKind::Properties => effect_controls::properties_panel(app, ui, rect),
        PanelKind::EssentialGraphics => graphics_templates::essential_graphics(app, ui, rect),
        PanelKind::History => misc::history(app, ui, rect),
        PanelKind::Markers => misc::markers(app, ui, rect),
        PanelKind::Info => misc::info(app, ui, rect),
        PanelKind::MediaBrowser => media_browser::show(app, ui, rect),
        PanelKind::AudioTrackMixer => mixer::track_mixer(app, ui, rect),
        PanelKind::AudioClipMixer => mixer::clip_mixer(app, ui, rect),
        PanelKind::LumetriScopes => scopes::show(app, ui, rect),
        PanelKind::Metadata => metadata::show(app, ui, rect),
        PanelKind::Timecode => timecode::show(app, ui, rect),
        PanelKind::Events => events::events(app, ui, rect),
        PanelKind::Progress => events::progress(app, ui, rect),
        PanelKind::ReferenceMonitor => reference::show(app, ui, rect),
        PanelKind::Text => text::show(app, ui, rect),
        PanelKind::EssentialSound => essential_sound::show(app, ui, rect),
        PanelKind::TextToSpeech => tts::show(app, ui, rect),
        other => crate::dock::placeholder(ui, rect, &app.tokens, &tlf!("{panel} — coming in a later milestone", panel = crate::i18n::t(other.title()))),
    }
}

pub mod source_drag;
pub mod source_range;

#[derive(Clone, Debug)]
enum DragPayload {
    Item(ItemId),
    /// A bin (by id) dragged in the Project panel.
    Bin(u64),
    Source(source_drag::SourceDrag),
    Effect(String),
    /// A graphics template (id, name) from Essential Graphics ▸ Browse.
    Template(String, String),
}

fn payload_id() -> egui::Id {
    egui::Id::new("filmcraft-drag-payload")
}

pub fn start_drag_source(ui: &egui::Ui, source: source_drag::SourceDrag) {
    ui.ctx().data_mut(|d| d.insert_temp(payload_id(), Some(DragPayloadBox(DragPayload::Source(source)))));
}
pub fn dragged_source(ui: &egui::Ui) -> Option<source_drag::SourceDrag> {
    match payload(ui) {
        Some(DragPayload::Source(s)) => Some(s),
        _ => None,
    }
}
pub fn start_drag_item(ui: &egui::Ui, item: ItemId) {
    ui.ctx().data_mut(|d| d.insert_temp(payload_id(), Some(DragPayloadBox(DragPayload::Item(item)))));
}
pub fn start_drag_bin(ui: &egui::Ui, bin: u64) {
    ui.ctx().data_mut(|d| d.insert_temp(payload_id(), Some(DragPayloadBox(DragPayload::Bin(bin)))));
}
pub fn dragged_bin(ui: &egui::Ui) -> Option<u64> {
    match payload(ui) {
        Some(DragPayload::Bin(b)) => Some(b),
        _ => None,
    }
}
pub fn start_drag_template(ui: &egui::Ui, id: &str, name: &str) {
    ui.ctx().data_mut(|d| d.insert_temp(payload_id(), Some(DragPayloadBox(DragPayload::Template(id.to_string(), name.to_string())))));
}
pub fn dragged_template(ui: &egui::Ui) -> Option<String> {
    match payload(ui) {
        Some(DragPayload::Template(id, _)) => Some(id),
        _ => None,
    }
}
pub fn start_drag_effect(ui: &egui::Ui, id: &str) {
    ui.ctx().data_mut(|d| d.insert_temp(payload_id(), Some(DragPayloadBox(DragPayload::Effect(id.to_string())))));
}
#[derive(Clone, Debug)]
struct DragPayloadBox(DragPayload);

fn payload(ui: &egui::Ui) -> Option<DragPayload> {
    ui.ctx().data(|d| d.get_temp::<Option<DragPayloadBox>>(payload_id())).flatten().map(|b| b.0)
}
pub fn dragged_project_item(ui: &egui::Ui) -> Option<ItemId> {
    match payload(ui) {
        Some(DragPayload::Item(i)) => Some(i),
        Some(DragPayload::Source(s)) => Some(s.item),
        _ => None,
    }
}
pub fn dragged_effect(ui: &egui::Ui) -> Option<String> {
    match payload(ui) {
        Some(DragPayload::Effect(e)) => Some(e),
        _ => None,
    }
}
pub fn clear_drag(ui: &egui::Ui) {
    ui.ctx().data_mut(|d| d.insert_temp::<Option<DragPayloadBox>>(payload_id(), None));
}

/// Draw the drag ghost near the pointer and clear the payload after release.
pub fn drag_ghost(app: &FilmcraftApp, ui: &egui::Ui) {
    let Some(pl) = payload(ui) else { return };
    let ctx = ui.ctx();
    if let Some(p) = ctx.pointer_hover_pos() {
        let label = match &pl {
            DragPayload::Item(i) => app.session.project.item(*i).map(|x| x.name.clone()).unwrap_or_default(),
            DragPayload::Bin(b) => app.session.project.root.find_bin(filmcraft_project::BinId(*b)).map(|x| x.name.clone()).unwrap_or_default(),
            DragPayload::Source(s) => {
                let name = app.session.project.item(s.item).map(|i| i.name.as_str()).unwrap_or("Source");
                let mode = match (s.video, s.audio) {
                    (true, true) => "video + audio",
                    (true, false) => "video",
                    _ => "audio",
                };
                format!("{name} · {mode} · {:.2} s", s.range.duration.seconds())
            }
            DragPayload::Template(_, name) => name.clone(),
            DragPayload::Effect(e) => match e.strip_prefix("preset:") {
                Some(name) => name.to_string(),
                None => filmcraft_project::find_effect(e).map(|d| d.name.to_string()).unwrap_or_default(),
            },
        };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag-ghost")));
        let r = Rect::from_min_size(p + egui::vec2(12.0, 8.0), egui::vec2(label.len() as f32 * 7.0 + 16.0, 20.0));
        painter.rect_filled(r, 4.0, Color32::from_black_alpha(200));
        painter.text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.5), Color32::WHITE);
    }
    if ctx.input(|i| i.pointer.any_released()) {
        // cleared next frame so drop targets see the release this frame
        let f = ctx.cumulative_frame_nr();
        let k = egui::Id::new("drag-release-frame");
        match ctx.data(|d| d.get_temp::<u64>(k)) {
            Some(prev) if prev < f => {
                clear_drag(ui);
                ctx.data_mut(|d| d.remove::<u64>(k));
            }
            None => {
                ctx.data_mut(|d| d.insert_temp(k, f));
            }
            Some(_) => {}
        }
    } else if !ctx.input(|i| i.pointer.any_down()) {
        clear_drag(ui);
    }
}

/// The panel "≡" menu.
pub fn panel_menu_popup(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    drag_ghost(app, ui);
    // bins opened in new windows (Project panel)
    project::floating(app, ui.ctx());
    let id = egui::Id::new("panel-menu");
    let Some((p, pos)) = ui.ctx().data(|d| d.get_temp::<(PanelKind, egui::Pos2)>(id)) else { return };
    let mut close = false;
    let area = egui::Area::new(id.with("area")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(190.0);
            // the Timeline's tabs are its open sequences: Close Panel closes the active one and
            // keeps the panel (Premiere's wording and behaviour)
            if p == PanelKind::Timeline && app.session.state.active_sequence.is_some() {
                let r = ui.button(tl!("Close Panel"));
                app.auto.add("panel.menu.Timeline.close", r.rect, "Close Panel");
                if r.clicked() {
                    let _ = app.session.execute("sequence.close", serde_json::json!({}));
                    close = true;
                }
                let r = ui.add_enabled(app.session.state.open_sequences.len() > 1, egui::Button::new(tl!("Close Other Timeline Panels")));
                app.auto.add("panel.menu.Timeline.closeOthers", r.rect, "Close Other Timeline Panels");
                if r.clicked() {
                    let _ = app.session.execute("sequence.closeOthers", serde_json::json!({}));
                    close = true;
                }
            } else if ui.button(tl!("Close Panel")).clicked() {
                app.ui.dock.close(p);
                close = true;
            }
            if ui.button(tl!("Maximize Frame")).clicked() {
                app.ui.dock = crate::dock::DockNode::Tabs { panels: vec![p], active: 0 };
                close = true;
            }
            if ui.button(tl!("Restore Workspace")).clicked() {
                let w = app.ui.workspace.clone();
                app.set_workspace(&w);
                close = true;
            }
            if p == PanelKind::Timeline {
                ui.separator();
                let r = ui.add_enabled(app.session.state.active_sequence.is_some(), egui::Button::new(tl!("Reveal Sequence in Project")));
                app.auto.add("panel.menu.Timeline.revealSequence", r.rect, "Reveal Sequence in Project");
                if r.clicked() {
                    let _ = app.session.execute("sequence.revealInProject", serde_json::json!({}));
                    close = true;
                }
                ui.checkbox(&mut app.ui.timeline.show_thumbnails, tl!("Video Thumbnails"));
                ui.checkbox(&mut app.ui.timeline.show_waveforms, tl!("Audio Waveforms"));
            }
            if p == PanelKind::Project {
                ui.separator();
                close |= project::panel_menu(app, ui);
            }
            if p == PanelKind::MediaBrowser {
                ui.separator();
                close |= media_browser::panel_menu(app, ui);
            }
        });
    });
    // A click elsewhere or Escape closes the menu. The click that opened it is over the tab, not
    // the menu, and must not close it again in the same frame.
    let fresh = ui.ctx().data(|d| d.get_temp::<u64>(egui::Id::new("panel-menu-opened"))) == Some(ui.ctx().cumulative_frame_nr());
    if close || (!fresh && area.response.clicked_elsewhere()) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().data_mut(|d| d.remove::<(PanelKind, egui::Pos2)>(id));
    }
}
