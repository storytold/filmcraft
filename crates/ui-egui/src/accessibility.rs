//! Accessibility Support - Screen readers, keyboard navigation, high contrast, RTL
//!
//! New Feature: Comprehensive accessibility support for inclusive design

use std::sync::{Arc, Mutex};
use egui::{Color32, FontId, FontFamily, TextStyle, Widget, Response, Sense, Vec2, Rect, Align2, Key, Modifiers};
use serde::{Deserialize, Serialize};

/// Accessibility configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityConfig {
    /// Enable screen reader support (ARIA labels, live regions)
    pub screen_reader: bool,
    
    /// Enable full keyboard navigation
    pub keyboard_navigation: bool,
    
    /// Focus indicator style
    pub focus_indicator: FocusIndicatorStyle,
    
    /// High contrast mode
    pub high_contrast: bool,
    
    /// Large text scale
    pub large_text_scale: f32,
    
    /// Reduced motion
    pub reduced_motion: bool,
    
    /// Color blind safe colors
    pub color_blind_safe: bool,
    
    /// RTL (Right-to-Left) language support
    pub rtl_support: bool,
    
    /// Announce changes via live regions
    pub announce_changes: bool,
    
    /// Minimum touch target size (for touch screens)
    pub min_touch_target: f32,
    
    /// Focus trap in modals
    pub focus_trap: bool,
    
    /// Skip links for main content
    pub skip_links: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FocusIndicatorStyle {
    /// Default browser/OS focus ring
    Default,
    /// Custom visible focus ring
    Visible,
    /// High visibility focus ring
    HighVisibility,
    /// No focus indicator (not recommended)
    None,
}

impl Default for AccessibilityConfig {
    fn default() -> Self {
        Self {
            screen_reader: true,
            keyboard_navigation: true,
            focus_indicator: FocusIndicatorStyle::Visible,
            high_contrast: false,
            large_text_scale: 1.0,
            reduced_motion: false,
            color_blind_safe: false,
            rtl_support: false,
            announce_changes: true,
            min_touch_target: 44.0, // WCAG 2.1 AA minimum
            focus_trap: true,
            skip_links: true,
        }
    }
}

/// Accessibility manager
pub struct AccessibilityManager {
    config: AccessibilityConfig,
    live_region: Arc<Mutex<String>>,
    focus_stack: Arc<Mutex<Vec<FocusTarget>>>,
    skip_link_target: Option<String>,
    announced_messages: Arc<Mutex<Vec<AnnouncedMessage>>>,
}

#[derive(Debug, Clone)]
struct FocusTarget {
    id: egui::Id,
    label: String,
    role: AriaRole,
}

