//! Menus and UI-level commands. The menu bar is generated from the engine registry plus the UI
//! command table below (commands that only affect the frontend: tools, playback, zoom, panels).
//! `invoke` is the single entry point used by menus, shortcuts and the control channel.

use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::state::{Mode, Tool};

pub struct UiCommand {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: &'static [&'static str],
    pub shortcut: Option<&'static str>,
}

macro_rules! uic {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr) => {
        UiCommand { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc }
    };
}

pub const UI_COMMANDS: &[UiCommand] = &[
    uic!("window.dockAction", "Docking Action", [], None),
    uic!("window.dockPanel", "Dock Panel", [], None),
    uic!("window.floatPanel", "Undock Panel", [], None),
    uic!("window.closePanel", "Close Panel", [], None),
    uic!("app.language.english", "English", ["Edit", "Language"], None),
    uic!("app.language.japanese", "日本語", ["Edit", "Language"], None),
    uic!("app.language.spanish", "Español", ["Edit", "Language"], None),
    uic!("app.language.portuguese", "Português (Brasil)", ["Edit", "Language"], None),
    uic!("app.language.ukrainian", "Українська", ["Edit", "Language"], None),
    uic!("app.language.chinese", "简体中文", ["Edit", "Language"], None),
    uic!("app.language.german", "Deutsch", ["Edit", "Language"], None),
    uic!("app.language.russian", "Русский", ["Edit", "Language"], None),
    uic!("app.language.french", "Français", ["Edit", "Language"], None),
    uic!("app.language.indonesian", "Bahasa Indonesia", ["Edit", "Language"], None),
    uic!("app.language.italian", "Italiano", ["Edit", "Language"], None),
    uic!("app.language.korean", "한국어", ["Edit", "Language"], None),
    uic!("source.playback.toggle", "Source Play/Stop", [], None),
    uic!("source.playback.play", "Play Source", [], None),
    uic!("source.playback.stop", "Stop Source", [], None),
    uic!("playback.toggle", "Play/Stop", [], Some("Space")),
    uic!("playback.forward", "Shuttle Right", [], Some("L")),
    uic!("playback.stop", "Shuttle Stop", [], Some("K")),
    uic!("playback.reverse", "Shuttle Left", [], Some("J")),
    uic!("playback.slowForward", "Shuttle Slow Right", [], None),
    uic!("playback.slowReverse", "Shuttle Slow Left", [], None),
    uic!("playback.playAround", "Play Around", [], None),
    uic!("playback.inToOut", "Play In to Out", [], Some("Alt+K")),
    uic!("playback.loop", "Loop", [], None),
    uic!("view.zoomIn", "Zoom In", ["View"], Some("=")),
    uic!("view.zoomOut", "Zoom Out", ["View"], Some("-")),
    uic!("view.zoomToSequence", "Zoom to Sequence", ["View"], Some("\\")),
    uic!("view.resetInterfaceZoom", "Reset Interface Zoom", ["View"], None),
    uic!("view.playbackRes.full", "Full", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.half", "1/2", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.quarter", "1/4", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.eighth", "1/8", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.sixteenth", "1/16", ["View", "Playback Resolution"], None),
    uic!("view.pausedRes.full", "Full", ["View", "Paused Resolution"], None),
    uic!("view.pausedRes.half", "1/2", ["View", "Paused Resolution"], None),
    uic!("view.pausedRes.quarter", "1/4", ["View", "Paused Resolution"], None),
    uic!("view.pausedRes.eighth", "1/8", ["View", "Paused Resolution"], None),
    uic!("view.pausedRes.sixteenth", "1/16", ["View", "Paused Resolution"], None),
    uic!("view.highQualityPlayback", "High Quality Playback", ["View"], None),
    uic!("view.display.composite", "Composite Video", ["View", "Display Mode"], None),
    uic!("view.display.alpha", "Alpha", ["View", "Display Mode"], None),
    uic!("view.display.red", "Red", ["View", "Display Mode"], None),
    uic!("view.display.green", "Green", ["View", "Display Mode"], None),
    uic!("view.display.blue", "Blue", ["View", "Display Mode"], None),
    uic!("view.display.multicam", "Multi-Camera", ["View", "Display Mode"], None),
    uic!("view.display.audioWaveform", "Audio Waveform", ["View", "Display Mode"], None),
    uic!("view.display.comparison", "Comparison View", ["View", "Display Mode"], None),
    uic!("view.display.videoAndWaveform", "Video and Audio Waveform Split", ["View", "Display Mode"], None),
    uic!("view.magnification.fit", "Fit", ["View", "Magnification"], None),
    uic!("view.magnification.10", "10%", ["View", "Magnification"], None),
    uic!("view.magnification.25", "25%", ["View", "Magnification"], None),
    uic!("view.magnification.50", "50%", ["View", "Magnification"], None),
    uic!("view.magnification.75", "75%", ["View", "Magnification"], None),
    uic!("view.magnification.100", "100%", ["View", "Magnification"], None),
    uic!("view.magnification.150", "150%", ["View", "Magnification"], None),
    uic!("view.magnification.200", "200%", ["View", "Magnification"], None),
    uic!("view.magnification.400", "400%", ["View", "Magnification"], None),
    uic!("view.magnification.800", "800%", ["View", "Magnification"], None),
    uic!("view.magnification.1600", "1600%", ["View", "Magnification"], None),
    uic!("view.showRulers", "Show Rulers", ["View"], None),
    uic!("view.showGuides", "Show Guides", ["View"], None),
    uic!("view.lockGuides", "Lock Guides", ["View"], None),
    uic!("view.addGuide", "Add Guide…", ["View"], None),
    uic!("view.clearGuides", "Clear Guides", ["View"], None),
    uic!("view.snapInProgramMonitor", "Snap in Program Monitor", ["View"], None),
    // the monitors' wrench menus only, as in the reference app
    uic!("view.transparencyGrid", "Transparency Grid", [], None),
    uic!("view.safeMargins", "Safe Margins", ["View", "Guide Templates"], None),
    uic!("view.guideTemplates.save", "Save Guides as Template…", ["View", "Guide Templates"], None),
    uic!("view.guideTemplates.manage", "Manage Guides…", ["View", "Guide Templates"], None),
    uic!("view.guideTemplates.apply", "Apply Guide Template", [], None),
    uic!("view.guideTemplates.delete", "Delete Guide Template", [], None),
    uic!("view.compare.setReference", "Set Comparison Reference", [], None),
    uic!("view.dynamicAudioWaveforms", "Dynamic Audio Waveforms", ["View"], None),
    uic!("multicam.toggleView", "Multi-Camera View", ["View"], Some("Shift+0")),
    uic!("multicam.recordToggle", "Multi-Camera Record On/Off Toggle", [], Some("0")),
    uic!("multicam.editCamerasDialog", "Edit Cameras…", [], None),
    uic!("voiceover.recordToggle", "Voice-over Record", [], None),
    uic!("voiceover.settingsDialog", "Voice-Over Record Settings…", [], None),
    // ids follow `ThemeKind`, labels follow Settings ▸ Appearance ▸ Color Theme (`darkest`, `dark`, `light`)
    uic!("view.theme.dark", "Darkest", ["View", "Appearance"], None),
    uic!("view.theme.medium", "Dark", ["View", "Appearance"], None),
    uic!("view.theme.light", "Light", ["View", "Appearance"], None),
    uic!("view.appearanceMode.next", "Next Appearance Mode", ["View", "Appearance"], None),
    uic!("window.workspace.editing", "Editing", ["Window", "Workspaces"], Some("Alt+Shift+1")),
    uic!("window.workspace.assembly", "Assembly", ["Window", "Workspaces"], Some("Alt+Shift+2")),
    uic!("window.workspace.color", "Color", ["Window", "Workspaces"], Some("Alt+Shift+3")),
    uic!("window.workspace.effects", "Effects", ["Window", "Workspaces"], Some("Alt+Shift+4")),
    uic!("window.workspace.audio", "Audio", ["Window", "Workspaces"], Some("Alt+Shift+5")),
    uic!("window.workspace.captionsandgraphics", "Captions and Graphics", ["Window", "Workspaces"], Some("Alt+Shift+6")),
    uic!("window.workspace.allpanels", "All Panels", ["Window", "Workspaces"], None),
    uic!("window.workspace.reset", "Reset to Saved Layout", ["Window", "Workspaces"], Some("Alt+Shift+0")),
    uic!("window.workspace.saveChanges", "Save Changes to this Workspace", ["Window", "Workspaces"], None),
    uic!("window.workspace.saveAs", "Save as New Workspace…", ["Window", "Workspaces"], None),
    uic!("window.workspace.edit", "Edit Workspaces…", ["Window", "Workspaces"], None),
    uic!("tool.selection", "Selection Tool", [], Some("V")),
    uic!("tool.trackSelectForward", "Track Select Forward Tool", [], Some("A")),
    uic!("tool.trackSelectBackward", "Track Select Backward Tool", [], Some("Shift+A")),
    uic!("tool.ripple", "Ripple Edit Tool", [], Some("B")),
    uic!("tool.rolling", "Rolling Edit Tool", [], Some("N")),
    uic!("tool.rateStretch", "Rate Stretch Tool", [], Some("R")),
    uic!("tool.remix", "Remix Tool", [], None),
    uic!("tool.razor", "Razor Tool", [], Some("C")),
    uic!("tool.slip", "Slip Tool", [], Some("Y")),
    uic!("tool.slide", "Slide Tool", [], Some("U")),
    uic!("tool.pen", "Pen Tool", [], Some("P")),
    uic!("tool.hand", "Hand Tool", [], Some("H")),
    uic!("tool.zoom", "Zoom Tool", [], Some("Z")),
    uic!("tool.type", "Type Tool", [], Some("T")),
    uic!("tool.verticalType", "Vertical Type Tool", [], None),
    uic!("mode.import", "Import", [], None),
    uic!("mode.edit", "Edit", [], None),
    uic!("mode.export", "Export", ["File", "Export"], Some("Cmd+M")),
    uic!("help.discord", "Join the ArtCraft Discord…", ["Help"], None),
    uic!("help.website", "ArtCraft Website", ["Help"], None),
    uic!("help.appPage", "FilmCraft on getartcraft.com", ["Help"], None),
    uic!("help.github", "FilmCraft on GitHub", ["Help"], None),
    uic!("help.reportIssue", "Report an Issue…", ["Help"], None),
    uic!("help.revealLogFiles", "Reveal Log Files…", ["Help"], None),
    uic!("help.systemCompatibilityReport", "System Compatibility Report…", ["Help"], None),
    uic!("app.checkForUpdates", "Check for Updates…", ["Help"], None),
    uic!("app.about", "About FilmCraft", ["Help"], None),
    uic!("app.keyboardShortcuts", "Keyboard Shortcuts…", ["Edit"], Some("Cmd+Alt+K")),
    uic!("app.settings.general", "General…", ["Edit", "Preferences"], Some("Cmd+,")),
    uic!("app.settings.appearance", "Appearance…", ["Edit", "Preferences"], None),
    uic!("app.settings.audio", "Audio…", ["Edit", "Preferences"], None),
    uic!("app.settings.audioHardware", "Audio Hardware…", ["Edit", "Preferences"], None),
    uic!("app.settings.autoSave", "Auto Save…", ["Edit", "Preferences"], None),
    uic!("app.settings.color", "Color…", ["Edit", "Preferences"], None),
    uic!("app.settings.graphics", "Graphics…", ["Edit", "Preferences"], None),
    uic!("app.settings.labels", "Labels…", ["Edit", "Preferences"], None),
    uic!("app.settings.media", "Media…", ["Edit", "Preferences"], None),
    uic!("app.settings.mediaAnalysis", "Media Analysis & Transcription…", ["Edit", "Preferences"], None),
    uic!("app.settings.mediaCache", "Media Cache…", ["Edit", "Preferences"], None),
    uic!("app.settings.memory", "Memory…", ["Edit", "Preferences"], None),
    uic!("app.settings.playback", "Playback…", ["Edit", "Preferences"], None),
    uic!("app.settings.plugins", "Plugins…", ["Edit", "Preferences"], None),
    uic!("app.settings.timeline", "Timeline…", ["Edit", "Preferences"], None),
    uic!("app.settings.trim", "Trim…", ["Edit", "Preferences"], None),
];

