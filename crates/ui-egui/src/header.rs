//! Header Bar UI - Search, Sound, and other top-right controls
//!
//! Fixes #3447: Search button does nothing
//! Fixes #3443: Sound button doesn't respond

use crate::state::UiState;
use filmcraft_engine::Session;
use egui::{Ui, Response, Sense, Vec2, Color32, Stroke, RichText, FontId, IconData};
use std::sync::Arc;

/// Header bar component with search, audio meter, and other controls
pub struct HeaderBar {
    /// Search state
    search_open: bool,
    search_query: String,
    
    /// Audio meter state
    show_audio_meter: bool,
    audio_meter_level: f32,
    
    /// Notification state
    notifications: Vec<Notification>,
    
    /// User menu state
    user_menu_open: bool,
}

#[derive(Debug, Clone)]
struct Notification {
    id: usize,
    message: String,
    level: NotificationLevel,
    timestamp: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum NotificationLevel {
    Info,
    Warning,
    Error,
}

impl HeaderBar {
    pub fn new() -> Self {
        Self {
            search_open: false,
            search_query: String::new(),
            show_audio_meter: true,
            audio_meter_level: 0.0,
            notifications: Vec::new(),
            user_menu_open: false,
        }
    }

    /// Show the header bar
    pub fn show(&mut self, ui: &mut Ui, ui_state: &mut UiState, session: &mut Session) {
        ui.horizontal_top(|ui| {
            // Left side: App menu (macOS) or window controls
            #[cfg(target_os = "macos")]
            {
                self.show_mac_menu_bar(ui, session);
            }
            
            #[cfg(not(target_os = "macos"))]
            {
                self.show_window_title(ui);
            }
            
            ui.separator();
            
            // Center: Workspace tabs / sequence name
            self.show_workspace_tabs(ui, ui_state, session);
            
            ui.separator();
            
            // Right side: Search, Audio, Notifications, User
            self.show_search_button(ui, ui_state, session);
            self.show_audio_button(ui, ui_state, session);
            self.show_notification_button(ui);
            self.show_user_menu(ui, session);
        });
    }

    #[cfg(target_os = "macos")]
    fn show_mac_menu_bar(&self, ui: &mut Ui, _session: &mut Session) {
        // macOS native menu bar is handled separately
        // This space reserved for traffic lights
        ui.allocate_space(Vec2::new(80.0, 22.0));
    }

    #[cfg(not(target_os = "macos"))]
    fn show_window_title(&self, ui: &mut Ui) {
        ui.label(RichText::new("FilmCraft").size(14.0).strong());
    }

    fn show_workspace_tabs(&self, ui: &mut Ui, ui_state: &UiState, _session: &mut Session) {
        // Show current workspace/sequence
        if let Some(seq) = ui_state.active_sequence {
            ui.label(format!("Sequence: {}", seq));
        } else {
            ui.label("No Sequence");
        }
    }

    /// Search button - FIXES #3447
    fn show_search_button(&mut self, ui: &mut Ui, ui_state: &mut UiState, session: &mut Session) {
        let response = ui.add(
            egui::Button::new(egui::RichText::new("🔍").size(16.0))
                .sense(Sense::click())
                .frame(false)
        );

        if response.clicked() {
            self.search_open = !self.search_open;
            
            if self.search_open {
                // Execute search command
                let _ = session.execute("ui.showSearch", serde_json::json!({}));
            } else {
                let _ = session.execute("ui.hideSearch", serde_json::json!({}));
            }
        }

        // Show search popup when open
        if self.search_open {
            egui::popup::show_tooltip_at_pointer(ui.ctx(), egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("search_popup")), |ui| {
                self.show_search_popup(ui, ui_state, session);
            });
        }

        // Tooltip
        response.on_hover_text("Search Commands (Cmd+Shift+P)");
    }

