//! Improved Docking System - Persistent layouts, keyboard navigation, focus management
//!
//! New Feature: Enhanced docking with layout persistence, keyboard accessibility, and better UX

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use egui::{Id, Vec2, Rect, Sense, Response, Color32, Stroke, Painter, Align2, FontId};
use egui::epaint::Shadow;
use serde::{Deserialize, Serialize};
use crate::dock::{PanelKind, LayoutExt, DockArea, DockState, NodeIndex, Style as DockStyle};
use crate::state::UiState;

/// Enhanced docking configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockConfig {
    /// Enable layout persistence per project
    pub persist_per_project: bool,
    
    /// Enable layout persistence per workspace
    pub persist_per_workspace: bool,
    
    /// Auto-save layout changes
    pub auto_save: bool,
    
    /// Layout save debounce (ms)
    pub save_debounce_ms: u32,
    
    /// Show tab close buttons
    pub show_tab_close: bool,
    
    /// Show tab context menu
    pub show_tab_menu: bool,
    
    /// Enable keyboard navigation
    pub keyboard_navigation: bool,
    
    /// Focus follows mouse
    pub focus_follows_mouse: bool,
    
    /// Tab drag threshold (px)
    pub drag_threshold: f32,
    
    /// Minimum tab width
    pub min_tab_width: f32,
    
    /// Maximum tab width
    pub max_tab_width: f32,
    
    /// Splitter width
    pub splitter_width: f32,
    
    /// Enable animated transitions
    pub animated: bool,
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            persist_per_project: true,
            persist_per_workspace: true,
            auto_save: true,
            save_debounce_ms: 500,
            show_tab_close: true,
            show_tab_menu: true,
            keyboard_navigation: true,
            focus_follows_mouse: false,
            drag_threshold: 8.0,
            min_tab_width: 120.0,
            max_tab_width: 300.0,
            splitter_width: 6.0,
            animated: true,
        }
    }
}

/// Saved layout for a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedLayout {
    pub name: String,
    pub dock_state: DockState<PanelKind>,
    pub active_panel: Option<PanelKind>,
    pub focused_panel: Option<PanelKind>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub modified_at: chrono::DateTime<chrono::Utc>,
    pub project_path: Option<PathBuf>,
}

/// Layout manager for persistence
pub struct LayoutManager {
    config: DockConfig,
    layouts: Arc<Mutex<HashMap<String, SavedLayout>>>,
    current_workspace: Arc<Mutex<String>>,
    last_save: Arc<Mutex<std::time::Instant>>,
    layouts_dir: PathBuf,
}

impl LayoutManager {
    pub fn new(config: DockConfig, layouts_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&layouts_dir).ok();
        
        Self {
            config,
            layouts: Arc::new(Mutex::new(HashMap::new())),
            current_workspace: Arc::new(Mutex::new("default".to_string())),
            last_save: Arc::new(Mutex::new(std::time::Instant::now())),
            layouts_dir,
        }
    }
    
    /// Save current dock state as a layout
    pub fn save_layout(&self, name: &str, dock_state: &DockState<PanelKind>, active_panel: Option<PanelKind>, focused_panel: Option<PanelKind>, project_path: Option<PathBuf>) {
        let mut layouts = self.layouts.lock().unwrap();
        
        let layout = SavedLayout {
            name: name.to_string(),
            dock_state: dock_state.clone(),
            active_panel,
            focused_panel,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            project_path,
        };
        
        layouts.insert(name.to_string(), layout);
        self.persist_layouts();
    }
    
    /// Load a layout by name
    pub fn load_layout(&self, name: &str) -> Option<SavedLayout> {
        let layouts = self.layouts.lock().unwrap();
        layouts.get(name).cloned()
    }
    
    /// Get current layout (auto-saved)
    pub fn current_layout(&self, dock_state: &DockState<PanelKind>, active_panel: Option<PanelKind>, focused_panel: Option<PanelKind>, project_path: Option<PathBuf>) -> SavedLayout {
        SavedLayout {
            name: "current".to_string(),
            dock_state: dock_state.clone(),
            active_panel,
            focused_panel,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            project_path,
        }
    }
    
    /// Auto-save current layout if enabled
    pub fn maybe_auto_save(&self, dock_state: &DockState<PanelKind>, active_panel: Option<PanelKind>, focused_panel: Option<PanelKind>, project_path: Option<PathBuf>) {
        if !self.config.auto_save {
            return;
        }
        
        let mut last_save = self.last_save.lock().unwrap();
        if last_save.elapsed().as_millis() < self.config.save_debounce_ms as u128 {
            return;
        }
        
        *last_save = std::time::Instant::now();
        
        let workspace = self.current_workspace.lock().unwrap().clone();
        self.save_layout(&workspace, dock_state, active_panel, focused_panel, project_path);
    }
    
    /// Set current workspace
    pub fn set_workspace(&self, name: &str) {
        *self.current_workspace.lock().unwrap() = name.to_string();
    }
    
    /// Get current workspace name
    pub fn current_workspace(&self) -> String {
        self.current_workspace.lock().unwrap().clone()
    }
    
    /// List all saved layouts
    pub fn list_layouts(&self) -> Vec<String> {
        self.layouts.lock().unwrap().keys().cloned().collect()
    }
    
    /// Delete a layout
    pub fn delete_layout(&self, name: &str) {
        self.layouts.lock().unwrap().remove(name);
        self.persist_layouts();
    }
    
    /// Rename a layout
    pub fn rename_layout(&self, old_name: &str, new_name: &str) {
        let mut layouts = self.layouts.lock().unwrap();
        if let Some(mut layout) = layouts.remove(old_name) {
            layout.name = new_name.to_string();
            layout.modified_at = chrono::Utc::now();
            layouts.insert(new_name.to_string(), layout);
            self.persist_layouts();
        }
    }
    
    /// Persist layouts to disk
    fn persist_layouts(&self) {
        let layouts = self.layouts.lock().unwrap();
        for (name, layout) in layouts.iter() {
            let path = self.layouts_dir.join(format!("{}.layout.json", name));
            if let Ok(json) = serde_json::to_string_pretty(layout) {
                std::fs::write(path, json).ok();
            }
        }
    }
    
    /// Load layouts from disk
    pub fn load_from_disk(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.layouts_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(layout) = serde_json::from_str::<SavedLayout>(&content) {
                            self.layouts.lock().unwrap().insert(layout.name.clone(), layout);
                        }
                    }
                }
            }
        }
    }
}

