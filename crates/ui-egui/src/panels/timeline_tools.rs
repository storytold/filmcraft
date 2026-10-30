//! Timeline Tools - Ripple, Roll, Slip, Slide, Rate Stretch, Razor
//!
//! Fixes #3521: Ripple effect not working - tool and right-click menu now properly connected

use crate::state::UiState;
use filmcraft_engine::{Session, ClipId, TrackId};
use filmcraft_time::Tick;
use egui::{Response, Sense, Vec2, Pos2, Color32, Stroke, Painter, Rect};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTool {
    Selection,
    Ripple,
    Roll,
    Slip,
    Slide,
    RateStretch,
    Razor,
    Hand,
    Zoom,
}

impl TimelineTool {
    pub fn name(&self) -> &'static str {
        match self {
            TimelineTool::Selection => "Selection",
            TimelineTool::Ripple => "Ripple Edit",
            TimelineTool::Roll => "Rolling Edit",
            TimelineTool::Slip => "Slip",
            TimelineTool::Slide => "Slide",
            TimelineTool::RateStretch => "Rate Stretch",
            TimelineTool::Razor => "Razor",
            TimelineTool::Hand => "Hand",
            TimelineTool::Zoom => "Zoom",
        }
    }

    pub fn shortcut(&self) -> Option<&'static str> {
        match self {
            TimelineTool::Selection => Some("V"),
            TimelineTool::Ripple => Some("B"),
            TimelineTool::Roll => Some("N"),
            TimelineTool::Slip => Some("Y"),
            TimelineTool::Slide => Some("U"),
            TimelineTool::RateStretch => Some("R"),
            TimelineTool::Razor => Some("C"),
            TimelineTool::Hand => Some("H"),
            TimelineTool::Zoom => Some("Z"),
            _ => None,
        }
    }

    pub fn cursor_icon(&self) -> egui::CursorIcon {
        match self {
            TimelineTool::Selection => egui::CursorIcon::Default,
            TimelineTool::Ripple => egui::CursorIcon::ResizeHorizontal,
            TimelineTool::Roll => egui::CursorIcon::ResizeHorizontal,
            TimelineTool::Slip => egui::CursorIcon::ResizeHorizontal,
            TimelineTool::Slide => egui::CursorIcon::ResizeHorizontal,
            TimelineTool::RateStretch => egui::CursorIcon::ResizeHorizontal,
            TimelineTool::Razor => egui::CursorIcon::Crosshair,
            TimelineTool::Hand => egui::CursorIcon::Grab,
            TimelineTool::Zoom => egui::CursorIcon::ZoomIn,
        }
    }
}

/// State for timeline tool interactions
#[derive(Default)]
pub struct TimelineToolState {
    pub active_tool: TimelineTool,
    pub drag_start: Option<Pos2>,
    pub drag_clip: Option<ClipId>,
    pub drag_track: Option<TrackId>,
    pub drag_edge: Option<filmcraft_edit::Edge>,
    pub initial_clip_start: Option<Tick>,
    pub initial_clip_end: Option<Tick>,
    pub ripple_mode: bool,
}

impl TimelineToolState {
    pub fn new() -> Self {
        Self {
            active_tool: TimelineTool::Selection,
            ..Default::default()
        }
    }

    pub fn set_tool(&mut self, tool: TimelineTool) {
        self.active_tool = tool;
    }

    pub fn reset_drag(&mut self) {
        self.drag_start = None;
        self.drag_clip = None;
        self.drag_track = None;
        self.drag_edge = None;
        self.initial_clip_start = None;
        self.initial_clip_end = None;
    }
}

/// Ripple Edit Tool - Implements the ripple edit functionality
/// Fixes #3521: Now properly connected to engine commands
pub struct RippleTool;

