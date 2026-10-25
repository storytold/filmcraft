//! Design tokens. Every widget reads colours/sizes from [`Tokens`] so themes apply everywhere.
//!
//! The default theme follows the look of Premiere Pro's current dark UI (values measured from
//! black-box screenshots, see `plan/premiere/02-ui-ux.md`); fonts are Inter + JetBrains Mono (OFL).
//! The Evolve theme ([`ThemeKind::Evolve`]) is an original palette in the spirit of page-based
//! grading suites' dark interfaces: cool charcoal panels, a warm orange highlight, near-white timecodes
//! (`docs/appearance.md`). No values, images or other assets of any vendor are used.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// Premiere-style darkest (default).
    #[default]
    Dark,
    /// Slightly lighter grey panels (Premiere's brightness slider mid position).
    Medium,
    Light,
    /// Cool charcoal panels with a warm orange highlight, in the spirit of page-based grading
    /// suites' dark interfaces (an original palette, see the module docs).
    Evolve,
}

impl ThemeKind {
    /// Every theme, in the order Settings ▸ Appearance lists them.
    pub const ALL: [ThemeKind; 4] = [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Evolve, ThemeKind::Light];

    /// Settings ▸ Appearance ▸ Color Theme value (`darkest`, `dark`, `evolve`, `light`).
    pub fn pref_name(self) -> &'static str {
        match self {
            ThemeKind::Dark => "darkest",
            ThemeKind::Medium => "dark",
            ThemeKind::Light => "light",
            ThemeKind::Evolve => "evolve",
        }
    }
    pub fn from_pref(s: &str) -> ThemeKind {
        match s {
            "dark" => ThemeKind::Medium,
            "light" => ThemeKind::Light,
            "evolve" => ThemeKind::Evolve,
            _ => ThemeKind::Dark,
        }
    }
    /// `ui.set {theme}` and `view.theme.<name>`: `dark`, `medium`, `light`, `evolve` (also the
    /// preference values).
    pub fn from_name(s: &str) -> Option<ThemeKind> {
        match s.to_ascii_lowercase().as_str() {
            "dark" | "darkest" => Some(ThemeKind::Dark),
            "medium" | "grey" | "gray" => Some(ThemeKind::Medium),
            "light" => Some(ThemeKind::Light),
            "evolve" => Some(ThemeKind::Evolve),
            _ => None,
        }
    }
    /// Light themes use egui's light visuals and dark text; every other theme is a dark one.
    pub fn is_light(self) -> bool {
        self == ThemeKind::Light
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Space between panels / app background.
    pub app_bg: Color32,
    /// Header bar (top).
    pub header_bg: Color32,
    /// Panel body.
    pub panel_bg: Color32,
    /// Panel tab strip.
    pub tab_bg: Color32,
    pub tab_text: Color32,
    pub tab_text_active: Color32,
    /// Blue focus rectangle around the active panel.
    pub focus: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub icon_active: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub field_bg: Color32,
    pub field_border: Color32,
    pub separator: Color32,
    pub row_alt: Color32,
    pub row_selected: Color32,
    /// Scrubby (hot-text) numeric value colour.
    pub hot_text: Color32,
    /// Selected pills and compact toggle backgrounds.
    pub pill_active_bg: Color32,
    /// The selected clip bar in Effect Controls.
    pub clip_bar_bg: Color32,
    /// Raised audio control surfaces, such as mixer channel strips.
    pub control_surface: Color32,
    /// Recessed audio control tracks and panner wells.
    pub control_well: Color32,
    pub control_border: Color32,
    pub control_handle_bg: Color32,
    pub control_handle: Color32,
    pub control_handle_dim: Color32,
    /// Data-plot surfaces (keyframe, EQ and dynamics graphs), distinct from media canvases.
    pub plot_bg: Color32,
    pub plot_grid: Color32,
    pub plot_axis: Color32,
    pub plot_handle: Color32,
    pub plot_handle_dim: Color32,
    /// Meter wells and the separate loudness readout surface.
    pub meter_bg: Color32,
    pub meter_readout_bg: Color32,
    pub meter_clip_off: Color32,
    pub panner_bg: Color32,
    pub panner_border: Color32,
    pub panner_speaker: Color32,
    pub fader_knob: Color32,
    pub eq_track: Color32,
    pub eq_tick: Color32,
    pub eq_knob: Color32,
    pub eq_knob_active: Color32,
    pub eq_node: Color32,
    pub eq_node_off: Color32,
    pub crossover_handle: Color32,
    pub keyframe_plot_bg: Color32,
    pub keyframe_plot_grid: Color32,
    pub keyframe_plot_axis: Color32,
    pub keyframe_handle: Color32,
    pub switch_fill: Color32,
    pub switch_knob: Color32,
    pub switch_border: Color32,
    pub slider_disabled_track: Color32,
    pub slider_disabled_knob: Color32,
    pub button_hover: Color32,
    pub button_border: Color32,
    pub caption_track_bg: Color32,
    pub scroll_thumb: Color32,
    pub scroll_thumb_hover: Color32,
    pub vertical_scroll_thumb: Color32,
    pub disabled_clip_bg: Color32,
    // timeline
    pub tl_bg: Color32,
    pub tl_track_bg: Color32,
    pub tl_track_bg_alt: Color32,
    pub tl_header_bg: Color32,
    pub tl_ruler_bg: Color32,
    pub tl_ruler_tick: Color32,
    pub tl_ruler_text: Color32,
    pub playhead: Color32,
    /// Fill of a track header's source-patch / track-targeting button while it is on.
    pub track_target: Color32,
    pub in_out_shade: Color32,
    pub clip_selected_border: Color32,
    pub render_red: Color32,
    pub render_yellow: Color32,
    pub render_green: Color32,
    pub monitor_bg: Color32,
    pub timecode: Color32,
    pub danger: Color32,
    pub radius: f32,
    pub radius_sm: f32,
    pub gap: f32,
    pub tab_h: f32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        // Measured from Premiere 26 "Darkest" (plan/premiere/02-ui-ux.md §1).
        let dark = Tokens {
            kind,
            app_bg: Color32::from_rgb(0, 0, 0),
            header_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            panel_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tab_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tab_text: Color32::from_rgb(0xb0, 0xb0, 0xb0),
            tab_text_active: Color32::from_rgb(0xd1, 0xd1, 0xd1),
            focus: Color32::from_rgb(0x57, 0x94, 0xec),
            accent: Color32::from_rgb(0x2f, 0x6b, 0xdf),
            accent_hover: Color32::from_rgb(0x3f, 0x7c, 0xe8),
            text: Color32::from_rgb(0xd1, 0xd1, 0xd1),
            text_dim: Color32::from_rgb(0xb0, 0xb0, 0xb0),
            text_faint: Color32::from_rgb(0x6e, 0x6e, 0x6e),
            icon: Color32::from_rgb(0xb0, 0xb0, 0xb0),
            icon_active: Color32::from_rgb(0xd1, 0xd1, 0xd1),
            hover: Color32::from_rgb(0x2c, 0x2c, 0x2c),
            pressed: Color32::from_rgb(0x4b, 0x4b, 0x4b),
            field_bg: Color32::from_rgb(0x0e, 0x0e, 0x0e),
            field_border: Color32::from_rgb(0x30, 0x30, 0x30),
            separator: Color32::from_rgb(0x30, 0x30, 0x30),
            row_alt: Color32::from_rgb(0x21, 0x21, 0x21),
            row_selected: Color32::from_rgb(0x33, 0x33, 0x33),
            hot_text: Color32::from_rgb(0x40, 0x96, 0xf3),
            pill_active_bg: Color32::from_rgb(0x3a, 0x3a, 0x3a),
            clip_bar_bg: Color32::from_rgb(58, 58, 70),
            control_surface: Color32::from_rgb(0x24, 0x24, 0x24),
            control_well: Color32::from_rgb(0x10, 0x10, 0x10),
            control_border: Color32::from_rgb(0x50, 0x50, 0x50),
            control_handle_bg: Color32::from_rgb(0x2a, 0x2a, 0x2a),
            control_handle: Color32::from_rgb(0xe0, 0xe0, 0xe0),
            control_handle_dim: Color32::from_rgb(0xb0, 0xb0, 0xb0),
            plot_bg: Color32::from_rgb(0x18, 0x18, 0x18),
            plot_grid: Color32::from_rgb(0x2c, 0x2c, 0x2c),
            plot_axis: Color32::from_rgb(0x44, 0x44, 0x44),
            plot_handle: Color32::from_rgb(0xe0, 0xe0, 0xe0),
            plot_handle_dim: Color32::from_rgb(0x90, 0x90, 0x90),
            meter_bg: Color32::BLACK,
            meter_readout_bg: Color32::from_rgb(0x14, 0x14, 0x14),
            meter_clip_off: Color32::from_rgb(0x30, 0x30, 0x30),
            panner_bg: Color32::from_gray(22),
            panner_border: Color32::from_gray(96),
            panner_speaker: Color32::from_gray(144),
            fader_knob: Color32::from_gray(90),
            eq_track: Color32::from_gray(48),
            eq_tick: Color32::from_gray(96),
            eq_knob: Color32::from_gray(192),
            eq_knob_active: Color32::from_gray(240),
            eq_node: Color32::from_gray(232),
            eq_node_off: Color32::from_gray(112),
            crossover_handle: Color32::from_gray(255),
            keyframe_plot_bg: Color32::from_gray(25),
            keyframe_plot_grid: Color32::from_gray(42),
            keyframe_plot_axis: Color32::from_gray(51),
            keyframe_handle: Color32::from_gray(208),
            switch_fill: Color32::from_gray(212),
            switch_knob: Color32::from_gray(29),
            switch_border: Color32::from_gray(138),
            slider_disabled_track: Color32::from_gray(74),
            slider_disabled_knob: Color32::from_gray(106),
            button_hover: Color32::from_gray(42),
            button_border: Color32::from_gray(75),
            caption_track_bg: Color32::from_rgb(0x23, 0x20, 0x2a),
            scroll_thumb: Color32::from_gray(0x4b),
            scroll_thumb_hover: Color32::from_gray(0x6a),
            vertical_scroll_thumb: Color32::from_gray(80),
            disabled_clip_bg: Color32::from_gray(0x2a),
            tl_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tl_track_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tl_track_bg_alt: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tl_header_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tl_ruler_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            tl_ruler_tick: Color32::from_rgb(0x8d, 0x8d, 0x8d),
            tl_ruler_text: Color32::from_rgb(0xb0, 0xb0, 0xb0),
            playhead: Color32::from_rgb(0x58, 0x95, 0xec),
            track_target: Color32::from_rgb(0x26, 0x5b, 0xc1),
            in_out_shade: Color32::from_rgb(0x3f, 0x3f, 0x3f),
            clip_selected_border: Color32::from_rgb(0xeb, 0xeb, 0xeb),
            render_red: Color32::from_rgb(0xe3, 0x48, 0x50),
            render_yellow: Color32::from_rgb(0xf0, 0xf0, 0x4f),
            render_green: Color32::from_rgb(0x2d, 0x9d, 0x78),
            monitor_bg: Color32::from_rgb(0x1d, 0x1d, 0x1d),
            timecode: Color32::from_rgb(0x40, 0x96, 0xf3),
            danger: Color32::from_rgb(0xdc, 0x51, 0x3d),
            radius: 0.0,
            radius_sm: 4.0,
            gap: 4.0,
            tab_h: 32.0,
        };
        match kind {
            ThemeKind::Dark => dark,
            ThemeKind::Medium => Tokens {
                app_bg: Color32::from_rgb(20, 20, 20),
                header_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                panel_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                tab_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                field_bg: Color32::from_rgb(0x22, 0x22, 0x22),
                tl_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                tl_track_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                tl_track_bg_alt: Color32::from_rgb(0x32, 0x32, 0x32),
                tl_header_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                tl_ruler_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                monitor_bg: Color32::from_rgb(0x32, 0x32, 0x32),
                row_alt: Color32::from_rgb(50, 50, 50),
                hover: Color32::from_rgb(64, 64, 64),
                ..dark
            },
            ThemeKind::Evolve => evolve(dark),
            ThemeKind::Light => Tokens {
                app_bg: Color32::from_rgb(180, 180, 180),
                header_bg: Color32::from_rgb(214, 214, 214),
                panel_bg: Color32::from_rgb(232, 232, 232),
                tab_bg: Color32::from_rgb(232, 232, 232),
                tab_text: Color32::from_rgb(90, 90, 90),
                tab_text_active: Color32::from_rgb(20, 20, 20),
                text: Color32::from_rgb(34, 34, 34),
                text_dim: Color32::from_rgb(90, 90, 90),
                text_faint: Color32::from_rgb(140, 140, 140),
                icon: Color32::from_rgb(60, 60, 60),
                hover: Color32::from_rgb(210, 210, 210),
                pressed: Color32::from_rgb(196, 196, 196),
                field_bg: Color32::from_rgb(250, 250, 250),
                field_border: Color32::from_rgb(170, 170, 170),
                separator: Color32::from_rgb(200, 200, 200),
                row_alt: Color32::from_rgb(224, 224, 224),
                row_selected: Color32::from_rgb(170, 200, 240),
                pill_active_bg: Color32::from_rgb(196, 196, 196),
                clip_bar_bg: Color32::from_rgb(190, 203, 224),
                control_surface: Color32::from_rgb(218, 218, 218),
                control_well: Color32::from_rgb(196, 196, 196),
                control_border: Color32::from_rgb(150, 150, 150),
                control_handle_bg: Color32::from_rgb(205, 205, 205),
                control_handle: Color32::from_rgb(50, 50, 50),
                control_handle_dim: Color32::from_rgb(85, 85, 85),
                plot_bg: Color32::from_rgb(245, 245, 245),
                plot_grid: Color32::from_rgb(210, 210, 210),
                plot_axis: Color32::from_rgb(170, 170, 170),
                plot_handle: Color32::from_rgb(50, 50, 50),
                plot_handle_dim: Color32::from_rgb(100, 100, 100),
                meter_bg: Color32::from_rgb(205, 205, 205),
                meter_readout_bg: Color32::from_rgb(218, 218, 218),
                meter_clip_off: Color32::from_rgb(155, 155, 155),
                panner_bg: Color32::from_gray(196),
                panner_border: Color32::from_gray(150),
                panner_speaker: Color32::from_gray(100),
                fader_knob: Color32::from_gray(140),
                eq_track: Color32::from_gray(196),
                eq_tick: Color32::from_gray(150),
                eq_knob: Color32::from_gray(85),
                eq_knob_active: Color32::from_gray(34),
                eq_node: Color32::from_gray(50),
                eq_node_off: Color32::from_gray(140),
                crossover_handle: Color32::from_gray(50),
                keyframe_plot_bg: Color32::from_gray(245),
                keyframe_plot_grid: Color32::from_gray(210),
                keyframe_plot_axis: Color32::from_gray(170),
                keyframe_handle: Color32::from_gray(50),
                switch_fill: Color32::from_gray(60),
                switch_knob: Color32::from_gray(232),
                switch_border: Color32::from_gray(110),
                slider_disabled_track: Color32::from_gray(190),
                slider_disabled_knob: Color32::from_gray(140),
                button_hover: Color32::from_gray(210),
                button_border: Color32::from_gray(170),
                icon_active: Color32::from_rgb(34, 34, 34),
                caption_track_bg: Color32::from_rgb(222, 215, 233),
                scroll_thumb: Color32::from_gray(150),
                scroll_thumb_hover: Color32::from_gray(130),
                vertical_scroll_thumb: Color32::from_gray(150),
                disabled_clip_bg: Color32::from_gray(190),
                in_out_shade: Color32::from_gray(185),
                tl_bg: Color32::from_rgb(210, 210, 210),
                tl_track_bg: Color32::from_rgb(222, 222, 222),
                tl_track_bg_alt: Color32::from_rgb(228, 228, 228),
                tl_header_bg: Color32::from_rgb(214, 214, 214),
                tl_ruler_bg: Color32::from_rgb(226, 226, 226),
                tl_ruler_text: Color32::from_rgb(80, 80, 80),
                ..dark
            },
        }
    }
}