#[derive(Debug, Clone)]
struct AnnouncedMessage {
    message: String,
    priority: AriaPriority,
    timestamp: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AriaRole {
    Button,
    Link,
    Textbox,
    Combobox,
    Listbox,
    Menu,
    MenuItem,
    Tab,
    TabPanel,
    Dialog,
    AlertDialog,
    Region,
    Status,
    Log,
    Timer,
    Marquee,
    Heading,
    Image,
    Separator,
    Slider,
    SpinButton,
    ProgressBar,
    Toolbar,
    Tooltip,
    Tree,
    TreeItem,
    Grid,
    Row,
    Cell,
    ColumnHeader,
    RowHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AriaPriority {
    Polite,
    Assertive,
    Off,
}

impl AccessibilityManager {
    pub fn new(config: AccessibilityConfig) -> Self {
        Self {
            config,
            live_region: Arc::new(Mutex::new(String::new())),
            focus_stack: Arc::new(Mutex::new(Vec::new())),
            skip_link_target: None,
            announced_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }
    
    /// Get current config
    pub fn config(&self) -> &AccessibilityConfig {
        &self.config
    }
    
    /// Update config
    pub fn set_config(&mut self, config: AccessibilityConfig) {
        self.config = config;
    }
    
    /// Announce message to screen readers
    pub fn announce(&self, message: &str, priority: AriaPriority) {
        if !self.config.announce_changes && !self.config.screen_reader {
            return;
        }
        
        let msg = AnnouncedMessage {
            message: message.to_string(),
            priority,
            timestamp: std::time::Instant::now(),
        };
        
        self.announced_messages.lock().unwrap().push(msg);
        
        // Update live region
        *self.live_region.lock().unwrap() = message.to_string();
    }
    
    /// Get live region content for screen readers
    pub fn live_region_content(&self) -> String {
        self.live_region.lock().unwrap().clone()
    }
    
    /// Push focus target onto stack
    pub fn push_focus(&self, id: egui::Id, label: &str, role: AriaRole) {
        self.focus_stack.lock().unwrap().push(FocusTarget {
            id,
            label: label.to_string(),
            role,
        });
    }
    
    /// Pop focus target from stack
    pub fn pop_focus(&self) -> Option<FocusTarget> {
        self.focus_stack.lock().unwrap().pop()
    }
    
    /// Get current focus target
    pub fn current_focus(&self) -> Option<FocusTarget> {
        self.focus_stack.lock().unwrap().last().cloned()
    }
    
    /// Set skip link target
    pub fn set_skip_link(&mut self, target_id: &str) {
        self.skip_link_target = Some(target_id.to_string());
    }
    
    /// Get skip link target
    pub fn skip_link_target(&self) -> Option<String> {
        self.skip_link_target.clone()
    }
    
    /// Check if element should be in tab order
    pub fn is_tab_stoppable(&self, widget: &dyn Widget) -> bool {
        // In real implementation, check widget's tab_index and disabled state
        true
    }
    
    /// Generate ARIA attributes for a widget
    pub fn aria_attributes(&self, role: AriaRole, label: &str, described_by: Option<&str>, expanded: Option<bool>, selected: Option<bool>, disabled: bool, required: bool) -> AriaAttributes {
        let mut attrs = AriaAttributes::default();
        attrs.role = Some(role);
        attrs.label = Some(label.to_string());
        attrs.described_by = described_by.map(|s| s.to_string());
        attrs.expanded = expanded;
        attrs.selected = selected;
        attrs.disabled = Some(disabled);
        attrs.required = Some(required);
        attrs
    }
    
    /// Get focus indicator style
    pub fn focus_style(&self) -> FocusStyle {
        match self.config.focus_indicator {
            FocusIndicatorStyle::Default => FocusStyle::default(),
            FocusIndicatorStyle::Visible => FocusStyle {
                width: 2.0,
                color: Color32::from_rgb(0, 120, 215),
                offset: 2.0,
            },
            FocusIndicatorStyle::HighVisibility => FocusStyle {
                width: 3.0,
                color: Color32::YELLOW,
                offset: 3.0,
            },
            FocusIndicatorStyle::None => FocusStyle {
                width: 0.0,
                color: Color32::TRANSPARENT,
                offset: 0.0,
            },
        }
    }
}

/// ARIA attributes for widgets
#[derive(Debug, Default, Clone)]
pub struct AriaAttributes {
    pub role: Option<AriaRole>,
    pub label: Option<String>,
    pub labelled_by: Option<String>,
    pub described_by: Option<String>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    pub disabled: Option<bool>,
    pub required: Option<bool>,
    pub hidden: Option<bool>,
    pub live: Option<AriaPriority>,
    pub controls: Option<String>,
    pub owns: Option<String>,
    pub level: Option<u32>, // For headings
    pub pos_in_set: Option<u32>,
    pub set_size: Option<u32>,
    pub value_min: Option<f32>,
    pub value_max: Option<f32>,
    pub value_now: Option<f32>,
    pub value_text: Option<String>,
}

/// Focus indicator style
#[derive(Debug, Clone)]
pub struct FocusStyle {
    pub width: f32,
    pub color: Color32,
    pub offset: f32,
}

impl Default for FocusStyle {
    fn default() -> Self {
        Self {
            width: 2.0,
            color: Color32::from_rgb(0, 120, 215),
            offset: 2.0,
        }
    }
}

/// Accessible button widget
pub struct AccessibleButton<'a> {
    label: &'a str,
    on_click: Option<Box<dyn FnOnce() + 'a>>,
    aria: AriaAttributes,
    enabled: bool,
    focus_style: FocusStyle,
}

impl<'a> AccessibleButton<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            on_click: None,
            aria: AriaAttributes::default(),
            enabled: true,
            focus_style: FocusStyle::default(),
        }
    }
    
    pub fn on_click<F: FnOnce() + 'a>(mut self, f: F) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
    
    pub fn aria(mut self, aria: AriaAttributes) -> Self {
        self.aria = aria;
        self
    }
    
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    
    pub fn focus_style(mut self, style: FocusStyle) -> Self {
        self.focus_style = style;
        self
    }
}