pub fn panel_command_id(p: PanelKind) -> String {
    format!("window.panel.{}", p.id())
}

/// Explicit monitor parameters override keyboard focus.
pub fn targets_source(app: &FilmcraftApp, params: &Value) -> bool {
    match params.get("monitor").and_then(Value::as_str) {
        Some("source") => true,
        Some(_) => false,
        None => app.ui.focused == PanelKind::Source,
    }
}

/// Execute a UI or engine command by id.
pub fn invoke(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, mut params: Value) -> Result<Value, String> {
    if id == "edit.selectAll" && (crate::panels::graphics::select_all_edited_text(app) || crate::panels::effect_controls::select_all_keyframes(app)) {
        return Ok(Value::Null);
    }
    if filmcraft_engine::source_monitor::source_command(id) && targets_source(app, &params) && params.get("target").is_none() {
        let object = params.as_object_mut().ok_or("command parameters must be an object")?;
        object.insert("target".into(), json!("source"));
    }
    if matches!(
        id,
        "app.language.english"
            | "app.language.japanese"
            | "app.language.spanish"
            | "app.language.portuguese"
            | "app.language.ukrainian"
            | "app.language.chinese"
            | "app.language.german"
            | "app.language.russian"
            | "app.language.french"
            | "app.language.indonesian"
            | "app.language.italian"
            | "app.language.korean"
    ) {
        // Japanese needs the craft-fonts (built with CRAFT_FONTS_DIR) or a font installed on the system
        if id == "app.language.japanese" && !crate::i18n::install_japanese_font(ctx) {
            return Err("no Japanese font is installed on this system (for example Noto Sans CJK JP); the interface stays in English".into());
        }
        // Chinese needs the Chinese fallback theme::install adds (craft-fonts or a system face)
        if id == "app.language.chinese" && !crate::i18n::chinese_font_available() {
            return Err("no Chinese font is installed on this system (for example Noto Sans CJK SC or Microsoft YaHei); the interface stays in English".into());
        }
        // Korean needs an installed Hangul font (none is bundled; see cjk.rs)
        if id == "app.language.korean" && !crate::i18n::korean_font_available() {
            return Err("no Korean font is installed on this system (for example Malgun Gothic or Noto Sans CJK KR); the interface stays in English".into());
        }
        let language = match id {
            "app.language.japanese" => crate::i18n::Language::Ja,
            "app.language.spanish" => crate::i18n::Language::Es,
            "app.language.portuguese" => crate::i18n::Language::PtBr,
            "app.language.ukrainian" => crate::i18n::Language::Uk,
            "app.language.chinese" => crate::i18n::Language::ZhCn,
            "app.language.german" => crate::i18n::Language::De,
            "app.language.russian" => crate::i18n::Language::Ru,
            "app.language.french" => crate::i18n::Language::Fr,
            "app.language.indonesian" => crate::i18n::Language::Id,
            "app.language.italian" => crate::i18n::Language::It,
            "app.language.korean" => crate::i18n::Language::Ko,
            _ => crate::i18n::Language::En,
        };
        // The preference is updated in memory before it is written, so a failed write (read-only
        // or full disk) still switches the interface; it only can't be remembered for next time.
        let saved = app.session.execute("prefs.set", json!({"key": "general.interfaceLanguage", "value": language.code()}));
        app.ui.language = language;
        crate::i18n::set_current(language);
        if let Err(e) = saved {
            app.ui.status = tlf!("The language changed but could not be saved: {e}", e);
        }
        ctx.request_repaint();
        return Ok(json!(app.ui.language));
    }
    if id == "file.exportFrame" {
        if params.get("target").is_none() {
            let source = targets_source(app, &params);
            params.as_object_mut().ok_or("command parameters must be an object")?.insert("target".into(), json!(if source { "source" } else { "program" }));
        }
        if params.get("path").is_none() {
            return crate::panels::frame_export::open(app, ctx, &params);
        }
    }
    if id == "perf.stats" {
        // the engine's counters plus playback, frame workers and UI timings
        return Ok(crate::perf::stats(app));
    }
    if let Some(result) = crate::dock_ui::route(app, ctx, id, &params) {
        return result;
    }
    if let Some(rest) = id.strip_prefix("window.panel.") {
        let p = PanelKind::from_name(rest).ok_or_else(|| format!("unknown panel `{rest}`"))?;
        app.show_panel(p);
        return Ok(Value::Null);
    }
    if let Some(r) = crate::panels::workspaces::route(app, id, &params) {
        return r;
    }
    if let Some(t) = id.strip_prefix("tool.") {
        let tool = Tool::from_name(t).ok_or_else(|| format!("unknown tool `{t}`"))?;
        app.ui.tool = tool;
        return Ok(json!({"tool": tool}));
    }
    if let Some(r) = crate::panels::keyboard::route(app, ctx, id, &params) {
        return r;
    }
    if !targets_source(app, &params)
        && let Some(r) = crate::panels::trim_monitor::route_transport(app, ctx, id)
    {
        return r;
    }
    if let Some(r) = crate::panels::multicam::route(app, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    if let Some(r) = crate::panels::voiceover::route(app, ctx, id, &params) {
        return r;
    }
    if let Some(r) = crate::panels::clip_dialogs::route(app, id, &params) {
        return r;
    }
    if let Some(r) = crate::panels::misc::route_history(app, ctx, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    if let Some(r) = crate::panels::settings::route(app, id) {
        return r;
    }
    if let Some(r) = crate::panels::graphics_templates::route(app, ctx, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    if let Some(r) = crate::panels::menu_dialogs::route(app, ctx, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    if let Some(r) = crate::panels::media_dialogs::route(app, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    if let Some(r) = crate::panels::monitor_view::route(app, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    match id {
        "playback.slowForward" | "playback.slowReverse" if targets_source(app, &params) => {
            app.shuttle_source(if id == "playback.slowForward" { 0.25 } else { -0.25 })?;
            return Ok(json!({"playing": app.source_playback.clock.playing, "speed": app.source_playback.clock.speed}));
        }
        "playback.slowForward" | "playback.slowReverse" => {
            app.play(if id == "playback.slowForward" { 0.25 } else { -0.25 });
            return Ok(json!({"speed": app.playback.speed}));
        }
        "source.playback.toggle" => {
            app.toggle_source_play()?;
            return Ok(json!({"playing": app.source_playback.clock.playing}));
        }
        "source.playback.play" => {
            app.play_source()?;
            return Ok(json!({"playing": app.source_playback.clock.playing}));
        }
        "source.playback.stop" => {
            app.stop_source();
            return Ok(Value::Null);
        }
        "playback.toggle"
            if params.get("monitor").and_then(Value::as_str) == Some("source") || (params.get("monitor").is_none() && app.ui.focused == PanelKind::Source) =>
        {
            app.toggle_source_play()?;
            return Ok(json!({"playing": app.source_playback.clock.playing}));
        }
        "playback.toggle" => {
            app.toggle_play(1.0);
            return Ok(json!({"playing": app.playback.playing}));
        }
        // L / J: pressing again in the same direction doubles the speed, up to 8×
        "playback.forward" | "playback.reverse" if targets_source(app, &params) => {
            let (clock, dir) = (&app.source_playback.clock, if id == "playback.forward" { 1.0 } else { -1.0 });
            let speed = if clock.playing && clock.speed * dir > 0.0 { (clock.speed * 2.0).clamp(-8.0, 8.0) } else { dir };
            app.shuttle_source(speed)?;
            return Ok(json!({"playing": app.source_playback.clock.playing, "speed": app.source_playback.clock.speed}));
        }
        "playhead.stepBack" | "playhead.stepForward" | "playhead.stepBack5" | "playhead.stepForward5" if targets_source(app, &params) => {
            app.stop_source();
            let item = app.session.state.source_item.ok_or("Open a Source clip first")?;
            let view = filmcraft_engine::clip_ops::source_view(&app.session, item).ok_or("Source clip is unavailable")?;
            let count = if id.ends_with('5') { i64::from(app.session.prefs.playback.step_many_frames) } else { 1 };
            let direction = if id.contains("Back") { -1 } else { 1 };
            let time = if view.rate.frame_duration().0 == 0 {
                app.session.state.source_playhead
            } else {
                let frame = view.rate.frame_at(app.session.state.source_playhead).saturating_add(count.saturating_mul(direction));
                view.rate.tick_of(frame)
            };
            return app.session.execute("source.setPlayhead", json!({"time": time.0})).map_err(|e| e.to_string());
        }
        "playback.forward" => {
            let s = if app.playback.playing && app.playback.speed > 0.0 { (app.playback.speed * 2.0).min(8.0) } else { 1.0 };
            app.play(s);
            return Ok(json!({"speed": app.playback.speed}));
        }
        "playback.reverse" => {
            let s = if app.playback.playing && app.playback.speed < 0.0 { (app.playback.speed * 2.0).max(-8.0) } else { -1.0 };
            app.play(s);
            return Ok(json!({"speed": app.playback.speed}));
        }
        "playback.stop" => {
            if targets_source(app, &params) {
                app.stop_source();
            } else {
                app.stop();
            }
            return Ok(Value::Null);
        }
        "playback.inToOut" if targets_source(app, &params) => {
            app.play_source_range(false, false)?;
            return Ok(Value::Null);
        }
        "playback.loop" if targets_source(app, &params) => {
            app.source_playback.clock.looping = !app.source_playback.clock.looping;
            return Ok(json!({"loop": app.source_playback.clock.looping}));
        }
        "playback.inToOut" => {
            let seq = app.session.active_sequence();
            let marked_out = seq.and_then(|q| q.mark_out);
            let out = seq.and_then(|q| q.mark_out.map(|o| o + q.settings.frame_rate.frame_duration()));
            if let Some(i) = seq.and_then(|q| q.mark_in) {
                app.session.set_playhead(i);
            }
            app.play(1.0);
            app.playback.stop_at = out;
            app.playback.rest_at = marked_out;
            return Ok(Value::Null);
        }
        "playback.loop" => {
            app.playback.looping = !app.playback.looping;
            return Ok(json!({"loop": app.playback.looping}));
        }
        "view.zoomIn" | "view.zoomOut" => {
            let f = if id == "view.zoomIn" { 1.6 } else { 1.0 / 1.6 };
            let ph = app.session.playhead().seconds();
            crate::panels::timeline::zoom_about(&mut app.ui.timeline, f, ph, app.last_timeline_width);
            return Ok(Value::Null);
        }
        "view.zoomToSequence" => {
            app.ui.timeline.fit_pending = true;
            return Ok(Value::Null);
        }
        // the whole interface back to Settings ▸ Appearance ▸ UI Scale, after Ctrl+Plus or a mouse
        // mapped to page zoom scaled it (#2253)
        "view.resetInterfaceZoom" => {
            let zoom = app.reset_ui_zoom(ctx);
            return Ok(json!({"zoom": zoom}));
        }
        "mode.import" => {
            app.ui.mode = Mode::Import;
            return Ok(Value::Null);
        }
        "mode.edit" => {
            app.ui.mode = Mode::Edit;
            return Ok(Value::Null);
        }
        "mode.export" => {
            app.ui.mode = Mode::Export;
            return Ok(Value::Null);
        }
        "app.checkForUpdates" => {
            let _ = crate::updates_ui::check(app);
            return Ok(serde_json::Value::Null);
        }
        "app.about" => {
            app.dialog = Some(crate::Dialog::About);
            return Ok(Value::Null);
        }
        id if crate::links::url_for(id).is_some() => {
            let url = crate::links::url_for(id).unwrap_or_default();
            crate::links::open(app, ctx, url);
            app.ui.status = tlf!("Opened {url}", url);
            return Ok(json!({"url": url}));
        }
        "help.shortcuts" | "app.keyboardShortcuts" => {
            crate::panels::shortcuts_dialog::open(app);
            return Ok(Value::Null);
        }
        // File ▸ Export ▸ AAF… / OMF… open their settings dialogs; with a path they export directly.
        "file.exportAaf" | "file.exportOmf" if params.get("path").is_none() => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            return crate::panels::interchange_export::open(app, ctx, id);
        }
        // File ▸ Export ▸ Media… opens the Export mode, like ⌘M (#382); with a path it exports directly.
        "file.exportMedia" if params.get("path").is_none() => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            app.ui.mode = Mode::Export;
            return Ok(json!({"mode": "export"}));
        }
        // Audio Gain from the menu or G opens the dialog; with params it applies directly.
        "clip.audioGain" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command("clip.audioGain").map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            if app.ui.audio_gain.mode.is_empty() {
                app.ui.audio_gain.mode = "adjust".into();
            }
            app.dialog = Some(crate::Dialog::AudioGain);
            return Ok(json!({"dialog": "audioGain"}));
        }
        // Colour dialogs from the menus; with params the engine command applies directly.
        "clip.interpretFootage" if params.get("colorSpace").is_none() && params.get("pixelAspect").is_none() => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            crate::panels::color_dialogs::open_interpret(app, &params);
            return Ok(json!({"dialog": "interpretFootage"}));
        }
        // Remix Properties… from the menu opens the dialog; with params it applies directly.
        "clip.remix.properties" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            return crate::panels::remix::open(app, ctx);
        }
        // Add Tracks… from the menu opens the dialog (new tracks after the last ones, as Premiere
        // offers); with params it adds the tracks directly.
        "sequence.addTracks" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            let seq = app.session.active_sequence();
            let count = |f: fn(&filmcraft_engine::project::Sequence) -> usize| seq.map_or(0, f);
            app.ui.add_tracks = crate::state::AddTracksDraft {
                video_after: count(|q| q.video_tracks.len()),
                audio_after: count(|q| q.audio_tracks.len()),
                submix_after: count(|q| q.submix_tracks.len()),
                ..Default::default()
            };
            app.dialog = Some(crate::Dialog::AddTracks);
            return Ok(json!({"dialog": "addTracks"}));
        }
        "sequence.deleteTracks" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            app.ui.delete_tracks = Default::default();
            app.dialog = Some(crate::Dialog::DeleteTracks);
            return Ok(json!({"dialog": "deleteTracks"}));
        }
        // Sequence Settings… from the menu opens the dialog; with params the engine command applies
        // them directly.
        "sequence.settings" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            crate::panels::sequence_settings::open(app);
            return Ok(json!({"dialog": "sequenceSettings"}));
        }
        // File ▸ New ▸ Sequence… (Cmd+N) opens New Sequence; with params (a clip, an agent) it
        // makes the sequence directly
        "file.newSequence" if params.as_object().is_none_or(|m| m.is_empty()) => {
            crate::panels::sequence_settings::open_new(app);
            return Ok(json!({"dialog": "newSequence"}));
        }
        "sequence.colorSettings" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            crate::panels::color_dialogs::open_sequence(app);
            return Ok(json!({"dialog": "sequenceColor"}));
        }
        // New Bin from menus, shortcuts and the Project panel asks for the name (inline rename);
        // agents pass `name` to name the bin directly
        "file.newBin" if params.get("name").is_none() => {
            let r = crate::panels::project::new_bin(app, &params);
            if let Err(e) = &r {
                app.ui.status = e.clone();
            }
            return r;
        }
        // From menus/shortcuts (no params) these ask first; agents pass params to act directly.
        "file.revert" if params.as_object().is_none_or(|m| m.is_empty()) && app.session.is_dirty() => {
            if app.session.path.is_none() {
                return Err("the project has not been saved yet".into());
            }
            app.dialog = Some(crate::Dialog::RevertConfirm);
            return Ok(json!({"dialog": "revert"}));
        }
        // Files or an image on the system clipboard paste into the Timeline, or import into the
        // Project panel (#611). Copying clips puts their names there as text, so media copied
        // *after* them still wins; media that was already there when Copy/Cut ran is ignored so
        // Paste of cut/copied clips still works (#697).
        "edit.paste" | "edit.pasteInsert"
            if params.as_object().is_none_or(|m| m.is_empty()) && matches!(app.ui.focused, PanelKind::Timeline | PanelKind::Project) =>
        {
            if let Some(r) = paste_clipboard_media(app, id == "edit.pasteInsert") {
                return r;
            }
        }
        "file.recover" if params.as_object().is_none_or(|m| m.is_empty()) => {
            if app.session.recovery_candidates().is_empty() {
                return Err("there are no unsaved changes to recover".into());
            }
            app.file_dialogs.recovery_choice = 0;
            app.dialog = Some(crate::Dialog::Recovery);
            return Ok(json!({"dialog": "recovery"}));
        }
        _ => {}
    }
    if id == "view.appearanceMode.next" {
        crate::panels::settings::cycle_appearance(app, ctx);
        return Ok(json!({"appearanceMode": app.session.prefs.appearance.appearance_mode}));
    }
    if let Some(th) = id.strip_prefix("view.theme.") {
        let k = crate::theme::ThemeKind::from_name(th).ok_or("unknown theme")?;
        crate::panels::settings::set_theme(app, ctx, k);
        return Ok(Value::Null);
    }
    // File ▸ Open Recent
    if let Some(i) = id.strip_prefix("file.openRecent.") {
        let path = i.parse::<usize>().ok().and_then(|i| app.session.prefs.general.recent_projects.get(i).cloned()).ok_or("no such recent project")?;
        return invoke(app, ctx, "file.open", json!({"path": path}));
    }
    if id == "file.clearRecent" {
        let mut next = app.session.prefs.clone();
        next.general.recent_projects.clear();
        app.session.set_prefs(next).map_err(|e| e.to_string())?;
        return Ok(Value::Null);
    }
    // File dialogs for commands that need a path.
    if (id == "file.import" && params.get("paths").is_none() && params.get("path").is_none())
        || (id == "file.importImageSequence" && params.get("path").is_none())
        || (id == "file.saveAs" && params.get("path").is_none())
        || (id == "file.saveCopy" && params.get("path").is_none())
        || (id == "file.open" && params.get("path").is_none())
        || (id == "file.save" && params.get("path").is_none() && app.session.path.is_none())
        || (matches!(id, "captions.import" | "captions.export") && params.get("path").is_none())
        || (id == "graphics.newFromFile" && params.get("path").is_none())
        || (crate::EXPORT_SAVE_DIALOGS.iter().any(|(c, ..)| *c == id) && params.get("path").is_none())
    {
        return app.file_dialog(id, &params);
    }
    let r = app.session.execute(id, params).map_err(|e| e.to_string());
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    if r.is_ok() && matches!(id, "playhead.start" | "playhead.end") {
        crate::panels::timeline::reveal_playhead(app, ctx);
    }
    let filled_clipboard = match id {
        "edit.copy" | "edit.cut" => r.is_ok(),
        // over a range with no clips on the targeted tracks they leave the clipboard (and the OS one) as it was
        "sequence.lift" | "sequence.extract" => r.as_ref().is_ok_and(|v| v["copied"].as_u64().is_some_and(|n| n > 0)),
        _ => false,
    };
    if filled_clipboard {
        remember_stale_clipboard_media(app);
        put_clip_names_on_system_clipboard(app, ctx);
    }
    r
}