/// The Evolve theme: an original palette in the spirit of page-based grading suites' dark
/// interfaces. Cool (slightly blue) charcoal panels on a near-black background, thin gutters, a warm orange
/// highlight for selections, the focused panel and primary buttons, a red playhead, near-white
/// timecodes and a darker viewer well. Chrome measurements (tab height, radii, gaps) are
/// FilmCraft's own. Secondary text reaches WCAG AA against every panel surface
/// (`tests::evolve_text_contrast_meets_wcag_aa`).
fn evolve(dark: Tokens) -> Tokens {
    let panel = Color32::from_rgb(0x28, 0x28, 0x2d);
    let chrome = Color32::from_rgb(0x1f, 0x1f, 0x24);
    // a fill under white labels (4.5:1); as text on the panels the orange is `hot_text`
    let accent = Color32::from_rgb(0xbe, 0x51, 0x1b);
    let text = Color32::from_rgb(0xe4, 0xe4, 0xea);
    let text_dim = Color32::from_rgb(0xb4, 0xb4, 0xbc);
    let text_faint = Color32::from_rgb(0x86, 0x86, 0x90);
    Tokens {
        kind: ThemeKind::Evolve,
        app_bg: Color32::from_rgb(0x12, 0x12, 0x16),
        header_bg: chrome,
        panel_bg: panel,
        tab_bg: chrome,
        tab_text: text_dim,
        tab_text_active: text,
        focus: Color32::from_rgb(0xe8, 0x7e, 0x46),
        accent,
        accent_hover: Color32::from_rgb(0xc4, 0x54, 0x1b),
        text,
        text_dim,
        text_faint,
        icon: text_dim,
        icon_active: text,
        hover: Color32::from_rgb(0x34, 0x34, 0x3a),
        pressed: Color32::from_rgb(0x46, 0x46, 0x4e),
        field_bg: Color32::from_rgb(0x1a, 0x1a, 0x1e),
        field_border: Color32::from_rgb(0x3c, 0x3c, 0x44),
        separator: Color32::from_rgb(0x38, 0x38, 0x40),
        row_alt: Color32::from_rgb(0x2c, 0x2c, 0x32),
        row_selected: Color32::from_rgb(0x44, 0x3a, 0x34),
        hot_text: Color32::from_rgb(0xf2, 0xa2, 0x6a),
        pill_active_bg: Color32::from_rgb(0x42, 0x42, 0x4a),
        clip_bar_bg: Color32::from_rgb(0x3e, 0x3a, 0x44),
        control_surface: Color32::from_rgb(0x2f, 0x2f, 0x35),
        control_well: Color32::from_rgb(0x17, 0x17, 0x1b),
        control_border: Color32::from_rgb(0x56, 0x56, 0x5e),
        control_handle_bg: Color32::from_rgb(0x36, 0x36, 0x3c),
        control_handle: Color32::from_rgb(0xe8, 0xe8, 0xee),
        control_handle_dim: text_dim,
        plot_bg: Color32::from_rgb(0x1c, 0x1c, 0x21),
        plot_grid: Color32::from_rgb(0x30, 0x30, 0x37),
        plot_axis: Color32::from_rgb(0x4a, 0x4a, 0x52),
        plot_handle: Color32::from_rgb(0xe8, 0xe8, 0xee),
        plot_handle_dim: Color32::from_rgb(0x9a, 0x9a, 0xa4),
        meter_bg: Color32::from_rgb(0x0f, 0x0f, 0x12),
        meter_readout_bg: Color32::from_rgb(0x1a, 0x1a, 0x1e),
        meter_clip_off: Color32::from_rgb(0x3a, 0x3a, 0x42),
        panner_bg: Color32::from_rgb(0x1c, 0x1c, 0x21),
        panner_border: Color32::from_rgb(0x66, 0x66, 0x70),
        panner_speaker: Color32::from_rgb(0x9a, 0x9a, 0xa4),
        fader_knob: Color32::from_rgb(0x6a, 0x6a, 0x74),
        eq_track: Color32::from_rgb(0x36, 0x36, 0x3c),
        eq_tick: Color32::from_rgb(0x66, 0x66, 0x70),
        eq_knob: Color32::from_rgb(0xc4, 0xc4, 0xcc),
        eq_knob_active: Color32::from_rgb(0xf2, 0xf2, 0xf6),
        eq_node: Color32::from_rgb(0xec, 0xec, 0xf0),
        eq_node_off: Color32::from_rgb(0x7a, 0x7a, 0x84),
        crossover_handle: Color32::WHITE,
        keyframe_plot_bg: Color32::from_rgb(0x1c, 0x1c, 0x21),
        keyframe_plot_grid: Color32::from_rgb(0x2e, 0x2e, 0x35),
        keyframe_plot_axis: Color32::from_rgb(0x3a, 0x3a, 0x42),
        keyframe_handle: Color32::from_rgb(0xd6, 0xd6, 0xde),
        switch_fill: Color32::from_rgb(0xd8, 0xd8, 0xe0),
        switch_knob: Color32::from_rgb(0x24, 0x24, 0x29),
        switch_border: Color32::from_rgb(0x8c, 0x8c, 0x96),
        slider_disabled_track: Color32::from_rgb(0x4c, 0x4c, 0x54),
        slider_disabled_knob: Color32::from_rgb(0x6c, 0x6c, 0x76),
        button_hover: Color32::from_rgb(0x34, 0x34, 0x3a),
        button_border: Color32::from_rgb(0x50, 0x50, 0x58),
        caption_track_bg: Color32::from_rgb(0x2a, 0x27, 0x33),
        scroll_thumb: Color32::from_rgb(0x50, 0x50, 0x58),
        scroll_thumb_hover: Color32::from_rgb(0x6e, 0x6e, 0x78),
        vertical_scroll_thumb: Color32::from_rgb(0x54, 0x54, 0x5c),
        disabled_clip_bg: Color32::from_rgb(0x33, 0x33, 0x39),
        tl_bg: Color32::from_rgb(0x20, 0x20, 0x25),
        tl_track_bg: Color32::from_rgb(0x2a, 0x2a, 0x30),
        tl_track_bg_alt: Color32::from_rgb(0x26, 0x26, 0x2c),
        tl_header_bg: Color32::from_rgb(0x24, 0x24, 0x29),
        tl_ruler_bg: chrome,
        tl_ruler_tick: Color32::from_rgb(0x8e, 0x8e, 0x98),
        tl_ruler_text: text_dim,
        playhead: Color32::from_rgb(0xe4, 0x3f, 0x2e),
        track_target: Color32::from_rgb(0xb8, 0x55, 0x24),
        in_out_shade: Color32::from_rgb(0x3f, 0x3f, 0x47),
        clip_selected_border: Color32::from_rgb(0xf4, 0xf4, 0xf8),
        monitor_bg: Color32::from_rgb(0x18, 0x18, 0x1c),
        timecode: Color32::from_rgb(0xf0, 0xf0, 0xf4),
        danger: Color32::from_rgb(0xe0, 0x4e, 0x3c),
        radius: 3.0,
        radius_sm: 3.0,
        gap: 3.0,
        tab_h: 30.0,
        ..dark
    }
}

