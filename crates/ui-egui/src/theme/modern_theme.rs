//! Modern Theme System - Customizable, accessible, high-DPI aware
//!
//! New Feature: Complete theme overhaul with modern design, accessibility, and customization

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use egui::{Color32, FontId, FontFamily, Style, Visuals, Widgets, CornerRadius, Shadow, Stroke, TextStyle};
use serde::{Deserialize, Serialize};

/// Theme configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Base theme variant
    pub variant: ThemeVariant,
    
    /// Accent color (used for highlights, selections, active states)
    pub accent_color: AccentColor,
    
    /// Custom accent color (when variant is Custom)
    pub custom_accent: Option<Color32>,
    
    /// UI scale factor (1.0 = 100%, supports high-DPI)
    pub ui_scale: f32,
    
    /// Font scaling (independent of UI scale)
    pub font_scale: f32,
    
    /// Corner radius for panels/buttons
    pub corner_radius: f32,
    
    /// Animation speed multiplier
    pub animation_speed: f32,
    
    /// Reduced motion (accessibility)
    pub reduced_motion: bool,
    
    /// High contrast mode (accessibility)
    pub high_contrast: bool,
    
    /// Color blind safe palette
    pub color_blind_safe: bool,
    
    /// Compact mode (denser UI)
    pub compact_mode: bool,
    
    /// Show tooltips
    pub show_tooltips: bool,
    
    /// Tooltip delay (ms)
    pub tooltip_delay_ms: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ThemeVariant {
    Dark,
    Light,
    Auto,      // Follow system
    Custom,    // User-defined
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AccentColor {
    Purple,    // FilmCraft default
    Blue,
    Green,
    Orange,
    Red,
    Teal,
    Pink,
    Custom,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            variant: ThemeVariant::Auto,
            accent_color: AccentColor::Purple,
            custom_accent: None,
            ui_scale: 1.0,
            font_scale: 1.0,
            corner_radius: 6.0,
            animation_speed: 1.0,
            reduced_motion: false,
            high_contrast: false,
            color_blind_safe: false,
            compact_mode: false,
            show_tooltips: true,
            tooltip_delay_ms: 500,
        }
    }
}

/// Complete theme with all colors and styles
#[derive(Debug, Clone)]
pub struct Theme {
    pub config: ThemeConfig,
    pub colors: ThemeColors,
    pub spacing: ThemeSpacing,
    pub typography: ThemeTypography,
    pub shadows: ThemeShadows,
    pub animations: ThemeAnimations,
}

/// Color palette for a theme
#[derive(Debug, Clone)]
pub struct ThemeColors {
    // Backgrounds
    pub bg_primary: Color32,      // Main background
    pub bg_secondary: Color32,    // Panel backgrounds
    pub bg_tertiary: Color32,     // Elevated surfaces
    pub bg_hover: Color32,        // Hover state
    pub bg_active: Color32,       // Active/pressed state
    pub bg_selected: Color32,     // Selected items
    pub bg_disabled: Color32,     // Disabled state
    
    // Text
    pub text_primary: Color32,    // Main text
    pub text_secondary: Color32,  // Secondary text
    pub text_muted: Color32,      // Disabled/muted text
    pub text_inverse: Color32,    // Text on accent backgrounds
    pub text_link: Color32,       // Links
    
    // Borders
    pub border_subtle: Color32,   // Subtle borders
    pub border_default: Color32,  // Default borders
    pub border_strong: Color32,   // Strong borders
    pub border_focus: Color32,    // Focus rings
    
    // Accent
    pub accent: Color32,          // Primary accent
    pub accent_hover: Color32,    // Accent hover
    pub accent_active: Color32,   // Accent pressed
    pub accent_muted: Color32,    // Subtle accent
    pub accent_on: Color32,       // Text on accent
    
    // Semantic
    pub success: Color32,
    pub success_bg: Color32,
    pub warning: Color32,
    pub warning_bg: Color32,
    pub error: Color32,
    pub error_bg: Color32,
    pub info: Color32,
    pub info_bg: Color32,
    