/// Enhanced dock area with better UX
pub struct EnhancedDockArea {
    config: DockConfig,
    layout_manager: Arc<LayoutManager>,
    dock_area: DockArea<PanelKind>,
    tab_hovered: Option<(NodeIndex, usize)>,
    tab_dragging: Option<(NodeIndex, usize)>,
    splitter_hovered: Option<NodeIndex>,
    splitter_dragging: Option<NodeIndex>,
    keyboard_focus: Option<NodeIndex>,
}

impl EnhancedDockArea {
    pub fn new(config: DockConfig, layouts_dir: PathBuf) -> Self {
        let layout_manager = Arc::new(LayoutManager::new(config.clone(), layouts_dir));
        layout_manager.load_from_disk();
        
        let dock_area = DockArea::new(Id::new("main_dock_area"));
        
        Self {
            config,
            layout_manager,
            dock_area,
            tab_hovered: None,
            tab_dragging: None,
            splitter_hovered: None,
            splitter_dragging: None,
            keyboard_focus: None,
        }
    }
    
    /// Show the dock area with all panels
    pub fn show(&mut self, ui: &mut egui::Ui, ui_state: &mut UiState, session: &mut Session) {
        let style = self.dock_style();
        
        // Handle keyboard navigation
        if self.config.keyboard_navigation {
            self.handle_keyboard_navigation(ui);
        }
        
        // Show dock area
        self.dock_area.show(ui, &style, |ui, panel_kind| {
            self.show_panel(ui, panel_kind, ui_state, session);
        });
        
        // Draw custom tab bar if needed
        self.draw_tab_bar(ui);
    }
    
    fn dock_style(&self) -> DockStyle {
        DockStyle::from_egui(ui.ctx().style().as_ref())
            .with_tab_bar_height(28.0)
            .with_splitter_width(self.config.splitter_width)
            .with_main_bg_color(ui.ctx().style().visuals.panel_fill)
    }
    