/// Run the command `id` chosen in the native menu bar, which takes the Edit menu's key
/// equivalents (⌘C, ⌘X, ⌘V, ⌘A, ⌘Z, ⇧⌘Z) before any window sees them: while a text field has
/// the keyboard, in the main window or a floating panel's own window, the field gets the key
/// and the project command does not run.
pub fn activate_native(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str) -> Result<Value, String> {
    let (Some(window), Some(text)) = (text_field_window(ctx), text_command(id, ctx.os().is_mac())) else {
        return invoke(app, ctx, id, json!({}));
    };
    match text {
        TextCommand::Paste => ctx.send_viewport_cmd_to(window, egui::ViewportCommand::RequestPaste),
        TextCommand::Events(events) if window == egui::ViewportId::ROOT => {
            app.synthetic.extend(events);
            ctx.request_repaint();
        }
        TextCommand::Events(events) => craft_ui::docking::send_keys(ctx, window, events),
    }
    Ok(Value::Null)
}

/// The window whose text field has the keyboard: a floating panel's own OS window, or the main
/// window.
pub fn text_field_window(ctx: &egui::Context) -> Option<egui::ViewportId> {
    craft_ui::docking::typing_window(ctx).or_else(|| ctx.text_edit_focused().then_some(egui::ViewportId::ROOT))
}