impl<'a> Widget for AccessibleButton<'a> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let mut btn = egui::Button::new(self.label);
        
        if !self.enabled {
            btn = btn.fill(ui.style().visuals.widgets.inactive.bg_fill);
        }
        
        let response = btn.ui(ui);
        
        // Apply focus style
        if response.has_focus() && self.focus_style.width > 0.0 {
            let rect = response.rect.expand(self.focus_style.offset);
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(self.focus_style.width, self.focus_style.color),
            );
        }
        
        // Handle click
        if response.clicked() {
            if let Some(f) = self.on_click {
                f();
            }
        }
        
        // Add ARIA attributes to response (would be used by screen reader bridge)
        // In real implementation, these would be output to the accessibility tree
        
        response
    }
}

/// Accessible slider widget
pub struct AccessibleSlider<'a> {
    value: &'a mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    label: &'a str,
    aria: AriaAttributes,
    focus_style: FocusStyle,
}

impl<'a> AccessibleSlider<'a> {
    pub fn new(value: &'a mut f32, range: std::ops::RangeInclusive<f32>) -> Self {
        Self {
            value,
            range,
            step: (range.end() - range.start()) / 100.0,
            label: "",
            aria: AriaAttributes::default(),
            focus_style: FocusStyle::default(),
        }
    }
    
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }
    
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }
    
    pub fn aria(mut self, aria: AriaAttributes) -> Self {
        self.aria = aria;
        self
    }
    
    pub fn focus_style(mut self, style: FocusStyle) -> Self {
        self.focus_style = style;
        self
    }
}

impl<'a> Widget for AccessibleSlider<'a> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let mut slider = egui::Slider::new(self.value, self.range).step_by(self.step);
        
        if !self.label.is_empty() {
            slider = slider.text(self.label);
        }
        
        let response = slider.ui(ui);
        
        // Apply focus style
        if response.has_focus() && self.focus_style.width > 0.0 {
            let rect = response.rect.expand(self.focus_style.offset);
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(self.focus_style.width, self.focus_style.color),
            );
        }
        
        // Update ARIA value
        // In real implementation, update live region
        
        response
    }
}

/// Accessible text input widget
pub struct AccessibleTextInput<'a> {
    value: &'a mut String,
    label: &'a str,
    placeholder: Option<&'a str>,
    aria: AriaAttributes,
    multiline: bool,
    focus_style: FocusStyle,
}

impl<'a> AccessibleTextInput<'a> {
    pub fn new(value: &'a mut String, label: &'a str) -> Self {
        Self {
            value,
            label,
            placeholder: None,
            aria: AriaAttributes::default(),
            multiline: false,
            focus_style: FocusStyle::default(),
        }
    }
    
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }
    
    pub fn aria(mut self, aria: AriaAttributes) -> Self {
        self.aria = aria;
        self
    }
    
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }
    
    pub fn focus_style(mut self, style: FocusStyle) -> Self {
        self.focus_style = style;
        self
    }
}

impl<'a> Widget for AccessibleTextInput<'a> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let mut text_edit = if self.multiline {
            egui::TextEdit::multiline(self.value)
        } else {
            egui::TextEdit::singleline(self.value)
        };
        
        if let Some(placeholder) = self.placeholder {
            text_edit = text_edit.hint_text(placeholder);
        }
        
        let response = text_edit.ui(ui);
        
        // Apply focus style
        if response.has_focus() && self.focus_style.width > 0.0 {
            let rect = response.rect.expand(self.focus_style.offset);
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(self.focus_style.width, self.focus_style.color),
            );
        }
        
        response
    }
}

/// Skip link widget (for keyboard users to skip to main content)
pub struct SkipLink {
    target_id: String,
    text: String,
}

impl SkipLink {
    pub fn new(target_id: &str, text: &str) -> Self {
        Self {
            target_id: target_id.to_string(),
            text: text.to_string(),
        }
    }
}

impl Widget for SkipLink {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        // Skip links are typically hidden until focused
        let response = ui.add(
            egui::Button::new(self.text)
                .fill(Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE)
        );
        
        // In real implementation, this would be positioned absolutely at top of page
        // and only visible when focused
        