    fn show_search_popup(&mut self, ui: &mut Ui, _ui_state: &mut UiState, session: &mut Session) {
        ui.set_min_width(300.0);
        ui.vertical(|ui| {
            ui.heading("Search Commands");
            ui.separator();
            
            let response = ui.text_edit_singleline(&mut self.search_query);
            response.request_focus();
            
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.search_open = false;
                self.search_query.clear();
            }
            
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.execute_search(session);
            }
            
            ui.separator();
            
            // Show matching commands
            self.show_matching_commands(ui, session);
        });
    }

    fn show_matching_commands(&self, ui: &mut Ui, session: &Session) {
        let commands = session.commands();
        let query = self.search_query.to_lowercase();
        
        let matches: Vec<_> = commands.iter()
            .filter(|cmd| {
                cmd.label.to_lowercase().contains(&query) ||
                cmd.id.to_lowercase().contains(&query)
            })
            .take(10)
            .collect();
        
        for cmd in matches {
            let shortcut = cmd.shortcut.unwrap_or("");
            let label = format!("{}  {}", cmd.label, shortcut);
            
            if ui.selectable_label(false, label).clicked() {
                // Execute command
                let _ = session.execute(&cmd.id, serde_json::json!({}));
                // Close popup handled by caller
            }
        }
        
        if matches.is_empty() && !query.is_empty() {
            ui.label("No commands found");
        }
    }

    fn execute_search(&mut self, session: &mut Session) {
        // Execute the first matching command
        let commands = session.commands();
        let query = self.search_query.to_lowercase();
        
        if let Some(cmd) = commands.iter().find(|cmd| {
            cmd.label.to_lowercase().contains(&query) ||
            cmd.id.to_lowercase().contains(&query)
        }) {
            let _ = session.execute(&cmd.id, serde_json::json!({}));
        }
        
        self.search_open = false;
        self.search_query.clear();
    }

    /// Audio button with meter - FIXES #3443
    fn show_audio_button(&mut self, ui: &mut Ui, ui_state: &mut UiState, session: &mut Session) {
        // Get current audio level from session
        self.audio_meter_level = self.get_audio_level(session);
        
        let button_response = ui.add(
            egui::Button::new(
                egui::RichText::new(if self.show_audio_meter { "🔊" } else { "🔇" }).size(16.0)
            )
            .sense(Sense::click())
            .frame(false)
        );

        if button_response.clicked() {
            self.show_audio_meter = !self.show_audio_meter;
            
            if self.show_audio_meter {
                let _ = session.execute("audio.mixer.show", serde_json::json!({}));
            } else {
                let _ = session.execute("audio.mixer.hide", serde_json::json!({}));
            }
        }

        // Show audio meter popup on hover or click
        if button_response.hovered() || self.show_audio_meter {
            egui::popup::show_tooltip_at_pointer(ui.ctx(), egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("audio_popup")), |ui| {
                self.show_audio_popup(ui, ui_state, session);
            });
        }

        button_response.on_hover_text("Audio Mixer (Shift+M)");
    }

    fn show_audio_popup(&mut self, ui: &mut Ui, _ui_state: &mut UiState, session: &mut Session) {
        ui.set_min_width(250.0);
        ui.vertical(|ui| {
            ui.heading("Audio Mixer");
            ui.separator();
            
            // Master meter
            ui.horizontal(|ui| {
                ui.label("Master");
                self.draw_audio_meter(ui, self.audio_meter_level);
            });
            
            ui.separator();
            
            // Track meters (simplified)
            if let Some(seq) = session.active_sequence() {
                for track in seq.audio_tracks() {
                    ui.horizontal(|ui| {
                        ui.label(&track.name);
                        let level = self.get_track_level(session, track.id);
                        self.draw_audio_meter(ui, level);
                    });
                }
            }
            
            ui.separator();
            
            // Quick controls
            if ui.button("Open Audio Mixer Panel").clicked() {
                let _ = session.execute("panel.show", serde_json::json!({"panel": "audioMixer"}));
            }
            
            if ui.button("Audio Hardware Settings").clicked() {
                let _ = session.execute("settings.open", serde_json::json!({"page": "audioHardware"}));
            }
        });
    }

    fn draw_audio_meter(&self, ui: &mut Ui, level: f32) {
        let meter_width = 100.0;
        let meter_height = 16.0;
        
        let (rect, _) = ui.allocate_exact_size(Vec2::new(meter_width, meter_height), Sense::hover());
        let painter = ui.painter();
        
        // Background
        painter.rect_filled(rect, 2.0, Color32::from_gray(40));
        
        // Meter fill
        let fill_width = (meter_width * level.clamp(0.0, 1.0)) as f32;
        let fill_rect = egui::Rect::from_min_size(rect.min, Vec2::new(fill_width, meter_height));
        
        let color = if level > 0.9 {
            Color32::RED
        } else if level > 0.7 {
            Color32::YELLOW
        } else {
            Color32::GREEN
        };
        
        painter.rect_filled(fill_rect, 2.0, color);
        
        // Peak indicator
        if level > 0.95 {
            painter.text(
                rect.right_top() + Vec2::new(-20.0, 2.0),
                egui::Align2::RIGHT_TOP,
                "PEAK",
                FontId::proportional(8.0),
                Color32::RED,
            );
        }
    }

    fn get_audio_level(&self, session: &Session) -> f32 {
        // Get master output level from session
        session.master_audio_level().unwrap_or(0.0)
    }

    fn get_track_level(&self, session: &Session, track_id: filmcraft_project::TrackId) -> f32 {
        session.track_audio_level(track_id).unwrap_or(0.0)
    }

    /// Notification button
    fn show_notification_button(&mut self, ui: &mut Ui) {
        let unread = self.notifications.iter().filter(|n| !n.timestamp.elapsed().as_secs() > 300).count();
        
        let label = if unread > 0 {
            format!("🔔 {}", unread)
        } else {
            "🔔".to_string()
        };
        
        let response = ui.add(
            egui::Button::new(egui::RichText::new(label).size(16.0))
                .sense(Sense::click())
                .frame(false)
        );
        
        if response.clicked() {
            self.show_notifications_popup(ui);
        }
    }

    fn show_notifications_popup(&mut self, ui: &mut Ui) {
        egui::popup::show_tooltip_at_pointer(ui.ctx(), egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("notifications_popup")), |ui| {
            ui.set_min_width(300.0);
            ui.vertical(|ui| {
                ui.heading("Notifications");
                ui.separator();
                
                if self.notifications.is_empty() {
                    ui.label("No notifications");
                } else {
                    for notif in &self.notifications {
                        let color = match notif.level {
                            NotificationLevel::Info => Color32::BLUE,
                            NotificationLevel::Warning => Color32::YELLOW,
                            NotificationLevel::Error => Color32::RED,
                        };
                        
                        ui.horizontal(|ui| {
                            ui.colored_label(color, "●");
                            ui.label(&notif.message);
                        });
                    }
                }
            });
        });
    }

    /// User menu
    fn show_user_menu(&mut self, ui: &mut Ui, session: &mut Session) {
        let response = ui.add(
            egui::Button::new(egui::RichText::new("👤").size(16.0))
                .sense(Sense::click())
                .frame(false)
        );
        
        if response.clicked() {
            self.user_menu_open = !self.user_menu_open;
        }
        
        if self.user_menu_open {
            egui::popup::show_tooltip_at_pointer(ui.ctx(), egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("user_menu_popup")), |ui| {
                ui.vertical(|ui| {
                    if ui.button("Preferences").clicked() {
                        let _ = session.execute("settings.open", serde_json::json!({}));
                        self.user_menu_open = false;
                    }
                    if ui.button("Keyboard Shortcuts").clicked() {
                        let _ = session.execute("ui.showShortcuts", serde_json::json!({}));
                        self.user_menu_open = false;
                    }
                    ui.separator();
                    if ui.button("Documentation").clicked() {
                        let _ = session.execute("help.documentation", serde_json::json!({}));
                        self.user_menu_open = false;
                    }
                    if ui.button("Discord Community").clicked() {
                        let _ = session.execute("help.discord", serde_json::json!({}));
                        self.user_menu_open = false;
                    }
                    ui.separator();
                    if ui.button("About FilmCraft").clicked() {
                        let _ = session.execute("help.about", serde_json::json!({}));
                        self.user_menu_open = false;
                    }
                });
            });
        }
    }

    /// Add a notification
    pub fn add_notification(&mut self, message: String, level: NotificationLevel) {
        self.notifications.push(Notification {
            id: self.notifications.len(),
            message,
            level,
            timestamp: std::time::Instant::now(),
        });
        
        // Keep only last 50
        if self.notifications.len() > 50 {
            self.notifications.remove(0);
        }
    }

    /// Update audio meter level (called from audio callback)
    pub fn set_audio_level(&mut self, level: f32) {
        self.audio_meter_level = level.clamp(0.0, 1.0);
    }
}