/// Whether a focused text field takes the menu command `id` ([`activate_native`]): the native
/// menu keeps such items enabled while one has the keyboard, whatever the project allows.
pub fn edits_text(id: &str) -> bool {
    text_command(id, false).is_some()
}

/// What a focused text field does for an Edit menu command.
#[derive(Debug, PartialEq)]
enum TextCommand {
    /// The clipboard text, which the window asks for and receives as `Paste`.
    Paste,
    Events(Vec<egui::Event>),
}

fn text_command(id: &str, mac: bool) -> Option<TextCommand> {
    let key = |key, shift| {
        let modifiers = egui::Modifiers { mac_cmd: mac, ctrl: !mac, command: true, shift, alt: false };
        TextCommand::Events([true, false].map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }).into())
    };
    Some(match id {
        "edit.copy" => TextCommand::Events(vec![egui::Event::Copy]),
        "edit.cut" => TextCommand::Events(vec![egui::Event::Cut]),
        "edit.paste" => TextCommand::Paste,
        "edit.undo" => key(egui::Key::Z, false),
        "edit.redo" => key(egui::Key::Z, true),
        "edit.selectAll" => key(egui::Key::A, false),
        _ => return None,
    })
}

/// Snapshot OS files/image present at Copy/Cut. Many platforms keep a previous file list after
/// `copy_text`, so Paste would otherwise import those files instead of the clips (#697).
fn remember_stale_clipboard_media(app: &mut FilmcraftApp) {
    app.stale_clipboard_media = app.hooks.clipboard_media.as_mut().and_then(|f| f()).as_ref().map(clipboard_media_key);
}