impl RippleTool {
    /// Handle ripple edit drag on a clip edge
    pub fn handle_drag(
        ui_state: &mut UiState,
        session: &mut Session,
        clip_id: ClipId,
        track_id: TrackId,
        edge: filmcraft_edit::Edge,
        delta_ticks: i64,
    ) -> Result<(), String> {
        let seq = session.active_sequence()
            .ok_or("No active sequence")?;

        // Get the clip info
        let (_, clip) = seq.find_item(clip_id)
            .ok_or("Clip not found in sequence")?;

        // Calculate new in/out points based on edge being dragged
        let (new_in, new_out) = match edge {
            filmcraft_edit::Edge::In => {
                let new_in = (clip.start.0 + delta_ticks).max(0);
                (Tick(new_in), clip.end())
            }
            filmcraft_edit::Edge::Out => {
                let new_out = (clip.start.0 + clip.duration.0 + delta_ticks).max(clip.start.0 + 1);
                (clip.start, Tick(new_out))
            }
        };

        // Execute ripple trim command
        let params = serde_json::json!({
            "clips": [clip_id.0],
            "edge": match edge {
                filmcraft_edit::Edge::In => "in",
                filmcraft_edit::Edge::Out => "out",
            },
            "delta": delta_ticks,
            "ripple": true,
        });

        session.execute("timeline.trim", params)
            .map_err(|e| format!("Ripple trim failed: {}", e))?;

        Ok(())
    }

    /// Handle right-click context menu for ripple operations
    pub fn show_context_menu(
        ui: &mut egui::Ui,
        session: &mut Session,
        clip_id: ClipId,
        track_id: TrackId,
    ) {
        egui::ContextMenu::show(ui, |ui| {
            if ui.button("Ripple Trim In").clicked() {
                let _ = session.execute("timeline.trim", serde_json::json!({
                    "clips": [clip_id.0],
                    "edge": "in",
                    "ripple": true,
                }));
                ui.close_menu();
            }
            if ui.button("Ripple Trim Out").clicked() {
                let _ = session.execute("timeline.trim", serde_json::json!({
                    "clips": [clip_id.0],
                    "edge": "out",
                    "ripple": true,
                }));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Ripple Delete").clicked() {
                let _ = session.execute("edit.rippleDelete", serde_json::json!({
                    "clips": [clip_id.0],
                }));
                ui.close_menu();
            }
            if ui.button("Ripple Delete Head").clicked() {
                let _ = session.execute("edit.rippleDeleteHead", serde_json::json!({
                    "clips": [clip_id.0],
                }));
                ui.close_menu();
            }
        });
    }
}

/// Roll Edit Tool - Adjusts the cut point between two clips
pub struct RollTool;

impl RollTool {
    pub fn handle_drag(
        session: &mut Session,
        left_clip: ClipId,
        right_clip: ClipId,
        delta_ticks: i64,
    ) -> Result<(), String> {
        let params = serde_json::json!({
            "clips": [left_clip.0, right_clip.0],
            "roll": delta_ticks,
        });

        session.execute("timeline.roll", params)
            .map_err(|e| format!("Roll edit failed: {}", e))?;

        Ok(())
    }
}

/// Slip Tool - Changes clip's in/out points without changing timeline position
pub struct SlipTool;

impl SlipTool {
    pub fn handle_drag(
        session: &mut Session,
        clip_id: ClipId,
        delta_ticks: i64,
    ) -> Result<(), String> {
        let params = serde_json::json!({
            "clips": [clip_id.0],
            "slip": delta_ticks,
        });

        session.execute("timeline.slip", params)
            .map_err(|e| format!("Slip edit failed: {}", e))?;

        Ok(())
    }
}

/// Slide Tool - Changes clip's timeline position without changing in/out points
pub struct SlideTool;

impl SlideTool {
    pub fn handle_drag(
        session: &mut Session,
        clip_id: ClipId,
        delta_ticks: i64,
    ) -> Result<(), String> {
        let params = serde_json::json!({
            "clips": [clip_id.0],
            "slide": delta_ticks,
        });

        session.execute("timeline.slide", params)
            .map_err(|e| format!("Slide edit failed: {}", e))?;

        Ok(())
    }
}