impl Tokens {
    /// The highlight as text on the panels (links, "Active" and "User" labels, the shortcut being
    /// recorded). The Evolve orange
    /// is a fill dark enough for white labels, too dark for normal text on its charcoal panels, so
    /// there the text is the lighter `hot_text` orange; the other themes use the highlight itself.
    pub fn accent_text(&self) -> Color32 {
        if self.kind == ThemeKind::Evolve { self.hot_text } else { self.accent }
    }

    /// Settings ▸ Appearance on top of a theme: the highlight colour (selections, primary buttons,
    /// the focus outline, the playhead, timecodes, drag values and track targeting) and accessible
    /// colour contrast (brighter secondary text and borders).
    pub fn with_appearance(mut self, highlight: Option<[u8; 3]>, accessible_contrast: bool) -> Self {
        // The default highlight means "the theme's own highlight": the Evolve theme keeps its
        // orange until the user picks a colour of their own.
        let default = filmcraft_engine::settings::parse_hex(filmcraft_engine::settings::DEFAULT_HIGHLIGHT);
        if let Some([r, g, b]) = highlight.filter(|h| Some(*h) != default || self.kind != ThemeKind::Evolve) {
            let c = Color32::from_rgb(r, g, b);
            // A highlight of the user's own also colours the theme's other blues: the same hue, as
            // much more or less saturated and bright as each of them is than the theme's highlight.
            // The theme's own highlight leaves them exactly as designed.
            if c != self.accent {
                let tint = |s: f32, v: f32| {
                    let mut h = egui::ecolor::HsvaGamma::from(c);
                    h.s = (h.s * s).clamp(0.0, 1.0);
                    h.v = (h.v * v).clamp(0.0, 1.0);
                    Color32::from(h)
                };
                self.focus = tint(0.92, 1.06);
                self.playhead = self.focus;
                self.timecode = tint(0.98, 1.09);
                self.hot_text = self.timecode;
                self.track_target = tint(1.0, 0.865);
            }
            let lift = |v: u8| v.saturating_add(16);
            self.accent = c;
            self.accent_hover = Color32::from_rgb(lift(r), lift(g), lift(b));
        }
        if accessible_contrast {
            if self.kind.is_light() {
                self.text_dim = Color32::from_rgb(40, 40, 40);
                self.text_faint = Color32::from_rgb(80, 80, 80);
                self.field_border = Color32::from_rgb(110, 110, 110);
            } else {
                self.text_dim = self.text;
                self.text_faint = Color32::from_rgb(0xa8, 0xa8, 0xa8);
                self.tab_text = self.tab_text_active;
                self.field_border = Color32::from_rgb(0x6a, 0x6a, 0x6a);
                self.separator = Color32::from_rgb(0x50, 0x50, 0x50);
            }
        }
        self
    }