    // Timeline specific
    pub timeline_bg: Color32,
    pub timeline_grid: Color32,
    pub timeline_playhead: Color32,
    pub timeline_selection: Color32,
    pub timeline_marker: Color32,
    
    // Audio
    pub audio_waveform: Color32,
    pub audio_meter_low: Color32,
    pub audio_meter_mid: Color32,
    pub audio_meter_high: Color32,
    pub audio_meter_peak: Color32,
    
    // Video scopes
    pub scope_waveform: Color32,
    pub scope_vectorscope: Color32,
    pub scope_parade_r: Color32,
    pub scope_parade_g: Color32,
    pub scope_parade_b: Color32,
}

/// Spacing system
#[derive(Debug, Clone)]
pub struct ThemeSpacing {
    pub xs: f32,      // 4px
    pub sm: f32,      // 8px
    pub md: f32,      // 16px
    pub lg: f32,      // 24px
    pub xl: f32,      // 32px
    pub xxl: f32,     // 48px
    
    pub item_spacing: f32,
    pub window_margin: f32,
    pub panel_padding: f32,
    pub button_padding: (f32, f32),  // (horizontal, vertical)
    pub input_padding: (f32, f32),
}

/// Typography system
#[derive(Debug, Clone)]
pub struct ThemeTypography {
    pub font_family: FontFamily,
    pub font_mono_family: FontFamily,
    
    pub size_xs: f32,   // 11px
    pub size_sm: f32,   // 12px
    pub size_md: f32,   // 14px
    pub size_lg: f32,   // 16px
    pub size_xl: f32,   // 18px
    pub size_xxl: f32,  // 24px
    pub size_xxxl: f32, // 32px
    
    pub weight_normal: u16,
    pub weight_medium: u16,
    pub weight_semibold: u16,
    pub weight_bold: u16,
    
    pub line_height_tight: f32,
    pub line_height_normal: f32,
    pub line_height_relaxed: f32,
}

/// Shadow system
#[derive(Debug, Clone)]
pub struct ThemeShadows {
    pub none: Shadow,
    pub xs: Shadow,
    pub sm: Shadow,
    pub md: Shadow,
    pub lg: Shadow,
    pub xl: Shadow,
}

/// Animation system
#[derive(Debug, Clone)]
pub struct ThemeAnimations {
    pub duration_fast: f32,     // 100ms
    pub duration_normal: f32,   // 200ms
    pub duration_slow: f32,     // 300ms
    pub easing_standard: egui::emath::EasingFunction,
    pub easing_emphasized: egui::emath::EasingFunction,
    pub easing_decelerate: egui::emath::EasingFunction,
}

impl Theme {
    /// Create theme from config
    pub fn from_config(config: ThemeConfig) -> Self {
        let variant = if config.variant == ThemeVariant::Auto {
            // Would detect system theme here
            ThemeVariant::Dark
        } else {
            config.variant
        };
        
        let accent = config.custom_accent.unwrap_or_else(|| 
            accent_color(config.accent_color, variant)
        );
        
        let colors = if config.high_contrast {
            high_contrast_colors(variant, accent)
        } else if config.color_blind_safe {
            color_blind_safe_colors(variant, accent)
        } else {
            standard_colors(variant, accent)
        };
        
        let spacing = if config.compact_mode {
            compact_spacing()
        } else {
            standard_spacing()
        };
        
        Self {
            config: config.clone(),
            colors,
            spacing,
            typography: standard_typography(config.font_scale),
            shadows: standard_shadows(variant),
            animations: standard_animations(config.animation_speed, config.reduced_motion),
        }
    }
    
    /// Apply theme to egui Style
    pub fn apply_to_style(&self, style: &mut Style) {
        let visuals = &mut style.visuals;
        
        // Colors
        visuals.override_text_color = Some(self.colors.text_primary);
        visuals.window_fill = self.colors.bg_primary;
        visuals.panel_fill = self.colors.bg_secondary;
        visuals.faint_bg_color = self.colors.bg_tertiary;
        visuals.extreme_bg_color = self.colors.bg_primary;
        visuals.code_bg_color = self.colors.bg_tertiary;
        visuals.warn_fg_color = self.colors.warning;
        visuals.error_fg_color = self.colors.error;
        visuals.hyperlink_color = self.colors.text_link;
        
        // Selection
        visuals.selection.bg_fill = self.colors.bg_selected;
        visuals.selection.stroke = Stroke::new(1.0, self.colors.accent);
        
        // Widgets
        visuals.widgets.noninteractive.bg_fill = self.colors.bg_secondary;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.colors.text_primary);
        