impl Default for HeaderBar {
    fn default() -> Self {
        Self::new()
    }
}

/// Search command palette (Cmd+Shift+P)
pub fn show_command_palette(ui: &mut Ui, ui_state: &mut UiState, session: &mut Session) {
    if ui_state.show_command_palette {
        egui::Window::new("Command Palette")
            .id(egui::Id::new("command_palette"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 100.0))
            .show(ui.ctx(), |ui| {
                let mut query = String::new();
                ui.text_edit_singleline(&mut query);
                
                ui.separator();
                
                let commands = session.commands();
                let matches: Vec<_> = commands.iter()
                    .filter(|cmd| {
                        cmd.label.to_lowercase().contains(&query.to_lowercase()) ||
                        cmd.id.to_lowercase().contains(&query.to_lowercase())
                    })
                    .take(15)
                    .collect();
                
                for cmd in matches {
                    let shortcut = cmd.shortcut.unwrap_or("");
                    if ui.selectable_label(false, format!("{}  {}", cmd.label, shortcut)).clicked() {
                        let _ = session.execute(&cmd.id, serde_json::json!({}));
                        ui_state.show_command_palette = false;
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_bar_creation() {
        let header = HeaderBar::new();
        assert!(!header.search_open);
        assert!(header.show_audio_meter);
    }

    #[test]
    fn test_notification_levels() {
        let mut header = HeaderBar::new();
        header.add_notification("Test info".to_string(), NotificationLevel::Info);
        header.add_notification("Test warning".to_string(), NotificationLevel::Warning);
        header.add_notification("Test error".to_string(), NotificationLevel::Error);
        
        assert_eq!(header.notifications.len(), 3);
    }
}