    /// Mono font for timecode.
    pub fn mono(size: f32) -> FontId {
        FontId::new(size, FontFamily::Monospace)
    }
    pub fn ui(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }
    pub fn semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name("semibold".into()))
    }
    /// The big blue timecode over the timeline and under the monitors: regular weight, as in
    /// Premiere (Premiere's reads 10 px tall and 76 px wide for 00:00:16:05 at 1x).
    pub fn timecode() -> FontId {
        FontId::new(16.0, FontFamily::Proportional)
    }
}

/// Menu text: Premiere's menus are the system's, larger and brighter than panel text, one item
/// every 24 px.
pub const MENU_TEXT_SIZE: f32 = 13.0;
pub const MENU_TEXT: Color32 = Color32::from_rgb(0xde, 0xde, 0xde);

/// Style of the menu bar's menus and their submenus (on top of egui's own menu style).
pub fn menu_style(s: &mut egui::Style) {
    egui::containers::menu::menu_style(s);
    s.text_styles.insert(TextStyle::Button, FontId::new(MENU_TEXT_SIZE, FontFamily::Proportional));
    s.text_styles.insert(TextStyle::Body, FontId::new(MENU_TEXT_SIZE, FontFamily::Proportional));
    s.spacing.item_spacing.y = 0.0;
    if s.visuals.dark_mode {
        s.visuals.override_text_color = Some(MENU_TEXT);
    }
}