fn clipboard_media_key(media: &crate::ClipboardMedia) -> String {
    match media {
        crate::ClipboardMedia::Files(paths) => format!("files:{}", paths.join("\0")),
        crate::ClipboardMedia::Image { width, height, rgba } => {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in rgba {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            format!("img:{width}x{height}:{h}")
        }
    }
}

/// Copied clips live in the session, but on Windows and Linux Ctrl+V only reaches the app when the
/// system clipboard holds text (an empty clipboard sends no paste at all, #199). Put the copied
/// clip names there, like other editors put their own data on the clipboard, so Ctrl+V pastes the
/// clips right after a copy.
fn put_clip_names_on_system_clipboard(app: &FilmcraftApp, ctx: &egui::Context) {
    let names: Vec<&str> = app.session.state.clipboard.iter().map(|(_, _, c)| c.name.as_str()).collect();
    if names.is_empty() {
        return;
    }
    let text = names.join("\n");
    ctx.copy_text(if text.trim().is_empty() { format!("{} clips", names.len()) } else { text });
}

/// Paste the files or the image on the system clipboard: placed on the Timeline when it has focus,
/// imported into the shown bin in the Project panel. None when the clipboard holds neither.
fn paste_clipboard_media(app: &mut FilmcraftApp, insert: bool) -> Option<Result<Value, String>> {
    // The paste event already carries the image bytes. Prefer them: re-reading the OS clipboard
    // is racy (it may have changed) and the web host installs no clipboard-media hook.
    let event_media = match app.pasted_image.take() {
        Some(image) => match paste_image_media(&image) {
            Ok(media) => Some(media),
            Err(error) => return Some(Err(error)),
        },
        None => None,
    };
    let (media, from_event) = match event_media {
        Some(media) => (media, true),
        None => ((app.hooks.clipboard_media.as_mut()?)()?, false),
    };
    if !from_event && app.stale_clipboard_media.as_ref() == Some(&clipboard_media_key(&media)) {
        return None;
    }
    let paths = match media {
        crate::ClipboardMedia::Files(paths) => paths,
        crate::ClipboardMedia::Image { width, height, rgba } => match encode_png(width, height, rgba)
            .and_then(|png| filmcraft_engine::paste_media::save_pasted_image(&mut app.session, &png).map_err(|e| e.to_string()))
        {
            Ok(path) => vec![path],
            Err(e) => return Some(Err(e)),
        },
    };
    let params = json!({"paths": paths, "insert": insert, "place": app.ui.focused == PanelKind::Timeline, "bin": app.import_bin().0});
    Some(app.session.execute("edit.pasteMedia", params).map_err(|e| e.to_string()))
}

/// Upper bound on the RGBA copy made from a clipboard image, matching the screenshot per-image
/// bound. The event's own `ColorImage` is still alive while this copy is encoded, so reject before
/// allocating it rather than only checking for integer overflow.
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// The RGBA bytes of an image delivered by `egui::Event::PasteImage`.
fn paste_image_media(image: &egui::ColorImage) -> Result<crate::ClipboardMedia, String> {
    let [width, height] = image.size;
    let len =
        width.checked_mul(height).and_then(|pixels| pixels.checked_mul(4)).ok_or_else(|| "the pasted image size overflows the clipboard buffer".to_string())?;
    if len > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(format!(
            "the pasted image is too large: {width}×{height} needs {len} RGBA bytes, over the {MAX_CLIPBOARD_IMAGE_BYTES}-byte clipboard image limit"
        ));
    }
    let mut rgba = Vec::with_capacity(len);
    for c in &image.pixels {
        // `Color32` is premultiplied internally; PNG/RgbaImage expect straight alpha. Round-trip
        // through the straight sRGB form so a semi-transparent clipboard pixel keeps its colour.
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    if rgba.len() != len {
        return Err("the pasted image does not match its reported size".into());
    }
    Ok(crate::ClipboardMedia::Image { width, height, rgba })
}

/// A compressed PNG of a clipboard image; an error when its size and pixels disagree.
fn encode_png(width: usize, height: usize, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let (w, h) = (u32::try_from(width).map_err(|e| e.to_string())?, u32::try_from(height).map_err(|e| e.to_string())?);
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("the image on the clipboard is malformed")?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(png)
}

/// A menu tree entry for display / `ui.menu.list`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub path: Vec<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
    /// Checkmark state of toggle / radio items (None = not checkable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

pub const MENUS: [&str; 9] = ["File", "Edit", "Clip", "Sequence", "Markers", "Graphics and Titles", "View", "Window", "Help"];

/// Labels only the native macOS app menu shows, with no command in the menu tree behind them: the
/// Settings submenu and the predefined Hide, Hide Others and Quit items. They are translated through
/// the catalogs like menu labels (`apps/filmcraft/src/native_menu.rs`).
pub const NATIVE_APP_MENU: [&str; 4] = ["Settings", "Hide FilmCraft", "Hide Others", "Quit FilmCraft"];

pub fn menu_items(app: &FilmcraftApp) -> Vec<MenuItem> {
    let mut v = menu_items_for(&app.session);
    for it in &mut v {
        it.label = app.ui.language.tr(&it.label).to_string();
        match it.id.as_str() {
            "app.language.english" => it.checked = Some(app.ui.language == crate::i18n::Language::En),
            "app.language.japanese" => it.checked = Some(app.ui.language == crate::i18n::Language::Ja),
            "app.language.spanish" => it.checked = Some(app.ui.language == crate::i18n::Language::Es),
            "app.language.portuguese" => it.checked = Some(app.ui.language == crate::i18n::Language::PtBr),
            "app.language.ukrainian" => it.checked = Some(app.ui.language == crate::i18n::Language::Uk),
            "app.language.chinese" => it.checked = Some(app.ui.language == crate::i18n::Language::ZhCn),
            "app.language.german" => it.checked = Some(app.ui.language == crate::i18n::Language::De),
            "app.language.russian" => it.checked = Some(app.ui.language == crate::i18n::Language::Ru),
            "app.language.french" => it.checked = Some(app.ui.language == crate::i18n::Language::Fr),
            "app.language.indonesian" => it.checked = Some(app.ui.language == crate::i18n::Language::Id),
            "app.language.italian" => it.checked = Some(app.ui.language == crate::i18n::Language::It),
            "app.language.korean" => it.checked = Some(app.ui.language == crate::i18n::Language::Ko),
            _ => {}
        }
        if it.id.starts_with("view.") {
            it.checked = crate::panels::monitor_view::checked(app, &it.id).or(crate::panels::menu_dialogs::checked(app, &it.id));
            it.enabled &= crate::panels::monitor_view::enabled(app, &it.id);
        }
    }
    crate::panels::workspaces::menu(app, &mut v);
    open_recent(&mut v, &app.session, |n| app.ui.language.tr(n).to_string());
    v
}

/// File ▸ Open Recent, after Open Project…: the recent projects (General ▸ `recentProjects`),
/// newest first, then Clear Recent Files. Clear is always there, so the submenu exists (in the
/// native menu too, built once at launch) before the first project is saved.
fn open_recent(items: &mut Vec<MenuItem>, session: &filmcraft_engine::Session, tr: impl Fn(&str) -> String) {
    let Some(at) = items.iter().position(|it| it.id == "file.open") else { return };
    let path: Vec<String> = vec!["File".into(), "Open Recent".into()];
    let recent = &session.prefs.general.recent_projects;
    let mut sub: Vec<MenuItem> = recent
        .iter()
        .enumerate()
        .map(|(i, p)| MenuItem { id: format!("file.openRecent.{i}"), label: recent_name(p), path: path.clone(), shortcut: None, enabled: true, checked: None })
        .collect();
    sub.push(MenuItem { id: "file.clearRecent".into(), label: tr("Clear Recent Files"), path, shortcut: None, enabled: !recent.is_empty(), checked: None });
    items.splice(at + 1..at + 1, sub);
}

/// A recent project as listed in File ▸ Open Recent and on Home: its file name.
pub fn recent_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// The menu bar as it ships, without a running app: English labels, default preferences, the
/// built-in workspaces, nothing checked. Deterministic, so `cargo xtask parity` can measure it
/// (`examples/menu_tree.rs` prints it as JSON).
pub fn default_menu_items() -> Vec<MenuItem> {
    let session = filmcraft_engine::Session::default();
    let mut v = menu_items_for(&session);
    crate::panels::workspaces::default_menu(&mut v, &session);
    open_recent(&mut v, &session, str::to_string);
    v
}

/// Menu entries with their live shortcuts and enablement.
pub fn menu_items_for(session: &filmcraft_engine::Session) -> Vec<MenuItem> {
    let mut out = Vec::new();
    for c in filmcraft_engine::command_specs() {
        if c.menu.is_empty() {
            continue;
        }
        // Edit ▸ Label items carry the names from Settings ▸ Labels
        let label = match c.id.strip_prefix("edit.label.").and_then(filmcraft_project::Label::from_name) {
            Some(l) => session.prefs.labels.name(l),
            None => c.label.into(),
        };
        out.push(MenuItem {
            id: c.id.into(),
            label,
            path: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: session.shortcuts.primary(c.id),
            enabled: session.is_enabled(c.id),
            checked: None,
        });
    }
    for c in UI_COMMANDS.iter().chain(crate::panels::keyboard::COMMANDS) {
        if c.menu.is_empty() {
            continue;
        }
        out.push(MenuItem {
            id: c.id.into(),
            label: c.label.into(),
            path: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: session.shortcuts.primary(c.id),
            enabled: true,
            checked: None,
        });
    }
    for p in PanelKind::ALL {
        out.push(MenuItem {
            id: panel_command_id(p),
            label: p.title().into(),
            path: vec!["Window".into()],
            shortcut: session.shortcuts.primary(&panel_command_id(p)),
            enabled: true,
            checked: None,
        });
    }
    out
}

/// Shortcut text for menus in this OS's notation (`⇧⌘K` on macOS, `Ctrl+Shift+K` elsewhere).
pub fn shortcut_text(s: &str) -> String {
    use filmcraft_engine::shortcuts::{Chord, Platform};
    Chord::parse(s).map(|c| c.display(Platform::current())).unwrap_or_else(|_| s.to_string())
}

/// Parse "Cmd+Shift+K" into the modifiers a chord requires + its key. Off macOS the Control key
/// is the primary modifier, so `Ctrl` and `Cmd` are one key and both mean `command` (the engine's
/// `Chord::effective` says the same): a physical Ctrl press carries `command` there, and egui's
/// matching asks only for what the pattern names, so a `Ctrl+…` chord matches a `Cmd+…` binding
/// and the other way round (#245). On a Mac they stay two keys.
pub fn parse_shortcut(s: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let mut m = egui::Modifiers::NONE;
    let mut key = None;
    let parts: Vec<&str> = if s == "+" { vec!["+"] } else { s.split('+').collect() };
    let mac = cfg!(target_os = "macos");
    for p in parts {
        match p {
            "Cmd" => m.command = true,
            "Shift" => m.shift = true,
            "Alt" => m.alt = true,
            "Ctrl" if !mac => m.command = true,
            "Ctrl" => m.ctrl = true,
            k => {
                key = match k {
                    ";" => Some(egui::Key::Semicolon),
                    "'" => Some(egui::Key::Quote),
                    "," => Some(egui::Key::Comma),
                    "." => Some(egui::Key::Period),
                    "/" => Some(egui::Key::Slash),
                    "\\" => Some(egui::Key::Backslash),
                    "=" => Some(egui::Key::Equals),
                    "-" => Some(egui::Key::Minus),
                    "`" => Some(egui::Key::Backtick),
                    _ => egui::Key::from_name(k),
                }
            }
        }
    }
    key.map(|k| (m, k))
}

/// One active key binding for the input loop: (modifiers, key, command id, panel or None).
pub type KeyBinding = (egui::Modifiers, egui::Key, String, Option<String>);

/// The active key bindings (from the engine's shortcut set), most specific first so Shift+I
/// doesn't also fire I.
pub fn bindings(app: &FilmcraftApp) -> Vec<KeyBinding> {
    let mut v: Vec<KeyBinding> =
        app.session.shortcuts.bindings.iter().filter_map(|b| parse_shortcut(&b.keys).map(|(m, k)| (m, k, b.command.clone(), b.panel.clone()))).collect();
    // With Shift held, a punctuation key arrives as its shifted glyph (Shift+; is `:` on US layouts).
    let shifted: Vec<KeyBinding> =
        v.iter().filter(|b| b.0.shift).filter_map(|(m, k, id, p)| shifted_key(*k).map(|k2| (*m, k2, id.clone(), p.clone()))).collect();
    v.extend(shifted);
    // `+` is the `=` key with Shift on US layouts and its own key on others and the keypad: a chord
    // on `=` takes it too, unless something is bound to `+` itself. Otherwise Ctrl+Plus fell
    // through to egui's interface zoom while Ctrl+- and Ctrl+0 were taken by FilmCraft
    // shortcuts, so the interface grew and could not shrink back (#2253).
    let plus: Vec<KeyBinding> = v
        .iter()
        .filter(|(m, k, _, p)| *k == egui::Key::Equals && !v.iter().any(|(m2, k2, _, p2)| *k2 == egui::Key::Plus && m2 == m && p2 == p))
        .map(|(m, _, id, p)| (*m, egui::Key::Plus, id.clone(), p.clone()))
        .collect();
    v.extend(plus);
    v.sort_by_key(|(m, ..)| std::cmp::Reverse(specificity(*m)));
    v
}

/// How many modifiers a chord names: chords naming more are matched first, because egui ignores
/// extra Shift and Alt when matching a key press.
pub fn specificity(m: egui::Modifiers) -> u8 {
    m.command as u8 + m.shift as u8 + m.alt as u8 + m.ctrl as u8
}

/// The key a US layout reports for `k` with Shift held, when it differs.
fn shifted_key(k: egui::Key) -> Option<egui::Key> {
    use egui::Key;
    Some(match k {
        Key::Semicolon => Key::Colon,
        Key::Slash => Key::Questionmark,
        Key::Equals => Key::Plus,
        Key::Backslash => Key::Pipe,
        Key::Num1 => Key::Exclamationmark,
        Key::OpenBracket => Key::OpenCurlyBracket,
        Key::CloseBracket => Key::CloseCurlyBracket,
        _ => return None,
    })
}

/// Frontend-owned commands, registered with the engine's shortcut set so they can be listed,
/// rebound and resolved alongside engine commands (the `shortcuts.` commands).
pub fn external_commands() -> Vec<filmcraft_engine::shortcuts::CommandInfo> {
    use filmcraft_engine::shortcuts::CommandInfo;
    let mut v: Vec<CommandInfo> =
        UI_COMMANDS.iter().chain(crate::panels::keyboard::COMMANDS).map(|c| CommandInfo::new(c.id, c.label, c.menu, c.shortcut)).collect();
    for p in PanelKind::ALL {
        v.push(CommandInfo::new(&panel_command_id(p), p.title(), &["Window"], p.window_shortcut()));
    }
    v
}

/// Draw the in-window menu bar.
pub fn menu_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("interface-language"), app.ui.language));
    let items = menu_items(app);
    // Draw Edit ▸ Label with the user's live palette, not the baked-in label colours.
    let labels = app.session.prefs.labels.clone();
    let ctx = ui.ctx().clone();
    let mut clicked: Option<String> = None;
    // The bar's titles and their menus share one size and colour, as in a native menu bar.
    egui::MenuBar::new().style(crate::theme::menu_style).config(egui::containers::menu::MenuConfig::new().style(crate::theme::menu_style)).ui(ui, |ui| {
        let mut titles = Vec::with_capacity(MENUS.len());
        for top in MENUS {
            let mine: Vec<&MenuItem> = items.iter().filter(|i| i.path.first().map(String::as_str) == Some(top)).collect();
            let r = ui.menu_button(app.ui.language.tr(top), |ui| {
                ui.set_min_width(260.0);
                if mine.is_empty() {
                    ui.add_enabled(false, egui::Button::new(tl!("(empty)")));
                }
                menu_level(ui, &mine, 1, &mut clicked, &mut app.auto, &labels);
            });
            app.auto.add(&format!("menu.{top}"), r.response.rect, top);
            titles.push(r.response);
        }
        switch_on_hover(ui.ctx(), &titles);
    });
    if let Some(id) = clicked {
        let _ = invoke(app, &ctx, &id, json!({}));
    }
}