    fn show_panel(&mut self, ui: &mut egui::Ui, panel_kind: PanelKind, ui_state: &mut UiState, session: &mut Session) {
        match panel_kind {
            PanelKind::Timeline => crate::panels::timeline::show(ui, ui_state, session),
            PanelKind::SourceMonitor => crate::panels::source_monitor::show(ui, ui_state, session),
            PanelKind::ProgramMonitor => crate::panels::program_monitor::show(ui, ui_state, session),
            PanelKind::Project => crate::panels::project::show(ui, ui_state, session),
            PanelKind::Effects => crate::panels::effects::show(ui, ui_state, session),
            PanelKind::EffectControls => crate::panels::effect_controls::show(ui, ui_state, session),
            PanelKind::Color => crate::panels::color::show(ui, ui_state, session),
            PanelKind::AudioMixer => crate::panels::audio_mixer::show(ui, ui_state, session),
            PanelKind::AudioClipMixer => crate::panels::audio_clip_mixer::show(ui, ui_state, session),
            PanelKind::Text => crate::panels::text::show(ui, ui_state, session),
            PanelKind::Metadata => crate::panels::metadata::show(ui, ui_state, session),
            PanelKind::History => crate::panels::history::show(ui, ui_state, session),
            PanelKind::Scopes => crate::panels::scopes::show(ui, ui_state, session),
            PanelKind::ReferenceMonitor => crate::panels::reference_monitor::show(ui, ui_state, session),
            PanelKind::MediaBrowser => crate::panels::media_browser::show(ui, ui_state, session),
            PanelKind::EssentialGraphics => crate::panels::essential_graphics::show(ui, ui_state, session),
            PanelKind::EssentialSound => crate::panels::essential_sound::show(ui, ui_state, session),
            PanelKind::LumetriScopes => crate::panels::lumetri_scopes::show(ui, ui_state, session),
            PanelKind::Markers => crate::panels::markers::show(ui, ui_state, session),
            PanelKind::Timecode => crate::panels::timecode::show(ui, ui_state, session),
            PanelKind::Events => crate::panels::events::show(ui, ui_state, session),
            PanelKind::Progress => crate::panels::progress::show(ui, ui_state, session),
        }
    }
    
    /// Draw custom tab bar with close buttons and context menus
    fn draw_tab_bar(&mut self, ui: &mut egui::Ui) {
        // This would be integrated with the dock library's tab rendering
        // For now, the dock library handles tabs internally
    }
    
    /// Handle keyboard navigation between panels
    fn handle_keyboard_navigation(&mut self, ui: &mut egui::Ui) {
        if !ui.ctx().input(|i| i.key_pressed(egui::Key::Tab)) {
            return;
        }
        
        let shift = ui.ctx().input(|i| i.modifiers.shift);
        
        // Navigate between panels
        // This would integrate with the dock library's focus system
        // For now, we track which panel has keyboard focus
    }
    
    /// Get layout manager for saving/loading
    pub fn layout_manager(&self) -> &Arc<LayoutManager> {
        &self.layout_manager
    }
    
    /// Save current workspace layout
    pub fn save_workspace(&self, name: &str, active_panel: Option<PanelKind>, focused_panel: Option<PanelKind>, project_path: Option<PathBuf>) {
        self.layout_manager.save_layout(name, self.dock_area.state(), active_panel, focused_panel, project_path);
    }
    
    /// Load workspace layout
    pub fn load_workspace(&mut self, name: &str) -> Option<(Option<PanelKind>, Option<PanelKind>)> {
        if let Some(layout) = self.layout_manager.load_layout(name) {
            self.dock_area.set_state(layout.dock_state);
            self.layout_manager.set_workspace(name);
            return Some((layout.active_panel, layout.focused_panel));
        }
        None
    }
    
    /// Show workspace switcher UI
    pub fn show_workspace_switcher(&self, ui: &mut egui::Ui) {
        let current = self.layout_manager.current_workspace();
        let layouts = self.layout_manager.list_layouts();
        
        ui.horizontal(|ui| {
            ui.label("Workspace:");
            
            egui::ComboBox::from_id_source("workspace_switcher")
                .selected_text(&current)
                .show_ui(ui, |ui| {
                    for layout in &layouts {
                        if ui.selectable_label(*layout == current, layout).clicked() {
                            // Would trigger load
                        }
                    }
                });
            
            if ui.button("+").clicked() {
                // Show create new workspace dialog
            }
            
            if ui.button("💾").on_hover_text("Save current layout").clicked() {
                // Show save dialog
            }
        });
    }
}

/// Workspace switcher widget
pub fn show_workspace_switcher(ui: &mut egui::Ui, layout_manager: &Arc<LayoutManager>, on_select: impl FnOnce(&str)) {
    let current = layout_manager.current_workspace();
    let layouts = layout_manager.list_layouts();
    
    ui.horizontal(|ui| {
        ui.label("Workspace:");
        
        egui::ComboBox::from_id_source("workspace_switcher")
            .selected_text(&current)
            .show_ui(ui, |ui| {
                for layout in &layouts {
                    if ui.selectable_label(*layout == current, layout).clicked() {
                        on_select(layout);
                    }
                }
            });
        
        ui.menu_button("⋮", |ui| {
            if ui.button("New Workspace").clicked() {
                ui.close_menu();
            }
            if ui.button("Save Current").clicked() {
                ui.close_menu();
            }
            if ui.button("Rename").clicked() {
                ui.close_menu();
            }
            if ui.button("Delete").clicked() {
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Import Layout...").clicked() {
                ui.close_menu();
            }
            if ui.button("Export Layout...").clicked() {
                ui.close_menu();
            }
        });
    })
}