/// Every font family the theme defines (fallback fonts such as the Japanese fonts are added to each
/// of them).
pub fn font_families() -> Vec<FontFamily> {
    vec![FontFamily::Proportional, FontFamily::Monospace, FontFamily::Name("semibold".into()), FontFamily::Name("medium".into())]
}

/// Install the Latin UI fonts, Chinese fallback (craft-fonts or an installed face), Japanese
/// craft-fonts, Arabic and Hebrew fallbacks (craft-fonts or installed faces, [`crate::rtl`]) and
/// egui visuals. Document names keep their glyphs in every interface language.
pub fn install(ctx: &egui::Context, t: &Tokens) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_REGULAR)));
    fonts.font_data.insert("inter-medium".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_MEDIUM)));
    fonts.font_data.insert("inter-semibold".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::INTER_SEMIBOLD)));
    fonts.font_data.insert("jbmono".into(), Arc::new(FontData::from_static(filmcraft_text::fonts::JETBRAINS_MONO_REGULAR)));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "jbmono".into());
    fonts.families.insert(FontFamily::Name("semibold".into()), vec!["inter-semibold".into(), "inter".into()]);
    fonts.families.insert(FontFamily::Name("medium".into()), vec!["inter-medium".into(), "inter".into()]);
    // Japanese before Chinese: the two share code points, and Japanese text must keep Japanese
    // glyph forms; Chinese faces still cover the hanzi Japanese fonts lack
    add_craft_fonts(&mut fonts);
    crate::cjk::install(&mut fonts);
    crate::rtl::install(&mut fonts);
    ctx.set_fonts(fonts);
    apply_visuals(ctx, t);
}

/// Name of the egui font for a craft-fonts entry.
fn craft_font_name(f: &filmcraft_text::fonts::CraftFont) -> String {
    format!("craft:{} {}", f.family, f.style)
}