/// Like a native menu bar (Windows, macOS): while one top-level menu is open, hovering another
/// top-level title opens that menu instead (#1500). egui's `MenuBar` only switches on click.
fn switch_on_hover(ctx: &egui::Context, titles: &[egui::Response]) {
    let ids: Vec<egui::Id> = titles.iter().map(egui::Popup::default_response_id).collect();
    let Some(open) = ids.iter().position(|id| egui::Popup::is_id_open(ctx, *id)) else { return };
    // `hovered` is false where a popup covers the title (a tall submenu pushed up over the
    // bar), so the pointer inside an open menu never switches it.
    if let Some(i) = titles.iter().position(egui::Response::hovered)
        && i != open
    {
        egui::Popup::open_id(ctx, ids[i]);
        ctx.request_repaint();
    }
}

/// One menu level: items whose path ends here, and a submenu (at its first item's position) for
/// each deeper path segment, recursively (e.g. Clip ▸ Video Options ▸ Time Interpolation).
fn menu_level(
    ui: &mut egui::Ui,
    items: &[&MenuItem],
    depth: usize,
    clicked: &mut Option<String>,
    auto: &mut crate::automation::Registry,
    labels: &filmcraft_engine::settings::LabelPrefs,
) {
    let mut subs: Vec<&str> = Vec::new();
    for it in items {
        if let Some(sub) = it.path.get(depth).map(String::as_str) {
            if subs.contains(&sub) {
                continue;
            }
            subs.push(sub);
            let inner: Vec<&MenuItem> = items.iter().copied().filter(|x| x.path.get(depth).map(String::as_str) == Some(sub)).collect();
            let language = ui.ctx().data(|d| d.get_temp::<crate::i18n::Language>(egui::Id::unique("interface-language"))).unwrap_or_default();
            ui.menu_button(language.tr(sub), |ui| {
                ui.set_min_width(220.0);
                menu_level(ui, &inner, depth + 1, clicked, auto, labels);
            });
        } else if menu_entry(ui, it, auto, labels) {
            *clicked = Some(it.id.clone());
            ui.close();
        }
    }
}