        visuals.widgets.inactive.bg_fill = self.colors.bg_secondary;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.colors.text_primary);
        visuals.widgets.inactive.weak_bg_fill = self.colors.bg_tertiary;
        visuals.widgets.inactive.expansion = 0.0;
        visuals.widgets.inactive.rounding = self.rounding();
        
        visuals.widgets.hovered.bg_fill = self.colors.bg_hover;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, self.colors.text_primary);
        visuals.widgets.hovered.weak_bg_fill = self.colors.accent_muted;
        visuals.widgets.hovered.expansion = 1.0;
        visuals.widgets.hovered.rounding = self.rounding();
        
        visuals.widgets.active.bg_fill = self.colors.bg_active;
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, self.colors.accent);
        visuals.widgets.active.weak_bg_fill = self.colors.accent;
        visuals.widgets.active.expansion = 1.0;
        visuals.widgets.active.rounding = self.rounding();
        
        visuals.widgets.open.bg_fill = self.colors.bg_tertiary;
        visuals.widgets.open.fg_stroke = Stroke::new(1.0, self.colors.text_primary);
        visuals.widgets.open.rounding = self.rounding();
        
        // Window
        visuals.window_shadow = self.shadows.md;
        visuals.window_rounding = CornerRadius::same(self.config.corner_radius + 2.0);
        visuals.window_stroke = Stroke::new(1.0, self.colors.border_default);
        
        // Menu
        visuals.menu_shadow = self.shadows.lg;
        visuals.menu_rounding = self.rounding();
        
        // Tooltips
        visuals.tooltip_shadow = self.shadows.md;
        visuals.tooltip_rounding = self.rounding();
        
        // Sliders
        visuals.slider_trailing_fill = true;
        
        // Spacing
        style.spacing.item_spacing = egui::vec2(self.spacing.item_spacing, self.spacing.item_spacing);
        style.spacing.window_margin = egui::Margin::same(self.spacing.window_margin);
        style.spacing.button_padding = egui::vec2(self.spacing.button_padding.0, self.spacing.button_padding.1);
        style.spacing.indent = self.spacing.md;
        style.spacing.combo_width = 200.0;
        style.spacing.text_edit_width = 280.0;
        style.spacing.slider_width = 200.0;
        style.spacing.interact_size = egui::vec2(40.0, 28.0);
        
        // Text styles
        self.apply_text_styles(style);
    }
    
    fn rounding(&self) -> CornerRadius {
        CornerRadius::same(self.config.corner_radius)
    }
    
    fn apply_text_styles(&self, style: &mut Style) {
        let t = &self.typography;
        let scale = self.config.ui_scale * self.config.font_scale;
        
        style.text_styles = [
            (TextStyle::Heading, FontId::new(t.size_xxxl * scale, t.font_family.clone())),
            (TextStyle::Body, FontId::new(t.size_md * scale, t.font_family.clone())),
            (TextStyle::Monospace, FontId::new(t.size_sm * scale, t.font_mono_family.clone())),
            (TextStyle::Button, FontId::new(t.size_sm * scale, t.font_family.clone())),
            (TextStyle::Small, FontId::new(t.size_xs * scale, t.font_family.clone())),
        ].into();
    }
}