/// Append the Japanese craft-fonts (empty unless built with `CRAFT_FONTS_DIR`) as the last fallbacks
/// of every font family, after the app's own fonts: BIZ UDPGothic first (bold before regular in
/// the "semibold" and "medium" families), then the other Japanese faces.
fn add_craft_fonts(fonts: &mut FontDefinitions) {
    let jpan: Vec<_> = filmcraft_text::fonts::craft_japanese().collect();
    for f in &jpan {
        fonts.font_data.insert(craft_font_name(f), Arc::new(FontData::from_static(f.bytes)));
    }
    for (family, stack) in fonts.families.iter_mut() {
        let heavy = matches!(family, FontFamily::Name(n) if matches!(n.as_ref(), "semibold" | "medium"));
        let mut order = jpan.clone();
        order.sort_by_key(|f| (!f.family.contains("Gothic"), (f.style == "Bold") != heavy));
        stack.extend(order.iter().map(|f| craft_font_name(f)));
    }
}

pub fn apply_visuals(ctx: &egui::Context, t: &Tokens) {
    let light = t.kind.is_light();
    // FilmCraft decides between light and dark itself (Settings ▸ Appearance ▸ Appearance Mode), so
    // egui must not switch to its other built-in style when the system appearance changes.
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    let mut v = if light { Visuals::light() } else { Visuals::dark() };
    v.panel_fill = t.panel_bg;
    v.window_fill = t.panel_bg;
    v.extreme_bg_color = t.field_bg;
    v.faint_bg_color = t.row_alt;
    v.override_text_color = Some(t.text);
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = t.accent_text();
    v.window_stroke = Stroke::new(1.0, t.field_border);
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(6);
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(140) };
    v.window_shadow = v.popup_shadow;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::same(t.radius_sm as u8);
    }
    v.widgets.noninteractive.bg_fill = t.panel_bg;
    v.widgets.noninteractive.weak_bg_fill = t.panel_bg;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.inactive.bg_fill = t.field_bg;
    v.widgets.inactive.weak_bg_fill = t.field_bg;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, t.field_border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.hovered.bg_fill = t.hover;
    v.widgets.hovered.weak_bg_fill = t.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.field_border);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, t.tab_text_active);
    v.widgets.active.bg_fill = t.pressed;
    v.widgets.active.weak_bg_fill = t.pressed;
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.tab_text_active);
    v.widgets.open.bg_fill = t.hover;
    v.widgets.open.weak_bg_fill = t.hover;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.menu_margin = egui::Margin::same(4);
        s.text_styles.insert(TextStyle::Body, FontId::new(12.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(12.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(15.0, FontFamily::Name("semibold".into())));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace));
        s.animation_time = ANIMATION_TIME;
    });
}

/// How long egui's fades and slides take (menus, collapsing sections, tooltips); 0 when
/// Settings ▸ Appearance ▸ "Animate zoom, scrolling and menus" is off (#2325).
pub const ANIMATION_TIME: f32 = 0.12;

#[cfg(test)]
mod tests {
    use super::*;

    const JAPANESE: &str = "日本語の文字";