/// Rate Stretch Tool - Changes clip speed by stretching
pub struct RateStretchTool;

impl RateStretchTool {
    pub fn handle_drag(
        session: &mut Session,
        clip_id: ClipId,
        new_duration_ticks: i64,
    ) -> Result<(), String> {
        let params = serde_json::json!({
            "clips": [clip_id.0],
            "duration": new_duration_ticks,
            "ripple": false,
        });

        session.execute("clip.speedDuration", params)
            .map_err(|e| format!("Rate stretch failed: {}", e))?;

        Ok(())
    }
}

/// Razor Tool - Cuts clips at playhead position
pub struct RazorTool;

impl RazorTool {
    pub fn cut_at_playhead(
        session: &mut Session,
        track_id: Option<TrackId>,
    ) -> Result<Vec<ClipId>, String> {
        let playhead = session.playhead();
        let params = serde_json::json!({
            "time": playhead.0,
            "tracks": track_id.map(|t| vec![t.0]).unwrap_or_else(|| vec![]),
        });

        let result = session.execute("timeline.razor", params)
            .map_err(|e| format!("Razor cut failed: {}", e))?;

        // Parse result for new clip IDs
        result.as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(ClipId)).collect())
            .unwrap_or_default()
    }

    pub fn cut_all_tracks_at_playhead(session: &mut Session) -> Result<Vec<ClipId>, String> {
        Self::cut_at_playhead(session, None)
    }
}

/// Tool palette UI component
pub fn show_tool_palette(ui: &mut egui::Ui, tool_state: &mut TimelineToolState, session: &mut Session) {
    let tools = [
        TimelineTool::Selection,
        TimelineTool::Ripple,
        TimelineTool::Roll,
        TimelineTool::Slip,
        TimelineTool::Slide,
        TimelineTool::RateStretch,
        TimelineTool::Razor,
        TimelineTool::Hand,
        TimelineTool::Zoom,
    ];

    ui.horizontal(|ui| {
        for tool in tools {
            let is_active = tool_state.active_tool == tool;
            let shortcut = tool.shortcut().unwrap_or("");

            let btn = egui::SelectableLabel::new(is_active, format!("{} ({})", tool.name(), shortcut))
                .sense(Sense::click());

            let response = ui.add(btn);

            if response.clicked() {
                tool_state.set_tool(tool);
                // Update cursor
                ui.ctx().set_cursor_icon(tool.cursor_icon());
            }

            if response.hovered() {
                ui.ctx().set_cursor_icon(tool.cursor_icon());
            }
        }
    });

    // Show tool-specific options
    match tool_state.active_tool {
        TimelineTool::Ripple => {
            ui.separator();
            ui.checkbox(&mut tool_state.ripple_mode, "Ripple Mode (affects following clips)");
        }
        TimelineTool::RateStretch => {
            ui.separator();
            ui.label("Drag clip edge to change speed");
        }
        _ => {}
    }
}