/// Generate standard colors for variant
fn standard_colors(variant: ThemeVariant, accent: Color32) -> ThemeColors {
    let is_dark = variant == ThemeVariant::Dark;
    
    let (bg_primary, bg_secondary, bg_tertiary) = if is_dark {
        (Color32::from_rgb(28, 28, 30), Color32::from_rgb(38, 38, 42), Color32::from_rgb(48, 48, 54))
    } else {
        (Color32::from_rgb(250, 250, 252), Color32::from_rgb(243, 243, 245), Color32::from_rgb(255, 255, 255))
    };
    
    let (text_primary, text_secondary, text_muted) = if is_dark {
        (Color32::from_rgb(255, 255, 255), Color32::from_rgb(180, 180, 185), Color32::from_rgb(120, 120, 125))
    } else {
        (Color32::from_rgb(28, 28, 30), Color32::from_rgb(88, 88, 92), Color32::from_rgb(148, 148, 152))
    };
    
    let accent_hover = accent.linear_multiply(1.2);
    let accent_active = accent.linear_multiply(0.8);
    let accent_muted = accent.linear_multiply(0.15);
    let accent_on = if accent.perceived_luminance() > 0.5 { Color32::BLACK } else { Color32::WHITE };
    
    ThemeColors {
        bg_primary,
        bg_secondary,
        bg_tertiary,
        bg_hover: if is_dark { Color32::from_rgb(58, 58, 64) } else { Color32::from_rgb(230, 230, 235) },
        bg_active: if is_dark { Color32::from_rgb(68, 68, 74) } else { Color32::from_rgb(220, 220, 225) },
        bg_selected: accent_muted,
        bg_disabled: if is_dark { Color32::from_rgb(48, 48, 54) } else { Color32::from_rgb(235, 235, 238) },
        
        text_primary,
        text_secondary,
        text_muted,
        text_inverse: accent_on,
        text_link: accent,
        
        border_subtle: if is_dark { Color32::from_rgb(58, 58, 64) } else { Color32::from_rgb(220, 220, 225) },
        border_default: if is_dark { Color32::from_rgb(68, 68, 74) } else { Color32::from_rgb(200, 200, 205) },
        border_strong: if is_dark { Color32::from_rgb(88, 88, 94) } else { Color32::from_rgb(180, 180, 185) },
        border_focus: accent,
        
        accent,
        accent_hover,
        accent_active,
        accent_muted,
        accent_on,
        
        success: Color32::from_rgb(34, 197, 94),
        success_bg: Color32::from_rgba_premultiplied(34, 197, 94, 30),
        warning: Color32::from_rgb(251, 191, 36),
        warning_bg: Color32::from_rgba_premultiplied(251, 191, 36, 30),
        error: Color32::from_rgb(239, 68, 68),
        error_bg: Color32::from_rgba_premultiplied(239, 68, 68, 30),
        info: Color32::from_rgb(59, 130, 246),
        info_bg: Color32::from_rgba_premultiplied(59, 130, 246, 30),
        
        timeline_bg: bg_primary,
        timeline_grid: if is_dark { Color32::from_rgb(58, 58, 64) } else { Color32::from_rgb(220, 220, 225) },
        timeline_playhead: accent,
        timeline_selection: accent_muted,
        timeline_marker: Color32::from_rgb(251, 191, 36),
        
        audio_waveform: accent,
        audio_meter_low: Color32::from_rgb(34, 197, 94),
        audio_meter_mid: Color32::from_rgb(251, 191, 36),
        audio_meter_high: Color32::from_rgb(249, 115, 22),
        audio_meter_peak: Color32::from_rgb(239, 68, 68),
        
        scope_waveform: Color32::from_rgb(0, 255, 0),
        scope_vectorscope: Color32::from_rgb(0, 255, 255),
        scope_parade_r: Color32::from_rgb(255, 0, 0),
        scope_parade_g: Color32::from_rgb(0, 255, 0),
        scope_parade_b: Color32::from_rgb(0, 0, 255),
    }
}