/// Panel tab context menu
pub fn show_panel_tab_menu(ui: &mut egui::Ui, panel_kind: PanelKind, dock_area: &mut DockArea<PanelKind>, node: NodeIndex, tab_index: usize) {
    egui::ContextMenu::show(ui, |ui| {
        ui.heading(format!("{:?}", panel_kind));
        ui.separator();
        
        if ui.button("Close Tab").clicked() {
            dock_area.remove_tab(node, tab_index);
            ui.close_menu();
        }
        
        if ui.button("Close Other Tabs").clicked() {
            dock_area.close_other_tabs(node, tab_index);
            ui.close_menu();
        }
        
        if ui.button("Close All Tabs").clicked() {
            dock_area.close_all_tabs(node);
            ui.close_menu();
        }
        
        ui.separator();
        
        if ui.button("Move to New Window").clicked() {
            dock_area.move_tab_to_new_window(node, tab_index);
            ui.close_menu();
        }
        
        if ui.button("Duplicate").clicked() {
            dock_area.duplicate_tab(node, tab_index);
            ui.close_menu();
        }
        
        ui.separator();
        
        if ui.button("Minimize").clicked() {
            dock_area.minimize_node(node);
            ui.close_menu();
        }
        
        if ui.button("Maximize").clicked() {
            dock_area.maximize_node(node);
            ui.close_menu();
        }
    });
}

/// Splitter handle with visual feedback
pub fn show_splitter_handle(ui: &mut egui::Ui, rect: Rect, is_horizontal: bool, hovered: bool, dragging: bool) -> Response {
    let sense = Sense::drag();
    let response = ui.allocate_rect(rect, sense);
    
    let painter = ui.painter();
    let color = if dragging {
        ui.style().visuals.widgets.active.bg_fill
    } else if hovered {
        ui.style().visuals.widgets.hovered.bg_fill
    } else {
        ui.style().visuals.widgets.inactive.bg_fill
    };
    
    painter.rect_filled(rect, 0.0, color);
    
    // Draw drag indicator
    let center = rect.center();
    let indicator_size = if is_horizontal { Vec2::new(24.0, 4.0) } else { Vec2::new(4.0, 24.0) };
    let indicator_rect = Rect::from_center_size(center, indicator_size);
    painter.rect_filled(indicator_rect, 2.0, ui.style().visuals.text_color().gamma_multiply(0.3));
    
    // Draw lines on indicator
    let line_color = ui.style().visuals.text_color().gamma_multiply(0.5);
    let stroke = Stroke::new(1.5, line_color);
    
    if is_horizontal {
        for i in 0..3 {
            let y = indicator_rect.top() + 4.0 + i as f32 * 6.0;
            painter.line_segment([
                egui::pos2(indicator_rect.left() + 4.0, y),
                egui::pos2(indicator_rect.right() - 4.0, y),
            ], stroke);
        }
    } else {
        for i in 0..3 {
            let x = indicator_rect.left() + 4.0 + i as f32 * 6.0;
            painter.line_segment([
                egui::pos2(x, indicator_rect.top() + 4.0),
                egui::pos2(x, indicator_rect.bottom() - 4.0),
            ], stroke);
        }
    }
    
    response
}

/// Keyboard shortcuts for docking
pub fn dock_keyboard_shortcuts() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Ctrl+Shift+T", "New tab"),
        ("Ctrl+W", "Close tab"),
        ("Ctrl+Shift+W", "Close panel"),
        ("Ctrl+1-9", "Switch to tab 1-9"),
        ("Ctrl+Tab", "Next tab"),
        ("Ctrl+Shift+Tab", "Previous tab"),
        ("Alt+1-9", "Focus panel 1-9"),
        ("F11", "Toggle fullscreen"),
        ("Ctrl+M", "Minimize/Maximize panel"),
        ("Ctrl+Shift+S", "Save layout"),
        ("Ctrl+Shift+L", "Load layout"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_dock_config_default() {
        let config = DockConfig::default();
        assert!(config.auto_save);
        assert!(config.keyboard_navigation);
    }
    
    #[test]
    fn test_layout_manager() {
        let temp_dir = std::env::temp_dir().join("test_layouts");
        let manager = LayoutManager::new(DockConfig::default(), temp_dir.clone());
        
        let dock_state = DockState::new(PanelKind::Timeline);
        manager.save_layout("test", &dock_state, Some(PanelKind::Timeline), Some(PanelKind::Timeline), None);
        
        let loaded = manager.load_layout("test");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().name, "test");
    }
}