/// Only Edit ▸ Label choices get a swatch. Other commands keep their usual menu layout.
fn label_menu_swatch(id: &str, labels: &filmcraft_engine::settings::LabelPrefs) -> Option<egui::Color32> {
    let label = id.strip_prefix("edit.label.").and_then(filmcraft_project::Label::from_name)?;
    Some(label_color(labels, label))
}

/// The user's colour for label `l` (Preferences ▸ Labels), else the built-in one.
pub fn label_color(labels: &filmcraft_engine::settings::LabelPrefs, l: filmcraft_project::Label) -> egui::Color32 {
    let [r, g, b] = labels.rgb(l);
    egui::Color32::from_rgb(r, g, b)
}

/// A label choice in a right-click Label menu (Project panel items, Timeline clips; #786), drawn
/// as Edit ▸ Label draws it: the user's name for the label with a swatch of its colour in the
/// left gutter. Registered for automation as `<prefix>.<Label>` (the label's built-in name).
pub fn label_choice(
    ui: &mut egui::Ui,
    auto: &mut crate::automation::Registry,
    labels: &filmcraft_engine::settings::LabelPrefs,
    l: filmcraft_project::Label,
    prefix: &str,
) -> egui::Response {
    let name = labels.name(l);
    let text = crate::i18n::t(&name);
    let r = ui.button(format!("      {text}"));
    auto.add(&format!("{prefix}.{}", l.name()), r.rect, text);
    paint_swatch(ui, r.rect, label_color(labels, l));
    r
}