/// High contrast colors for accessibility
fn high_contrast_colors(variant: ThemeVariant, accent: Color32) -> ThemeColors {
    let is_dark = variant == ThemeVariant::Dark;
    
    let (bg, text) = if is_dark {
        (Color32::BLACK, Color32::WHITE)
    } else {
        (Color32::WHITE, Color32::BLACK)
    };
    
    ThemeColors {
        bg_primary: bg,
        bg_secondary: bg,
        bg_tertiary: bg,
        bg_hover: if is_dark { Color32::from_rgb(32, 32, 32) } else { Color32::from_rgb(224, 224, 224) },
        bg_active: if is_dark { Color32::from_rgb(48, 48, 48) } else { Color32::from_rgb(208, 208, 208) },
        bg_selected: accent,
        bg_disabled: if is_dark { Color32::from_rgb(32, 32, 32) } else { Color32::from_rgb(224, 224, 224) },
        
        text_primary: text,
        text_secondary: text,
        text_muted: if is_dark { Color32::from_rgb(192, 192, 192) } else { Color32::from_rgb(64, 64, 64) },
        text_inverse: if is_dark { Color32::BLACK } else { Color32::WHITE },
        text_link: accent,
        
        border_subtle: text,
        border_default: text,
        border_strong: text,
        border_focus: accent,
        
        accent,
        accent_hover: accent.linear_multiply(1.2),
        accent_active: accent.linear_multiply(0.8),
        accent_muted: accent,
        accent_on: if accent.perceived_luminance() > 0.5 { Color32::BLACK } else { Color32::WHITE },
        
        success: Color32::from_rgb(0, 255, 0),
        success_bg: Color32::from_rgba_premultiplied(0, 255, 0, 60),
        warning: Color32::from_rgb(255, 255, 0),
        warning_bg: Color32::from_rgba_premultiplied(255, 255, 0, 60),
        error: Color32::from_rgb(255, 0, 0),
        error_bg: Color32::from_rgba_premultiplied(255, 0, 0, 60),
        info: Color32::from_rgb(0, 255, 255),
        info_bg: Color32::from_rgba_premultiplied(0, 255, 255, 60),
        
        timeline_bg: bg,
        timeline_grid: text,
        timeline_playhead: accent,
        timeline_selection: accent,
        timeline_marker: Color32::from_rgb(255, 255, 0),
        
        audio_waveform: accent,
        audio_meter_low: Color32::from_rgb(0, 255, 0),
        audio_meter_mid: Color32::from_rgb(255, 255, 0),
        audio_meter_high: Color32::from_rgb(255, 128, 0),
        audio_meter_peak: Color32::from_rgb(255, 0, 0),
        
        scope_waveform: Color32::from_rgb(0, 255, 0),
        scope_vectorscope: Color32::from_rgb(0, 255, 255),
        scope_parade_r: Color32::from_rgb(255, 0, 0),
        scope_parade_g: Color32::from_rgb(0, 255, 0),
        scope_parade_b: Color32::from_rgb(0, 0, 255),
    }
}

/// Color blind safe palette
fn color_blind_safe_colors(variant: ThemeVariant, accent: Color32) -> ThemeColors {
    // Use color-blind safe palette (avoid red/green confusion)
    // Replace red/green with blue/orange
    let mut colors = standard_colors(variant, accent);
    
    // Adjust semantic colors for color blindness
    colors.success = Color32::from_rgb(0, 180, 216);      // Blue-green
    colors.success_bg = Color32::from_rgba_premultiplied(0, 180, 216, 30);
    colors.warning = Color32::from_rgb(255, 165, 0);       // Orange
    colors.warning_bg = Color32::from_rgba_premultiplied(255, 165, 0, 30);
    colors.error = Color32::from_rgb(230, 0, 126);         // Magenta-red
    colors.error_bg = Color32::from_rgba_premultiplied(230, 0, 126, 30);
    
    // Audio meters
    colors.audio_meter_low = colors.success;
    colors.audio_meter_mid = colors.warning;
    colors.audio_meter_high = Color32::from_rgb(255, 100, 0);
    colors.audio_meter_peak = colors.error;
    
    colors
}

/// Get accent color from enum
fn accent_color(accent: AccentColor, variant: ThemeVariant) -> Color32 {
    let is_dark = variant == ThemeVariant::Dark;
    
    match accent {
        AccentColor::Purple => Color32::from_rgb(168, 85, 247),   // FilmCraft purple
        AccentColor::Blue => Color32::from_rgb(59, 130, 246),
        AccentColor::Green => Color32::from_rgb(34, 197, 94),
        AccentColor::Orange => Color32::from_rgb(249, 115, 22),
        AccentColor::Red => Color32::from_rgb(239, 68, 68),
        AccentColor::Teal => Color32::from_rgb(20, 184, 166),
        AccentColor::Pink => Color32::from_rgb(236, 72, 153),
        AccentColor::Custom => if is_dark { Color32::from_rgb(168, 85, 247) } else { Color32::from_rgb(124, 58, 237) },
    }
}