    fn installed() -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx, &Tokens::for_kind(ThemeKind::default()));
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        ctx
    }

    #[test]
    fn light_theme_has_light_chrome_and_plot_surfaces() {
        let dark = Tokens::for_kind(ThemeKind::Dark);
        let light = Tokens::for_kind(ThemeKind::Light);
        let luminance = |c: Color32| u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b());

        for (name, dark_surface, light_surface) in [
            ("active pill", dark.pill_active_bg, light.pill_active_bg),
            ("clip bar", dark.clip_bar_bg, light.clip_bar_bg),
            ("control surface", dark.control_surface, light.control_surface),
            ("control well", dark.control_well, light.control_well),
            ("plot", dark.plot_bg, light.plot_bg),
            ("keyframe plot", dark.keyframe_plot_bg, light.keyframe_plot_bg),
            ("panner", dark.panner_bg, light.panner_bg),
            ("caption lane", dark.caption_track_bg, light.caption_track_bg),
            ("active toggle", dark.pressed, light.pressed),
            ("meter", dark.meter_bg, light.meter_bg),
            ("meter readout", dark.meter_readout_bg, light.meter_readout_bg),
        ] {
            assert!(luminance(light_surface) > luminance(dark_surface), "{name} did not adapt to Light");
        }
        assert!(luminance(light.control_handle) < luminance(light.control_handle_bg));
        assert!(luminance(light.plot_handle) < luminance(light.plot_bg));
        assert!(luminance(light.icon_active) < luminance(light.pressed));
        assert_eq!(Tokens::for_kind(ThemeKind::Medium).plot_bg, dark.plot_bg, "Medium keeps the established dark plot treatment");
    }

    #[test]
    fn dark_and_medium_keep_existing_audio_and_effect_palettes() {
        for kind in [ThemeKind::Dark, ThemeKind::Medium] {
            let t = Tokens::for_kind(kind);
            // Existing painted colors, recorded independently of the Light palette.
            assert_eq!(t.pill_active_bg, Color32::from_gray(0x3a));
            assert_eq!(t.clip_bar_bg, Color32::from_rgb(58, 58, 70));
            assert_eq!(t.control_surface, Color32::from_gray(0x24));
            assert_eq!(t.control_well, Color32::from_gray(0x10));
            assert_eq!(t.control_handle, Color32::from_gray(0xe0));
            assert_eq!(t.meter_bg, Color32::BLACK);
            assert_eq!(t.plot_bg, Color32::from_gray(0x18));
            assert_eq!(t.plot_grid, Color32::from_gray(0x2c));
            assert_eq!(t.keyframe_plot_bg, Color32::from_gray(0x19));
            assert_eq!(t.keyframe_plot_grid, Color32::from_gray(0x2a));
            assert_eq!(t.keyframe_plot_axis, Color32::from_gray(0x33));
            assert_eq!(t.keyframe_handle, Color32::from_gray(0xd0));
        }
    }

    /// The colours the highlight drives besides the accent (#707).
    fn highlight_blues(t: &Tokens) -> [Color32; 5] {
        [t.focus, t.playhead, t.timecode, t.hot_text, t.track_target]
    }

    #[test]
    fn the_default_highlight_leaves_the_theme_blues_as_designed() {
        let default = filmcraft_engine::settings::parse_hex(filmcraft_engine::settings::DEFAULT_HIGHLIGHT);
        for k in ThemeKind::ALL {
            assert_eq!(highlight_blues(&Tokens::for_kind(k).with_appearance(default, false)), highlight_blues(&Tokens::for_kind(k)), "{k:?}");
            assert_eq!(Tokens::for_kind(k).with_appearance(default, false).accent, Tokens::for_kind(k).accent, "{k:?} keeps its own highlight");
        }
    }

    /// WCAG 2.x relative luminance and contrast ratio (sRGB).
    fn contrast(a: Color32, b: Color32) -> f64 {
        fn luminance(c: Color32) -> f64 {
            let lin = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
        }
        let (l1, l2) = (luminance(a), luminance(b));
        (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
    }

    /// The Evolve palette: normal text at WCAG AA (4.5:1) on every surface it is drawn on, large
    /// or decorative text (faint labels, ruler ticks) at 3:1, and the highlight distinct from the
    /// panels it marks.
    #[test]
    fn evolve_text_contrast_meets_wcag_aa() {
        let t = Tokens::for_kind(ThemeKind::Evolve);
        assert_eq!(t.kind, ThemeKind::Evolve);
        let surfaces = [
            ("panel", t.panel_bg),
            ("tab strip", t.tab_bg),
            ("header", t.header_bg),
            ("field", t.field_bg),
            ("hover", t.hover),
            ("row", t.row_alt),
            ("selected row", t.row_selected),
            ("timeline", t.tl_bg),
            ("track", t.tl_track_bg),
            ("ruler", t.tl_ruler_bg),
            ("monitor", t.monitor_bg),
            ("app", t.app_bg),
        ];
        for (name, bg) in surfaces {
            for (what, fg) in [
                ("text", t.text),
                ("secondary text", t.text_dim),
                ("tab text", t.tab_text),
                ("active tab text", t.tab_text_active),
                ("icons", t.icon),
                ("timecode", t.timecode),
                ("ruler text", t.tl_ruler_text),
                ("hot text", t.hot_text),
            ] {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{what} on {name}: {ratio:.2}:1 is below WCAG AA");
            }
            let faint = contrast(t.text_faint, bg);
            assert!(faint >= 3.0, "faint text on {name}: {faint:.2}:1");
        }
        // the accent as a fill under white labels (primary buttons, selections), also hovered, and
        // as text on the surfaces (links, labels): normal text, 4.5:1
        for (what, fill) in [("accent", t.accent), ("hovered accent", t.accent_hover)] {
            let ratio = contrast(Color32::WHITE, fill);
            assert!(ratio >= 4.5, "white labels on the {what}: {ratio:.2}:1 is below WCAG AA");
        }
        for (name, bg) in surfaces {
            let ratio = contrast(t.accent_text(), bg);
            assert!(ratio >= 4.5, "accent text on {name}: {ratio:.2}:1 is below WCAG AA");
        }
        assert!(contrast(t.accent, t.panel_bg) >= 3.0 && contrast(t.focus, t.panel_bg) >= 3.0 && contrast(t.playhead, t.tl_track_bg) >= 3.0);
        // Dark chrome around lighter panels, as in page-based suites: the window background is the darkest surface.
        let lum = |c: Color32| u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b());
        assert!(lum(t.app_bg) < lum(t.header_bg) && lum(t.header_bg) < lum(t.panel_bg) && lum(t.monitor_bg) < lum(t.panel_bg));
        // the theme is its own: not the Darkest palette with another accent
        let dark = Tokens::for_kind(ThemeKind::Dark);
        assert!(t.panel_bg != dark.panel_bg && t.accent != dark.accent && t.playhead != dark.playhead && t.timecode != dark.timecode);
        assert!(!t.kind.is_light());
    }

    /// Theme names round-trip through the preference value, `ui.set {theme}` and serde.
    #[test]
    fn evolve_theme_names_round_trip() {
        for k in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_pref(k.pref_name()), k);
            assert_eq!(ThemeKind::from_name(&format!("{k:?}")), Some(k), "the view.theme.* id");
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<ThemeKind>(&json).unwrap(), k);
        }
        assert_eq!(ThemeKind::from_pref("nonsense"), ThemeKind::Dark);
        // a highlight of the user's own still recolours the Evolve theme
        let t = Tokens::for_kind(ThemeKind::Evolve).with_appearance(Some([0x2f, 0x6b, 0xdf]), false);
        assert_eq!(t.accent, Tokens::for_kind(ThemeKind::Evolve).accent, "the default highlight is the theme's own");
        let t = Tokens::for_kind(ThemeKind::Evolve).with_appearance(Some([0x40, 0xa0, 0x60]), false);
        assert_eq!(t.accent, Color32::from_rgb(0x40, 0xa0, 0x60));
        assert_ne!(t.playhead, Tokens::for_kind(ThemeKind::Evolve).playhead);
    }

    #[test]
    fn a_highlight_of_ones_own_colours_the_playhead_timecodes_focus_and_targeting() {
        let pink = Color32::from_rgb(0xe0, 0x45, 0x7b);
        let t = Tokens::for_kind(ThemeKind::Dark).with_appearance(Some([0xe0, 0x45, 0x7b]), false);
        let hue = |c: Color32| egui::ecolor::HsvaGamma::from(c).h;
        for (c, was) in highlight_blues(&t).into_iter().zip(highlight_blues(&Tokens::for_kind(ThemeKind::Dark))) {
            assert_ne!(c, was);
            assert!((hue(c) - hue(pink)).abs() < 0.01, "{c:?} has the highlight's hue");
        }
        let v = |c: Color32| egui::ecolor::HsvaGamma::from(c).v;
        assert!(v(t.playhead) > v(pink) && v(t.timecode) > v(pink) && v(t.track_target) < v(pink), "lighter and darker as the default blues are");
    }

    /// Chinese media and track names must render even when the interface stays in English.
    /// Compare the rasterized glyphs with the missing-glyph box, including custom weight families.
    #[test]
    fn chinese_names_render_in_every_theme_and_font_family() {
        const CHINESE: &str = "中文旁白音乐声轨测试简体汉语繁體漢語车";
        filmcraft_text::fonts::scan_system();
        let available = filmcraft_text::fonts::all_faces()
            .iter()
            .any(|f| matches!(f.info.origin, "system" | filmcraft_text::fonts::CRAFT_ORIGIN) && f.covers_text(CHINESE));
        if !available {
            eprintln!("SKIPPED: no craft-fonts or installed Chinese font");
            return;
        }
        let ctx = egui::Context::default();
        for kind in ThemeKind::ALL {
            install(&ctx, &Tokens::for_kind(kind));
            ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            ctx.fonts_mut(|fonts| {
                for family in font_families() {
                    let font = FontId::new(13.0, family);
                    let missing = fonts.layout_no_wrap("\u{fffd}".into(), font.clone(), Color32::WHITE);
                    let missing_uv = missing.rows[0].glyphs[0].uv_rect;
                    let names = fonts.layout_no_wrap(CHINESE.into(), font.clone(), Color32::WHITE);
                    assert_eq!(names.rows.iter().map(|r| r.glyphs.len()).sum::<usize>(), CHINESE.chars().count());
                    for glyph in names.rows.iter().flat_map(|r| &r.glyphs) {
                        assert!(!glyph.uv_rect.is_nothing(), "empty {} in {font:?}", glyph.chr);
                        assert_ne!(glyph.uv_rect, missing_uv, "missing {} in {kind:?} / {font:?}", glyph.chr);
                    }
                }
            });
        }
        // the layouts above put glyphs in the atlas: take that update so it isn't dropped unapplied
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    /// Typed, pasted or IME-composed Hangul draws real glyphs, not the missing-glyph box (artcraft#2093).
    #[test]
    fn korean_text_renders_with_an_installed_korean_font() {
        const KOREAN: &str = "한국어 입력 테스트";
        filmcraft_text::fonts::scan_system();
        // `covers_text`, not `has_char`: probing keeps no font file loaded (see `cjk::find_system_font`)
        let available = filmcraft_text::fonts::all_faces().iter().any(|f| f.info.origin == "system" && f.covers_text(KOREAN));
        if !available {
            eprintln!("SKIPPED: no installed Korean font");
            return;
        }
        let ctx = egui::Context::default();
        install(&ctx, &Tokens::for_kind(ThemeKind::Dark));
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            for family in font_families() {
                let font = FontId::new(13.0, family);
                let missing = fonts.layout_no_wrap("\u{fffd}".into(), font.clone(), Color32::WHITE);
                let missing_uv = missing.rows[0].glyphs[0].uv_rect;
                let text = fonts.layout_no_wrap(KOREAN.into(), font.clone(), Color32::WHITE);
                for glyph in text.rows.iter().flat_map(|r| &r.glyphs).filter(|g| g.chr != ' ') {
                    assert_ne!(glyph.uv_rect, missing_uv, "missing {} in {font:?}", glyph.chr);
                }
            }
        });
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    /// Built with craft-fonts: every font family ends with the Japanese faces (BIZ UDPGothic
    /// first), and Japanese text gets real glyphs, not the replacement box.
    #[test]
    fn japanese_renders_with_craft_fonts() {
        if filmcraft_text::fonts::craft_japanese().next().is_none() {
            eprintln!("SKIPPED: built without craft-fonts (set CRAFT_FONTS_DIR to run)");
            return;
        }
        let ctx = installed();
        ctx.fonts_mut(|fonts| {
            let defs = fonts.definitions().clone();
            for (family, stack) in &defs.families {
                // installed faces come after the craft run: right-to-left ones (Hebrew is not in
                // craft-fonts) and the Hangul face (none is in craft-fonts)
                let stack: Vec<&String> = stack.iter().filter(|n| !crate::rtl::is_system_face(n) && n.as_str() != crate::cjk::KOREAN_FACE).collect();
                let first = stack.iter().position(|n| n.starts_with("craft:")).unwrap_or(stack.len());
                assert!(first > 0 && stack[first..].iter().all(|n| n.starts_with("craft:")), "{family:?}: {stack:?}");
                let first_japanese = stack.iter().find(|n| n.starts_with("craft:BIZ") || n.starts_with("craft:Shippori"));
                assert!(first_japanese.is_some_and(|n| n.starts_with("craft:BIZ UDPGothic")), "{family:?}: {stack:?}");
            }
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let font = FontId::new(13.0, family);
                for ch in JAPANESE.chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing {ch} in {font:?}");
                }
            }
        });
        // laid out and drawn: the replacement box (U+FFFD) would show as one glyph per character
        // from the fallback face; a real face gives each character its own width
        let galley = ctx.fonts_mut(|f| f.layout_no_wrap(JAPANESE.into(), FontId::new(13.0, FontFamily::Proportional), Color32::WHITE));
        assert_eq!(galley.rows.iter().map(|r| r.glyphs.len()).sum::<usize>(), JAPANESE.chars().count());
        assert!(galley.size().x > 13.0 * 4.0, "{:?}", galley.size());
    }

    /// Latin text keeps its UI faces, with or without craft-fonts and system Chinese fonts.
    #[test]
    fn works_without_craft_fonts() {
        let ctx = installed();
        let n = filmcraft_text::fonts::craft_japanese().count() + crate::cjk::craft_fonts().count() + crate::rtl::craft_fonts().count();
        ctx.fonts_mut(|fonts| {
            let defs = fonts.definitions().clone();
            assert_eq!(defs.font_data.keys().filter(|k| k.starts_with("craft:")).count(), n);
            assert_eq!(defs.families[&FontFamily::Proportional].first().map(String::as_str), Some("inter"));
            assert_eq!(defs.families[&FontFamily::Monospace].first().map(String::as_str), Some("jbmono"));
            assert!(fonts.has_glyph(&FontId::new(13.0, FontFamily::Proportional), 'A'));
            if n == 0 {
                assert!(defs.families.values().flatten().all(|name| !name.starts_with("craft:")));
            }
        });
    }
}