/// A label swatch in the left gutter of the menu row `row`.
fn paint_swatch(ui: &egui::Ui, row: egui::Rect, color: egui::Color32) {
    let rect = egui::Rect::from_center_size(row.left_center() + egui::vec2(10.0, 0.0), egui::vec2(12.0, 12.0));
    ui.painter().rect_filled(rect, 2.0, color);
    ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color), egui::StrokeKind::Inside);
}

fn menu_entry(ui: &mut egui::Ui, it: &MenuItem, auto: &mut crate::automation::Registry, labels: &filmcraft_engine::settings::LabelPrefs) -> bool {
    // Checkmarks and label swatches each occupy the same left gutter.
    let swatch = label_menu_swatch(&it.id, labels);
    let label = if it.checked.is_some() || swatch.is_some() { format!("      {}", it.label) } else { it.label.clone() };
    let mut b = egui::Button::new(label);
    if let Some(s) = &it.shortcut {
        b = b.shortcut_text(shortcut_text(s));
    }
    let mut r = ui.add_enabled(it.enabled, b);
    // Help items say whether they open the web browser (and which address) or stay in FilmCraft
    if let Some(hint) = crate::links::menu_hint(&it.id) {
        r = r.on_hover_text(hint);
    }
    auto.add(&format!("menu.{}", it.id), r.rect, &it.label);
    if let Some(color) = swatch {
        paint_swatch(ui, r.rect, color);
    }
    if it.checked == Some(true) {
        let c = r.rect.left_center() + egui::vec2(10.0, 0.0);
        let col = ui.visuals().text_color();
        let st = egui::Stroke::new(1.5, col);
        ui.painter().line_segment([c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.0, 3.0)], st);
        ui.painter().line_segment([c + egui::vec2(-1.0, 3.0), c + egui::vec2(4.5, -3.5)], st);
    }
    r.clicked()
}

#[cfg(test)]
mod parse_shortcut_tests {
    use super::{label_menu_swatch, parse_shortcut};

    #[test]
    fn edit_label_menu_swatch_uses_current_custom_palette() {
        use filmcraft_engine::settings::{LabelPrefs, label_id};
        use filmcraft_project::Label;

        let mut labels = LabelPrefs::default();
        let label = Label::Iris;
        let key = label_id(label);
        labels.colors.get_mut(&key).unwrap().color = "#12ab34".into();

        assert_eq!(label_menu_swatch("edit.label.Iris", &labels), Some(egui::Color32::from_rgb(0x12, 0xab, 0x34)));
        assert_eq!(label_menu_swatch("edit.clear", &labels), None);
        assert_eq!(label_menu_swatch("edit.label.invalid", &labels), None);
    }

    /// Off macOS `Ctrl` and `Cmd` are the same key, so either spelling parses to the modifiers a
    /// physical Ctrl press carries and matches a binding written the other way (#245).
    #[test]
    fn ctrl_and_cmd_are_one_key_off_macos() {
        let (ctrl, k) = parse_shortcut("Ctrl+Z").unwrap();
        let (cmd, k2) = parse_shortcut("Cmd+Z").unwrap();
        assert_eq!((k, k2), (egui::Key::Z, egui::Key::Z));
        if cfg!(target_os = "macos") {
            assert_eq!((ctrl.ctrl, ctrl.command), (true, false));
            assert_eq!((cmd.ctrl, cmd.command), (false, true));
            assert!(!ctrl.matches_logically(cmd) && !cmd.matches_logically(ctrl), "two different keys on a Mac");
        } else {
            assert_eq!((ctrl.ctrl, ctrl.command), (false, true));
            assert_eq!(ctrl, cmd, "one key off a Mac");
            // what egui-winit reports for the physical key, and a bare `command` as tests send it
            let physical = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
            assert!(physical.matches_logically(cmd) && physical.matches_logically(ctrl));
            assert!(egui::Modifiers::COMMAND.matches_logically(ctrl));
        }
        assert!(!parse_shortcut("Z").unwrap().0.matches_logically(cmd), "a bare key never stands in for the chord");
        let (m, k) = parse_shortcut("Cmd+Shift+K").unwrap();
        assert!(m.command && m.shift && !m.alt && k == egui::Key::K);
        assert_eq!(parse_shortcut("+").map(|(_, k)| k), Some(egui::Key::Plus));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clipboard image is premultiplied inside `Color32`. The PNG writer expects straight
    /// alpha, so a semi-transparent pixel must come back with its colour, not darkened.
    #[test]
    fn pasted_images_keep_straight_alpha_colour() {
        // Red at 50% opacity: premultiplied storage is about (128, 0, 0, 128).
        let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 0, 0, 128]);
        let media = paste_image_media(&image).expect("a 1×1 clipboard image");
        let crate::ClipboardMedia::Image { width, height, rgba } = media else { panic!("not an image") };
        assert_eq!((width, height), (1, 1));
        assert_eq!(rgba, vec![255, 0, 0, 128], "straight RGBA, not the premultiplied bytes");
        let png = encode_png(width, height, rgba).expect("a PNG");
        let decoded = image::load_from_memory(&png).expect("decode the pasted PNG").to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 128]);
    }

    /// An image whose RGBA size exceeds the 128 MiB cap is rejected before any RGBA copy is allocated.
    #[test]
    fn oversized_clipboard_images_are_rejected_before_allocating_rgba() {
        let side = 100_000;
        let image = egui::ColorImage { size: [side, side], source_size: egui::vec2(100_000.0, 100_000.0), pixels: Vec::new() };
        let Err(error) = paste_image_media(&image) else { panic!("an oversized clipboard image was accepted") };
        assert!(error.contains("too large"), "{error}");
    }

    /// What `cargo xtask parity` measures: every item sits under a top-level menu, and Window ▸
    /// Workspaces lists the built-in workspaces before the layout commands, none checked.
    #[test]
    fn default_menu_items_are_the_shipped_menu_bar() {
        let items = default_menu_items();
        assert!(items.len() > 300, "{} items", items.len());
        assert!(items.iter().all(|it| it.path.first().is_some_and(|top| MENUS.contains(&top.as_str()))));
        let ws: Vec<&MenuItem> = items.iter().filter(|it| it.path == ["Window", "Workspaces"]).collect();
        for name in crate::dock::WORKSPACES {
            assert!(ws.iter().any(|it| it.label == name && it.checked.is_none()), "no workspace {name}");
        }
        let editing = ws.iter().position(|it| it.label == "Editing");
        let reset = ws.iter().position(|it| it.id == "window.workspace.reset");
        assert!(matches!((editing, reset), (Some(e), Some(r)) if e < r), "workspaces come before the layout commands");
        assert!(items.iter().any(|it| it.id == "file.save" && it.path == ["File"]));
    }

    /// #1500: like a native menu bar, while one menu is open hovering another title opens it.
    #[test]
    fn hovering_another_title_switches_the_open_menu() {
        use egui_kittest::{Harness, kittest::Queryable};

        let app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let mut harness = Harness::builder().with_size(egui::vec2(1200.0, 700.0)).build_ui_state(|ui, app| menu_bar(app, ui), app);
        // the app's fonts, which draw the macOS shortcut symbols (⇧⌘S) egui's default fonts lack
        crate::theme::install(&harness.ctx, &harness.state().tokens);
        harness.run_steps(3);
        // Nothing open: hovering a title does not open it.
        harness.get_by_label("Edit").hover();
        harness.run_steps(3);
        assert!(harness.query_by_label_contains("Save All").is_none() && harness.query_by_label_contains("Undo").is_none());
        harness.get_by_label("File").click();
        harness.run_steps(3);
        assert!(harness.query_by_label_contains("Save All").is_some(), "File menu did not open");
        harness.get_by_label("Edit").hover();
        harness.run_steps(4);
        assert!(harness.query_by_label_contains("Save All").is_none(), "File menu should close");
        assert!(harness.query_by_label_contains("Undo").is_some(), "Edit menu should open on hover");
    }
}