        response
    }
}

/// Live region widget for screen reader announcements
pub struct LiveRegion {
    content: String,
    priority: AriaPriority,
}

impl LiveRegion {
    pub fn new(content: &str, priority: AriaPriority) -> Self {
        Self {
            content: content.to_string(),
            priority,
        }
    }
}

impl Widget for LiveRegion {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        // Live regions are typically hidden visually but announced by screen readers
        // In web, this would be an aria-live region
        // In native, we'd use platform accessibility APIs
        
        let response = ui.allocate_response(Vec2::ZERO, Sense::hover());
        
        // Store for accessibility bridge
        // In real implementation, this would interface with platform APIs
        
        response
    }
}

/// Keyboard navigation helper
pub struct KeyboardNavigator {
    config: AccessibilityConfig,
    focus_order: Vec<egui::Id>,
    current_index: usize,
}

impl KeyboardNavigator {
    pub fn new(config: AccessibilityConfig) -> Self {
        Self {
            config,
            focus_order: Vec::new(),
            current_index: 0,
        }
    }
    
    /// Register a focusable element
    pub fn register(&mut self, id: egui::Id) {
        if !self.focus_order.contains(&id) {
            self.focus_order.push(id);
        }
    }
    
    /// Unregister a focusable element
    pub fn unregister(&mut self, id: egui::Id) {
        self.focus_order.retain(|&x| x != id);
        if self.current_index >= self.focus_order.len() {
            self.current_index = self.focus_order.len().saturating_sub(1);
        }
    }
    
    /// Move focus to next element
    pub fn next(&mut self, ui: &mut egui::Ui) -> bool {
        if self.focus_order.is_empty() {
            return false;
        }
        
        self.current_index = (self.current_index + 1) % self.focus_order.len();
        ui.memory_mut(|mem| mem.request_focus(self.focus_order[self.current_index]));
        true
    }
    
    /// Move focus to previous element
    pub fn previous(&mut self, ui: &mut egui::Ui) -> bool {
        if self.focus_order.is_empty() {
            return false;
        }
        
        if self.current_index == 0 {
            self.current_index = self.focus_order.len() - 1;
        } else {
            self.current_index -= 1;
        }
        ui.memory_mut(|mem| mem.request_focus(self.focus_order[self.current_index]));
        true
    }
    
    /// Move focus to first element
    pub fn first(&mut self, ui: &mut egui::Ui) -> bool {
        if self.focus_order.is_empty() {
            return false;
        }
        self.current_index = 0;
        ui.memory_mut(|mem| mem.request_focus(self.focus_order[0]));
        true
    }
    
    /// Move focus to last element
    pub fn last(&mut self, ui: &mut egui::Ui) -> bool {
        if self.focus_order.is_empty() {
            return false;
        }
        self.current_index = self.focus_order.len() - 1;
        ui.memory_mut(|mem| mem.request_focus(self.focus_order[self.current_index]));
        true
    }
    
    /// Handle keyboard navigation keys
    pub fn handle_keys(&mut self, ui: &mut egui::Ui) {
        if !self.config.keyboard_navigation {
            return;
        }
        
        let input = ui.input(|i| i.clone());
        
        // Tab navigation
        if input.key_pressed(Key::Tab) {
            if input.modifiers.shift {
                self.previous(ui);
            } else {
                self.next(ui);
            }
        }
        
        // Arrow key navigation (for menus, tabs, etc.)
        if input.key_pressed(Key::ArrowRight) || input.key_pressed(Key::ArrowDown) {
            self.next(ui);
        }
        if input.key_pressed(Key::ArrowLeft) || input.key_pressed(Key::ArrowUp) {
            self.previous(ui);
        }
        
        // Home/End
        if input.key_pressed(Key::Home) {
            self.first(ui);
        }
        if input.key_pressed(Key::End) {
            self.last(ui);
        }
        
        // Escape to close modals/menus
        if input.key_pressed(Key::Escape) {
            // Would close topmost modal/menu
        }
    }
}

/// RTL (Right-to-Left) layout helper
pub struct RtlLayout {
    enabled: bool,
}

impl RtlLayout {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
    
    /// Apply RTL transformation to a rect
    pub fn transform_rect(&self, rect: Rect, container_width: f32) -> Rect {
        if !self.enabled {
            return rect;
        }
        
        Rect::from_min_max(
            egui::pos2(container_width - rect.max.x, rect.min.y),
            egui::pos2(container_width - rect.min.x, rect.max.y),
        )
    }
    