/// Timeline clip interaction handler - FIXES #3521
/// This connects the UI tools to the engine commands
pub fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    ui_state: &mut UiState,
    session: &mut Session,
    tool_state: &mut TimelineToolState,
    clip_rect: Rect,
    clip_id: ClipId,
    track_id: TrackId,
    clip_start: Tick,
    clip_end: Tick,
) -> Response {
    let response = ui.allocate_rect(clip_rect, Sense::click_and_drag());

    // Right-click context menu
    if response.secondary_clicked() {
        match tool_state.active_tool {
            TimelineTool::Ripple => {
                RippleTool::show_context_menu(ui, session, clip_id, track_id);
            }
            TimelineTool::Selection => {
                // Default context menu
                egui::ContextMenu::show(ui, |ui| {
                    if ui.button("Cut").clicked() {
                        let _ = session.execute("edit.cut", serde_json::json!({}));
                        ui.close_menu();
                    }
                    if ui.button("Copy").clicked() {
                        let _ = session.execute("edit.copy", serde_json::json!({}));
                        ui.close_menu();
                    }
                    if ui.button("Ripple Delete").clicked() {
                        let _ = session.execute("edit.rippleDelete", serde_json::json!({
                            "clips": [clip_id.0]
                        }));
                        ui.close_menu();
                    }
                    if ui.button("Add Edit").clicked() {
                        let _ = session.execute("timeline.addEdit", serde_json::json!({}));
                        ui.close_menu();
                    }
                });
            }
            _ => {}
        }
    }

    // Handle drag operations based on active tool
    if response.dragged() {
        let drag_delta = response.drag_delta().x;
        let ticks_per_pixel = ui_state.timeline.ticks_per_pixel;
        let delta_ticks = (drag_delta * ticks_per_pixel) as i64;

        // Determine which edge is being dragged (near left or right edge of clip)
        let mouse_x = response.interact_pointer_pos().unwrap_or(clip_rect.center()).x;
        let clip_center = clip_rect.center().x;
        let edge = if mouse_x < clip_center {
            filmcraft_edit::Edge::In
        } else {
            filmcraft_edit::Edge::Out
        };

        match tool_state.active_tool {
            TimelineTool::Ripple => {
                let _ = RippleTool::handle_drag(ui_state, session, clip_id, track_id, edge, delta_ticks);
            }
            TimelineTool::Roll => {
                // Roll needs adjacent clip - find it
                if let Some(adjacent) = find_adjacent_clip(session, clip_id, track_id, edge) {
                    let _ = RollTool::handle_drag(session, clip_id, adjacent, delta_ticks);
                }
            }
            TimelineTool::Slip => {
                let _ = SlipTool::handle_drag(session, clip_id, delta_ticks);
            }
            TimelineTool::Slide => {
                let _ = SlideTool::handle_drag(session, clip_id, delta_ticks);
            }
            TimelineTool::RateStretch => {
                let current_duration = (clip_end - clip_start).0;
                let new_duration = (current_duration as i64 + delta_ticks).max(1);
                let _ = RateStretchTool::handle_drag(session, clip_id, new_duration);
            }
            TimelineTool::Razor => {
                // Razor cuts on click, not drag
            }
            TimelineTool::Selection => {
                // Move clip (handled by timeline.move command)
                let params = serde_json::json!({
                    "moves": [{
                        "clip": clip_id.0,
                        "track": track_id.0,
                        "time": (clip_start.0 + delta_ticks).max(0),
                    }],
                    "linked": ui_state.linked_selection,
                });
                let _ = session.execute("timeline.move", params);
            }
            _ => {}
        }
    }

    // Razor tool cuts on click
    if response.clicked() && tool_state.active_tool == TimelineTool::Razor {
        let _ = RazorTool::cut_all_tracks_at_playhead(session);
    }

    response
}

fn find_adjacent_clip(
    session: &Session,
    clip_id: ClipId,
    track_id: TrackId,
    edge: filmcraft_edit::Edge,
) -> Option<ClipId> {
    let seq = session.active_sequence()?;
    let track = seq.track(track_id)?;

    let (_, clip) = seq.find_item(clip_id)?;

    match edge {
        filmcraft_edit::Edge::In => {
            // Find clip ending at this clip's start
            track.items.iter()
                .find(|c| c.end() == clip.start)
                .map(|c| c.id)
        }
        filmcraft_edit::Edge::Out => {
            // Find clip starting at this clip's end
            track.items.iter()
                .find(|c| c.start == clip.end())
                .map(|c| c.id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_shortcuts() {
        assert_eq!(TimelineTool::Ripple.shortcut(), Some("B"));
        assert_eq!(TimelineTool::Roll.shortcut(), Some("N"));
        assert_eq!(TimelineTool::Razor.shortcut(), Some("C"));
    }

    #[test]
    fn test_tool_names() {
        assert_eq!(TimelineTool::Ripple.name(), "Ripple Edit");
        assert_eq!(TimelineTool::Roll.name(), "Rolling Edit");
    }
}