fn standard_spacing() -> ThemeSpacing {
    ThemeSpacing {
        xs: 4.0, sm: 8.0, md: 16.0, lg: 24.0, xl: 32.0, xxl: 48.0,
        item_spacing: 8.0,
        window_margin: 8.0,
        panel_padding: 12.0,
        button_padding: (12.0, 6.0),
        input_padding: (10.0, 8.0),
    }
}

fn compact_spacing() -> ThemeSpacing {
    ThemeSpacing {
        xs: 2.0, sm: 4.0, md: 8.0, lg: 12.0, xl: 16.0, xxl: 24.0,
        item_spacing: 4.0,
        window_margin: 4.0,
        panel_padding: 8.0,
        button_padding: (8.0, 4.0),
        input_padding: (8.0, 6.0),
    }
}

fn standard_typography(font_scale: f32) -> ThemeTypography {
    ThemeTypography {
        font_family: FontFamily::Proportional,
        font_mono_family: FontFamily::Monospace,
        
        size_xs: 11.0 * font_scale,
        size_sm: 12.0 * font_scale,
        size_md: 14.0 * font_scale,
        size_lg: 16.0 * font_scale,
        size_xl: 18.0 * font_scale,
        size_xxl: 24.0 * font_scale,
        size_xxxl: 32.0 * font_scale,
        
        weight_normal: 400,
        weight_medium: 500,
        weight_semibold: 600,
        weight_bold: 700,
        
        line_height_tight: 1.2,
        line_height_normal: 1.5,
        line_height_relaxed: 1.75,
    }
}

fn standard_shadows(variant: ThemeVariant) -> ThemeShadows {
    let shadow_color = if variant == ThemeVariant::Dark {
        Color32::from_rgba_premultiplied(0, 0, 0, 80)
    } else {
        Color32::from_rgba_premultiplied(0, 0, 0, 40)
    };
    
    ThemeShadows {
        none: Shadow::NONE,
        xs: Shadow { extrusion: 1.0, color: shadow_color },
        sm: Shadow { extrusion: 4.0, color: shadow_color },
        md: Shadow { extrusion: 8.0, color: shadow_color },
        lg: Shadow { extrusion: 16.0, color: shadow_color },
        xl: Shadow { extrusion: 24.0, color: shadow_color },
    }
}

fn standard_animations(speed: f32, reduced: bool) -> ThemeAnimations {
    if reduced {
        ThemeAnimations {
            duration_fast: 0.0,
            duration_normal: 0.0,
            duration_slow: 0.0,
            easing_standard: egui::emath::EasingFunction::Linear,
            easing_emphasized: egui::emath::EasingFunction::Linear,
            easing_decelerate: egui::emath::EasingFunction::Linear,
        }
    } else {
        ThemeAnimations {
            duration_fast: 0.1 * speed,
            duration_normal: 0.2 * speed,
            duration_slow: 0.3 * speed,
            easing_standard: egui::emath::EasingFunction::CubicOut,
            easing_emphasized: egui::emath::EasingFunction::QuadraticOut,
            easing_decelerate: egui::emath::EasingFunction::CubicOut,
        }
    }
}

/// Theme manager for runtime theme switching
pub struct ThemeManager {
    current: RwLock<Theme>,
    configs: RwLock<HashMap<String, ThemeConfig>>,
    system_theme_listener: Option<std::thread::JoinHandle<()>>,
}

impl ThemeManager {
    pub fn new(default_config: ThemeConfig) -> Self {
        let theme = Theme::from_config(default_config.clone());
        let mut configs = HashMap::new();
        configs.insert("default".to_string(), default_config);
        
        Self {
            current: RwLock::new(theme),
            configs: RwLock::new(configs),
            system_theme_listener: None,
        }
    }
    
    pub fn current(&self) -> Theme {
        self.current.read().unwrap().clone()
    }
    
    pub fn set_config(&self, name: &str, config: ThemeConfig) {
        self.configs.write().unwrap().insert(name.to_string(), config.clone());
        *self.current.write().unwrap() = Theme::from_config(config);
    }
    