    /// Apply RTL transformation to a position
    pub fn transform_pos(&self, pos: egui::Pos2, container_width: f32) -> egui::Pos2 {
        if !self.enabled {
            return pos;
        }
        egui::pos2(container_width - pos.x, pos.y)
    }
    
    /// Get horizontal alignment for RTL
    pub fn align(&self, align: Align2) -> Align2 {
        if !self.enabled {
            return align;
        }
        
        match align {
            Align2::LEFT_TOP => Align2::RIGHT_TOP,
            Align2::LEFT_CENTER => Align2::RIGHT_CENTER,
            Align2::LEFT_BOTTOM => Align2::RIGHT_BOTTOM,
            Align2::RIGHT_TOP => Align2::LEFT_TOP,
            Align2::RIGHT_CENTER => Align2::LEFT_CENTER,
            Align2::RIGHT_BOTTOM => Align2::LEFT_BOTTOM,
            _ => align,
        }
    }
    
    /// Check if RTL is enabled
    pub fn is_rtl(&self) -> bool {
        self.enabled
    }
}

/// Accessibility settings UI
pub fn show_accessibility_settings(ui: &mut egui::Ui, config: &mut AccessibilityConfig) {
    ui.heading("Accessibility");
    ui.separator();
    
    ui.checkbox(&mut config.screen_reader, "Screen Reader Support");
    ui.checkbox(&mut config.keyboard_navigation, "Full Keyboard Navigation");
    ui.checkbox(&mut config.high_contrast, "High Contrast Mode");
    ui.checkbox(&mut config.reduced_motion, "Reduced Motion");
    ui.checkbox(&mut config.color_blind_safe, "Color Blind Safe Colors");
    ui.checkbox(&mut config.rtl_support, "Right-to-Left (RTL) Support");
    ui.checkbox(&mut config.announce_changes, "Announce Changes");
    ui.checkbox(&mut config.focus_trap, "Focus Trap in Modals");
    ui.checkbox(&mut config.skip_links, "Skip Links");
    
    ui.separator();
    
    ui.horizontal(|ui| {
        ui.label("Focus Indicator:");
        egui::ComboBox::from_id_source("focus_indicator")
            .selected_text(format!("{:?}", config.focus_indicator))
            .show_ui(ui, |ui| {
                for style in [FocusIndicatorStyle::Default, FocusIndicatorStyle::Visible, FocusIndicatorStyle::HighVisibility, FocusIndicatorStyle::None] {
                    ui.selectable_value(&mut config.focus_indicator, style, format!("{:?}", style));
                }
            });
    });
    
    ui.horizontal(|ui| {
        ui.label("Large Text Scale:");
        ui.add(egui::Slider::new(&mut config.large_text_scale, 1.0..=2.0).step_by(0.1).text("×"));
    });
    
    ui.horizontal(|ui| {
        ui.label("Min Touch Target:");
        ui.add(egui::Slider::new(&mut config.min_touch_target, 24.0..=64.0).suffix("px"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_accessibility_config_default() {
        let config = AccessibilityConfig::default();
        assert!(config.screen_reader);
        assert!(config.keyboard_navigation);
        assert_eq!(config.focus_indicator, FocusIndicatorStyle::Visible);
    }
    
    #[test]
    fn test_announce() {
        let manager = AccessibilityManager::new(AccessibilityConfig::default());
        manager.announce("Test message", AriaPriority::Polite);
        assert!(manager.live_region_content().contains("Test message"));
    }
    
    #[test]
    fn test_rtl_transform() {
        let rtl = RtlLayout::new(true);
        let rect = Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(200.0, 200.0));
        let transformed = rtl.transform_rect(rect, 800.0);
        assert_eq!(transformed.min.x, 600.0);
        assert_eq!(transformed.max.x, 700.0);
    }
    
    #[test]
    fn test_keyboard_navigator() {
        let mut nav = KeyboardNavigator::new(AccessibilityConfig::default());
        let id1 = egui::Id::new("btn1");
        let id2 = egui::Id::new("btn2");
        
        nav.register(id1);
        nav.register(id2);
        
        assert_eq!(nav.focus_order.len(), 2);
    }
}