    pub fn get_config(&self, name: &str) -> Option<ThemeConfig> {
        self.configs.read().unwrap().get(name).cloned()
    }
    
    pub fn apply_to_context(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        self.current.read().unwrap().apply_to_style(&mut style);
        ctx.set_style(style);
    }
    
    /// Start listening for system theme changes
    pub fn start_system_listener(&mut self) {
        // Would integrate with platform APIs
    }
}

/// Theme UI for settings panel
pub fn show_theme_settings(ui: &mut egui::Ui, manager: &ThemeManager) {
    let current = manager.current();
    let mut config = current.config.clone();
    let changed = false;
    
    ui.heading("Appearance");
    ui.separator();
    
    // Theme variant
    ui.horizontal(|ui| {
        ui.label("Theme:");
        egui::ComboBox::from_id_source("theme_variant")
            .selected_text(format!("{:?}", config.variant))
            .show_ui(ui, |ui| {
                for v in [ThemeVariant::Dark, ThemeVariant::Light, ThemeVariant::Auto] {
                    ui.selectable_value(&mut config.variant, v, format!("{:?}", v));
                }
            });
    });
    
    // Accent color
    ui.horizontal(|ui| {
        ui.label("Accent Color:");
        egui::ComboBox::from_id_source("accent_color")
            .selected_text(format!("{:?}", config.accent_color))
            .show_ui(ui, |ui| {
                for a in [
                    AccentColor::Purple, AccentColor::Blue, AccentColor::Green,
                    AccentColor::Orange, AccentColor::Red, AccentColor::Teal,
                    AccentColor::Pink, AccentColor::Custom
                ] {
                    ui.selectable_value(&mut config.accent_color, a, format!("{:?}", a));
                }
            });
    });
    
    if config.accent_color == AccentColor::Custom {
        ui.horizontal(|ui| {
            ui.label("Custom:");
            egui::color_picker::color_edit_button_srgb(ui, config.custom_accent.get_or_insert(Color32::from_rgb(168, 85, 247)));
        });
    }
    
    ui.separator();
    
    // UI Scale
    ui.horizontal(|ui| {
        ui.label("UI Scale:");
        ui.add(egui::Slider::new(&mut config.ui_scale, 0.75..=2.0).text("×").step_by(0.05));
        if ui.button("Reset").clicked() {
            config.ui_scale = 1.0;
        }
    });
    
    // Font Scale
    ui.horizontal(|ui| {
        ui.label("Font Scale:");
        ui.add(egui::Slider::new(&mut config.font_scale, 0.75..=2.0).text("×").step_by(0.05));
    });
    
    // Corner radius
    ui.horizontal(|ui| {
        ui.label("Corner Radius:");
        ui.add(egui::Slider::new(&mut config.corner_radius, 0.0..=16.0).suffix("px"));
    });
    
    ui.separator();
    
    // Accessibility
    ui.collapsing("Accessibility", |ui| {
        ui.checkbox(&mut config.reduced_motion, "Reduced Motion");
        ui.checkbox(&mut config.high_contrast, "High Contrast");
        ui.checkbox(&mut config.color_blind_safe, "Color Blind Safe");
        ui.checkbox(&mut config.compact_mode, "Compact Mode");
    });
    
    // Apply button
    if ui.button("Apply Theme").clicked() {
        manager.set_config("default", config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_theme_creation() {
        let config = ThemeConfig::default();
        let theme = Theme::from_config(config);
        assert_eq!(theme.config.variant, ThemeVariant::Auto);
    }
    
    #[test]
    fn test_accent_colors() {
        let purple = accent_color(AccentColor::Purple, ThemeVariant::Dark);
        assert_eq!(purple, Color32::from_rgb(168, 85, 247));
    }
    
    #[test]
    fn test_high_contrast() {
        let config = ThemeConfig { high_contrast: true, ..Default::default() };
        let theme = Theme::from_config(config);
        assert_eq!(theme.colors.bg_primary, Color32::BLACK);
        assert_eq!(theme.colors.text_primary, Color32::WHITE);
    }
}