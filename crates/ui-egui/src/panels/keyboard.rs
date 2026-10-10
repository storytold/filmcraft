//! Keyboard-only UI commands from Premiere's default keyboard (M3.12): frame maximize / full
//! screen, panel cycling, monitor zoom, track heights, Project panel and Text panel keyboard
//! navigation, play-to-Out variants. The project-changing keyboard commands are engine commands
//! (`filmcraft_engine::keyboard`); these only change what the UI shows.
//!
//! | Id | Premiere command | Default key (scope) |
//! |---|---|---|
//! | `window.maximizeFrame` / `window.maximizeFrameUnderCursor` | Maximize or Restore Active Frame / Frame Under Cursor | Shift+` / ` |
//! | `window.toggleFullScreen` | Toggle Full Screen | Ctrl+` |
//! | `window.nextPanel` / `window.prevPanel` | Select Next / Previous Panel | Ctrl+Shift+. / Ctrl+Shift+, |
//! | `window.toggleMonitorFocus` | Toggle Source/Program Monitor Focus | |
//! | `window.openSearch` / `panel.selectFindBox` | Open Search / Select Find Box | Cmd+Shift+F / Shift+F |
//! | `view.programZoom100` / `view.programZoomFit` | Zoom Program Monitor to 100% / Fit | Cmd+Shift+1 / Cmd+Shift+0 |
//! | `view.sourceZoom100` / `view.sourceZoomFit` | Zoom Source Monitor to 100% / Fit | Cmd+Alt+Shift+1 / Cmd+Alt+Shift+0 |
//! | `playback.inToOutPreroll` / `playback.toOut` | Play In to Out with Preroll/Postroll / Play from Playhead to Out Point | Shift+Space / Ctrl+Space |
//! | `timeline.expandAllTracks` / `timeline.minimizeAllTracks` | Expand / Minimize All Tracks | Shift+= / Shift+- |
//! | `timeline.increaseVideoHeight` … `timeline.decreaseAudioHeight` | Increase / Decrease Video / Audio Tracks Height | Cmd+= / Cmd+- / Alt+= / Alt+- (Timeline) |
//! | `timeline.nextScreen` / `timeline.prevScreen` | Show Next / Previous Screen | PageDown / PageUp (Timeline) |
//! | `timeline.playheadToCursor` | Move Playhead to Cursor (the frame under the pointer) | |
//! | `mixer.showHideTracks` / `mixer.meterInputOnly` | Audio Track Mixer ▸ Show/Hide Tracks…, Meter Input(s) Only | |
//! | `projectPanel.*` | Project panel: List / Icon / Toggle View, Hover Scrub, thumbnail size, Move / Extend Selection | (Project) |
//! | `textPanel.*` | Text panel transcript: word / line / segment navigation and selection, Delete (lift), Ripple Delete (extract; also the selected pause), Show Program Transcript, Find | (Text) |
//! | `effectControls.clear` | Clear in Effect Controls: the selected keyframes, else the selected effects (`panels::effect_controls::clear`) | Backspace, Delete (Effect Controls) |
//! | `graphics.beginTextEditing` | Begin Text Editing for a Graphic Layer | Cmd+Alt+' |
//! | `help.filmcraftHelp` | Help (Premiere Help…) | F1 |
//! | `app.quit` | Quit | Cmd+Q |

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::menus::UiCommand;
use filmcraft_engine::project_panel::ViewMode;

/// Keyboard-only view state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysState {
    /// The maximized frame (Maximize or Restore Frame); None = the normal layout.
    pub maximized: Option<PanelKind>,
    /// Track heights before Expand / Minimize All Tracks (pressing again restores them).
    pub saved_heights: Option<[f32; 2]>,
    /// Icon view columns in the last frame (Move Selection Up / Down step).
    pub icon_columns: usize,
}

impl Default for KeysState {
    fn default() -> Self {
        Self { maximized: None, saved_heights: None, icon_columns: 1 }
    }
}

macro_rules! uic {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr) => {
        UiCommand { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc }
    };
}

/// Frontend commands added for keyboard parity. Panel-scoped keys are in the shortcut tables
/// (`FILMCRAFT_PANEL`), so they have no application-wide default here.
pub const COMMANDS: &[UiCommand] = &[
    uic!("window.maximizeFrame", "Maximize or Restore Active Frame", [], Some("Shift+`")),
    uic!("window.maximizeFrameUnderCursor", "Maximize or Restore Frame Under Cursor", [], Some("`")),
    uic!("window.toggleFullScreen", "Toggle Full Screen", [], Some("Ctrl+`")),
    uic!("window.nextPanel", "Select Next Panel", [], Some("Ctrl+Shift+.")),
    uic!("window.prevPanel", "Select Previous Panel", [], Some("Ctrl+Shift+,")),
    uic!("window.toggleMonitorFocus", "Toggle Source/Program Monitor Focus", [], None),
    uic!("window.openSearch", "Open Search", [], Some("Cmd+Shift+F")),
    uic!("panel.selectFindBox", "Select Find Box", [], Some("Shift+F")),
    uic!("window.workspace.learning", "Learning", ["Window", "Workspaces"], Some("Alt+Shift+7")),
    uic!("window.workspace.review", "Review", ["Window", "Workspaces"], Some("Alt+Shift+8")),
    uic!("view.programZoom100", "Zoom Program Monitor to 100%", [], Some("Cmd+Shift+1")),
    uic!("view.programZoomFit", "Zoom Program Monitor to Fit", [], Some("Cmd+Shift+0")),
    uic!("view.sourceZoom100", "Zoom Source Monitor to 100%", [], Some("Cmd+Alt+Shift+1")),
    uic!("view.sourceZoomFit", "Zoom Source Monitor to Fit", [], Some("Cmd+Alt+Shift+0")),
    uic!("playback.inToOutPreroll", "Play In to Out with Preroll/Postroll", [], Some("Shift+Space")),
    uic!("playback.toOut", "Play from Playhead to Out Point", [], Some("Ctrl+Space")),
    uic!("timeline.expandAllTracks", "Expand All Tracks", [], Some("Shift+=")),
    uic!("timeline.minimizeAllTracks", "Minimize All Tracks", [], Some("Shift+-")),
    uic!("timeline.increaseVideoHeight", "Increase Video Tracks Height", [], None),
    uic!("timeline.decreaseVideoHeight", "Decrease Video Tracks Height", [], None),
    uic!("timeline.increaseAudioHeight", "Increase Audio Tracks Height", [], None),
    uic!("timeline.decreaseAudioHeight", "Decrease Audio Tracks Height", [], None),
    uic!("timeline.nextScreen", "Show Next Screen", [], None),
    uic!("timeline.prevScreen", "Show Previous Screen", [], None),
    uic!("timeline.playheadToCursor", "Move Playhead to Cursor", [], None),
    uic!("mixer.showHideTracks", "Show/Hide Tracks…", [], None),
    uic!("mixer.meterInputOnly", "Meter Input(s) Only", [], None),
    uic!("projectPanel.viewList", "List", [], None),
    uic!("projectPanel.viewIcon", "Icon", [], None),
    uic!("projectPanel.toggleView", "Toggle View", [], None),
    uic!("projectPanel.hoverScrub", "Hover Scrub", [], None),
    uic!("projectPanel.thumbnailLarger", "Thumbnail Size Next", [], None),
    uic!("projectPanel.thumbnailSmaller", "Thumbnail Size Previous", [], None),
    uic!("projectPanel.moveUp", "Move Selection Up", [], None),
    uic!("projectPanel.moveDown", "Move Selection Down", [], None),
    uic!("projectPanel.moveLeft", "Move Selection Left", [], None),
    uic!("projectPanel.moveRight", "Move Selection Right", [], None),
    uic!("projectPanel.moveHome", "Move Selection Home", [], None),
    uic!("projectPanel.moveEnd", "Move Selection End", [], None),
    uic!("projectPanel.movePageUp", "Move Selection Page Up", [], None),
    uic!("projectPanel.movePageDown", "Move Selection Page Down", [], None),
    uic!("projectPanel.extendUp", "Extend Selection Up", [], None),
    uic!("projectPanel.extendDown", "Extend Selection Down", [], None),
    uic!("projectPanel.extendLeft", "Extend Selection Left", [], None),
    uic!("projectPanel.extendRight", "Extend Selection Right", [], None),
    uic!("projectPanel.markIn", "Mark In (Hover Scrub)", [], None),
    uic!("projectPanel.markOut", "Mark Out (Hover Scrub)", [], None),
    uic!("projectPanel.rename", "Rename", [], None),
    uic!("projectPanel.openBin", "Open Bin", [], None),
    uic!("projectPanel.up", "Up One Level", [], None),
    uic!("projectPanel.closeBinTab", "Close Bin", [], None),
    uic!("projectPanel.metadataDisplay", "Metadata Display…", [], None),
    uic!("projectPanel.saveViewPresetAs", "Save As New View Preset", [], None),
    uic!("projectPanel.manageViewPresets", "Manage Saved View Presets", [], None),
    uic!("projectPanel.freeformOptions", "Freeform View Options…", [], None),
    uic!("projectPanel.saveArrangement", "Save Arrangement…", [], None),
    uic!("projectPanel.revealProject", "Reveal Project in Finder…", [], None),
    uic!("projectPanel.newPanel", "New Project Panel", ["Window"], None),
    uic!("textPanel.prevWord", "Navigate to Previous Word", [], None),
    uic!("textPanel.nextWord", "Navigate to Next Word", [], None),
    uic!("textPanel.selectPrevWord", "Select to Previous Word", [], None),
    uic!("textPanel.selectNextWord", "Select to Next Word", [], None),
    uic!("textPanel.prevLine", "Navigate to Previous Line", [], None),
    uic!("textPanel.nextLine", "Navigate to Next Line", [], None),
    uic!("textPanel.selectPrevLine", "Select to Previous Line", [], None),
    uic!("textPanel.selectNextLine", "Select to Next Line", [], None),
    uic!("textPanel.segmentStart", "Navigate to Start of Segment", [], None),
    uic!("textPanel.segmentEnd", "Navigate to End of Segment", [], None),
    uic!("textPanel.selectToSegmentStart", "Select to Segment Start", [], None),
    uic!("textPanel.selectToSegmentEnd", "Select to Segment End", [], None),
    uic!("textPanel.delete", "Delete", [], None),
    uic!("textPanel.rippleDelete", "Ripple Delete", [], None),
    uic!("textPanel.showProgramTranscript", "Show Program Transcript", [], None),
    uic!("effectControls.clear", "Clear (Effect Controls)", [], None),
    uic!("textPanel.find", "Find in Transcript", [], None),
    uic!("graphics.beginTextEditing", "Begin Text Editing for a Graphic Layer", [], Some("Cmd+Alt+'")),
    uic!("help.filmcraftHelp", "FilmCraft Help…", ["Help"], Some("F1")),
    uic!("app.quit", "Quit FilmCraft", [], Some("Cmd+Q")),
];

/// FilmCraft's documentation (Help ▸ FilmCraft Help…, F1).
pub const HELP_URL: &str = "https://github.com/storytold/filmcraft/tree/main/docs";

const HEIGHT_STEP: f32 = 12.0;
const MIN_TRACK_H: f32 = 24.0;
const MAX_TRACK_H: f32 = 240.0;

/// Run a keyboard UI command; None when `id` is not one of them.
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !COMMANDS.iter().any(|c| c.id == id) {
        return None;
    }
    let r = match id {
        "window.maximizeFrame" => Ok(toggle_maximize(app, app.ui.focused)),
        "window.maximizeFrameUnderCursor" => {
            let p =
                params.get("panel").and_then(Value::as_str).and_then(PanelKind::from_name).or_else(|| panel_under_pointer(app, ctx)).unwrap_or(app.ui.focused);
            Ok(toggle_maximize(app, p))
        }
        "window.toggleFullScreen" => {
            let fs = !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fs));
            Ok(json!({"fullScreen": fs}))
        }
        "window.nextPanel" | "window.prevPanel" => Ok(cycle_panel(app, id == "window.nextPanel")),
        "window.toggleMonitorFocus" => {
            let p = if app.ui.focused == PanelKind::Program { PanelKind::Source } else { PanelKind::Program };
            app.show_panel(p);
            Ok(json!({"focused": p.title()}))
        }
        "window.openSearch" => {
            app.show_panel(PanelKind::Project);
            Ok(focus_find_box(app, ctx))
        }
        "panel.selectFindBox" => Ok(focus_find_box(app, ctx)),
        "window.workspace.learning" | "window.workspace.review" => {
            let name = if id.ends_with("learning") { "Learning" } else { "Review" };
            app.set_workspace(name);
            Ok(json!({"workspace": name}))
        }
        "view.programZoom100" | "view.programZoomFit" | "view.sourceZoom100" | "view.sourceZoomFit" => {
            let v = if id.starts_with("view.program") { &mut app.ui.program } else { &mut app.ui.source };
            v.zoom = id.ends_with("100").then_some(1.0);
            v.pan = [0.0, 0.0];
            Ok(json!({"zoom": v.zoom}))
        }
        "playback.inToOutPreroll" => {
            if crate::menus::targets_source(app, params) {
                app.play_source_range(false, true).map(|_| Value::Null)
            } else {
                play_range(app, true)
            }
        }
        "playback.toOut" => {
            if crate::menus::targets_source(app, params) {
                app.play_source_range(true, false).map(|_| Value::Null)
            } else {
                play_range(app, false)
            }
        }
        "timeline.expandAllTracks" | "timeline.minimizeAllTracks" => Ok(all_heights(app, id == "timeline.expandAllTracks")),
        "timeline.increaseVideoHeight" | "timeline.decreaseVideoHeight" | "timeline.increaseAudioHeight" | "timeline.decreaseAudioHeight" => {
            let d = if id.contains("increase") { HEIGHT_STEP } else { -HEIGHT_STEP };
            let tv = &mut app.ui.timeline;
            let h = if id.contains("Video") { &mut tv.video_track_h } else { &mut tv.audio_track_h };
            *h = (*h + d).clamp(MIN_TRACK_H, MAX_TRACK_H);
            Ok(json!({"videoTrackHeight": tv.video_track_h, "audioTrackHeight": tv.audio_track_h}))
        }
        "timeline.nextScreen" | "timeline.prevScreen" => {
            let tv = &mut app.ui.timeline;
            let w = (app.last_timeline_width.max(200.0) - tv.header_w) as f64;
            let page = w / tv.pps.max(1e-6);
            let d = if id == "timeline.nextScreen" { page } else { -page };
            tv.target_scroll = (tv.target_scroll + d).max(0.0);
            Ok(json!({"scroll": tv.target_scroll}))
        }
        "timeline.playheadToCursor" => playhead_to_cursor(app, ctx, params),
        "mixer.showHideTracks" => {
            app.show_panel(PanelKind::AudioTrackMixer);
            crate::panels::mixer::open_show_hide(ctx);
            Ok(json!({"dialog": "showHideTracks"}))
        }
        "mixer.meterInputOnly" => {
            let on = params.get("enabled").and_then(Value::as_bool).unwrap_or(!app.ui.mixer_meter_input_only);
            app.ui.mixer_meter_input_only = on;
            Ok(json!({"meterInputOnly": on}))
        }
        "graphics.beginTextEditing" => begin_text_editing(app),
        "effectControls.clear" => crate::panels::effect_controls::clear(app),
        "help.filmcraftHelp" => {
            crate::links::open(ctx, HELP_URL);
            Ok(json!({"url": HELP_URL}))
        }
        "app.quit" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            Ok(Value::Null)
        }
        _ if id.starts_with("projectPanel.") => match crate::panels::project::route(app, ctx, &id["projectPanel.".len()..], params) {
            Some(r) => r,
            None => project_panel(app, &id["projectPanel.".len()..]),
        },
        // Find in the Text panel: its search field
        "textPanel.find" => {
            app.show_panel(PanelKind::Text);
            app.ui.focused = PanelKind::Text;
            Ok(focus_find_box(app, ctx))
        }
        _ if id.starts_with("textPanel.") => text_panel(app, &id["textPanel.".len()..]),
        _ => return None,
    };
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    Some(r)
}

/// Move Playhead to Cursor: the playhead jumps to the frame under the pointer in the Timeline
/// (`x`, a screen position, stands in for the pointer).
fn playhead_to_cursor(app: &mut FilmcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    const AWAY: &str = "move the pointer over the Timeline";
    let rate = app.session.active_sequence().ok_or("no sequence is open")?.settings.frame_rate;
    if !app.ui.dock.is_visible(PanelKind::Timeline) {
        return Err(AWAY.into());
    }
    let layout = app.tl.layout.as_ref().ok_or(AWAY)?;
    let x = match params.get("x").and_then(Value::as_f64) {
        Some(x) => x as f32,
        None => {
            let pos = ctx.input(|i| i.pointer.latest_pos()).ok_or(AWAY)?;
            let area = egui::Rect::from_min_max(egui::pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
            if !area.contains(pos) {
                return Err(AWAY.into());
            }
            pos.x
        }
    };
    if !x.is_finite() || x < layout.content.min.x || x > layout.content.max.x {
        return Err(AWAY.into());
    }
    let t = rate.snap_nearest(layout.tick_at(x).max(filmcraft_time::Tick::ZERO));
    app.stop();
    app.session.set_playhead(t);
    Ok(json!({"time": app.session.playhead().0}))
}

// ------------------------------------------------------------------ frames and panels

fn toggle_maximize(app: &mut FilmcraftApp, p: PanelKind) -> Value {
    app.ui.keys.maximized = if app.ui.keys.maximized.is_some() { None } else { Some(p) };
    if let Some(p) = app.ui.keys.maximized {
        app.ui.dock.activate(p);
        app.ui.focused = p;
    }
    json!({"maximized": app.ui.keys.maximized.map(|p| p.title())})
}

/// The visible panel under the pointer (from last frame's automation rects).
fn panel_under_pointer(app: &FilmcraftApp, ctx: &egui::Context) -> Option<PanelKind> {
    let pos = ctx.input(|i| i.pointer.latest_pos())?;
    app.auto.query("panel.").into_iter().find_map(|e| {
        let r = egui::Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3]));
        r.contains(pos).then(|| PanelKind::from_name(&e.id["panel.".len()..])).flatten()
    })
}

/// Visible panels in layout order (the active tab of each frame).
fn visible_panels(app: &FilmcraftApp) -> Vec<PanelKind> {
    let mut all = Vec::new();
    app.ui.dock.panels(&mut all);
    all.into_iter().filter(|p| app.ui.dock.is_visible(*p) && !p.compact()).collect()
}

fn cycle_panel(app: &mut FilmcraftApp, next: bool) -> Value {
    let v = visible_panels(app);
    if v.is_empty() {
        return Value::Null;
    }
    let i = v.iter().position(|p| *p == app.ui.focused).unwrap_or(0);
    let j = if next { (i + 1) % v.len() } else { (i + v.len() - 1) % v.len() };
    app.ui.focused = v[j];
    json!({"focused": v[j].title()})
}

/// Ask the focused panel's search field for keyboard focus (`widgets::search_field` takes it).
fn focus_find_box(app: &mut FilmcraftApp, ctx: &egui::Context) -> Value {
    let p = app.ui.focused;
    let rect =
        app.auto.find(&format!("panel.{}", p.id())).map(|e| egui::Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3])));
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(FOCUS_SEARCH), rect.unwrap_or(egui::Rect::EVERYTHING)));
    json!({"panel": p.title()})
}

/// egui temp-data key: the panel rect whose search field should take focus.
pub const FOCUS_SEARCH: &str = "fc-focus-search";

/// Called by search fields: take focus when Select Find Box targets the panel they sit in.
pub fn take_search_focus(ui: &egui::Ui, field: egui::Rect, resp: &egui::Response) {
    let id = egui::Id::new(FOCUS_SEARCH);
    let Some(panel) = ui.ctx().data(|d| d.get_temp::<egui::Rect>(id)) else { return };
    if panel.contains(field.center()) {
        resp.request_focus();
        ui.ctx().data_mut(|d| d.remove::<egui::Rect>(id));
    }
}

// ------------------------------------------------------------------ playback and timeline

fn play_range(app: &mut FilmcraftApp, preroll: bool) -> Result<Value, String> {
    let seq = app.session.active_sequence().ok_or("no sequence is open")?;
    let fd = seq.settings.frame_rate.frame_duration();
    let end = seq.mark_out.map(|o| o + fd).unwrap_or(seq.duration());
    let start = if preroll {
        let i = seq.mark_in.unwrap_or(filmcraft_time::Tick::ZERO);
        let pre = filmcraft_time::Tick::from_seconds_f64(app.session.prefs.playback.preroll_seconds);
        (i - pre).max(filmcraft_time::Tick::ZERO)
    } else {
        app.session.playhead()
    };
    let stop = if preroll { end + filmcraft_time::Tick::from_seconds_f64(app.session.prefs.playback.postroll_seconds) } else { end };
    app.session.set_playhead(start);
    app.play(1.0);
    app.playback.stop_at = Some(stop);
    Ok(json!({"from": start.0, "to": stop.0}))
}

fn all_heights(app: &mut FilmcraftApp, expand: bool) -> Value {
    let tv = &mut app.ui.timeline;
    let target = if expand { 120.0 } else { MIN_TRACK_H };
    let at_target = tv.video_track_h == target && tv.audio_track_h == target;
    match (at_target, app.ui.keys.saved_heights) {
        // pressing again restores the heights from before
        (true, Some([v, a])) => {
            tv.video_track_h = v;
            tv.audio_track_h = a;
            app.ui.keys.saved_heights = None;
        }
        _ => {
            app.ui.keys.saved_heights = Some([tv.video_track_h, tv.audio_track_h]);
            tv.video_track_h = target;
            tv.audio_track_h = target;
        }
    }
    json!({"videoTrackHeight": tv.video_track_h, "audioTrackHeight": tv.audio_track_h})
}

fn begin_text_editing(app: &mut FilmcraftApp) -> Result<Value, String> {
    let list = app.session.execute("graphics.list", json!({})).map_err(|e| e.to_string())?;
    let layers = list["layers"].as_array().cloned().unwrap_or_default();
    let is_text = |l: &Value| l["kind"] == json!("text");
    let pick = app
        .session
        .state
        .graphic_layers
        .first()
        .and_then(|i| layers.iter().find(|l| l["layer"].as_u64() == Some(*i as u64) && is_text(l)))
        .or_else(|| layers.iter().rev().find(|l| is_text(l)))
        .ok_or("the graphic has no text layer")?;
    let (clip, layer) = (list["clip"].as_u64().unwrap_or(0), pick["layer"].as_u64().unwrap_or(0) as usize);
    let len = pick["text"].as_str().map_or(0, str::len);
    app.session.execute("graphics.selectLayer", json!({"clip": clip, "layers": [layer]})).map_err(|e| e.to_string())?;
    // like Premiere, the whole text is selected so typing replaces it
    app.ui.gfx_edit = Some(crate::state::GfxEdit { clip, layer, caret: len, anchor: 0 });
    app.show_panel(PanelKind::Program);
    Ok(json!({"clip": clip, "layer": layer}))
}

// ------------------------------------------------------------------ Project panel

/// Items as the Project panel lists them (its shown tab, in display order).
fn panel_items(app: &FilmcraftApp) -> Vec<filmcraft_project::ItemId> {
    crate::panels::project::visible_items(app, crate::panels::project::shown_inst(app))
}

fn project_panel(app: &mut FilmcraftApp, op: &str) -> Result<Value, String> {
    match op {
        "viewList" | "viewIcon" | "toggleView" => {
            let inst = crate::panels::project::shown_inst(app);
            let cur = crate::panels::project::view_of(app, inst).mode;
            let mode = match op {
                "viewList" => ViewMode::List,
                "viewIcon" => ViewMode::Icon,
                _ if cur == ViewMode::List => ViewMode::Icon,
                _ => ViewMode::List,
            };
            let mut acts = Vec::new();
            crate::panels::project::set_mode(app, inst, mode, &mut acts);
            for (c, p) in acts {
                app.session.execute(&c, p).map_err(|e| e.to_string())?;
            }
            return Ok(json!({"view": mode}));
        }
        "hoverScrub" => {
            let on = !app.session.prefs.project_panel.hover_scrub;
            app.session.execute("project.view.set", json!({"hoverScrub": on})).map_err(|e| e.to_string())?;
            return Ok(json!({"hoverScrub": on}));
        }
        "thumbnailLarger" | "thumbnailSmaller" => {
            let f = if op == "thumbnailLarger" { 1.25 } else { 0.8 };
            let inst = crate::panels::project::shown_inst(app);
            let v = crate::panels::project::view_of(app, inst);
            let size = (v.icon_size * f).clamp(48.0, 400.0);
            match inst {
                crate::panels::project::Inst::Main => {
                    app.session.execute("project.view.set", json!({"iconSize": size})).map_err(|e| e.to_string())?;
                }
                crate::panels::project::Inst::Tab(k) => app.ui.project_panel.tabs[k].icon_size = size,
            }
            return Ok(json!({"iconSize": size}));
        }
        _ => {}
    }
    let items = panel_items(app);
    if items.is_empty() {
        return Ok(json!({"items": []}));
    }
    let sel = app.session.state.project_selection.clone();
    let last = sel.last().and_then(|s| items.iter().position(|i| i == s));
    let icon = crate::panels::project::view_of(app, crate::panels::project::shown_inst(app)).mode != ViewMode::List;
    let cols = if icon { app.ui.keys.icon_columns.max(1) as i64 } else { 1 };
    let page = 10i64;
    let n = items.len() as i64;
    let (step, extend): (Option<i64>, bool) = match op {
        "moveUp" => (Some(-cols), false),
        "moveDown" => (Some(cols), false),
        "moveLeft" => (Some(if icon { -1 } else { 0 }), false),
        "moveRight" => (Some(if icon { 1 } else { 0 }), false),
        "movePageUp" => (Some(-page * cols), false),
        "movePageDown" => (Some(page * cols), false),
        "extendUp" => (Some(-cols), true),
        "extendDown" => (Some(cols), true),
        "extendLeft" => (Some(if icon { -1 } else { 0 }), true),
        "extendRight" => (Some(if icon { 1 } else { 0 }), true),
        "moveHome" | "moveEnd" => (None, false),
        _ => return Err(format!("unknown Project panel command `{op}`")),
    };
    let target = match (step, last) {
        (None, _) => {
            if op == "moveHome" {
                0
            } else {
                n - 1
            }
        }
        (Some(d), Some(i)) => (i as i64 + d).clamp(0, n - 1),
        (Some(d), None) => {
            if d < 0 {
                n - 1
            } else {
                0
            }
        }
    } as usize;
    let mut new_sel: Vec<filmcraft_project::ItemId> = if extend {
        let anchor = sel.first().and_then(|s| items.iter().position(|i| i == s)).unwrap_or(target);
        let (a, b) = (anchor.min(target), anchor.max(target));
        let mut v: Vec<_> = items[a..=b].to_vec();
        // keep the anchor first and the moving end last
        if target < anchor {
            v.reverse();
        }
        v
    } else {
        vec![items[target]]
    };
    new_sel.dedup();
    let ids: Vec<u64> = new_sel.iter().map(|i| i.0).collect();
    app.session.execute("project.select", json!({"items": ids})).map_err(|e| e.to_string())?;
    Ok(json!({"items": ids}))
}

// ------------------------------------------------------------------ Text panel (transcript)

fn text_panel(app: &mut FilmcraftApp, op: &str) -> Result<Value, String> {
    if op == "showProgramTranscript" {
        app.show_panel(PanelKind::Text);
        app.ui.text_tab = "Transcript".into();
        return Ok(json!({"tab": "Transcript"}));
    }
    if app.ui.text_tab != "Transcript" {
        // the Captions tab: Delete / Ripple Delete act on the selected captions
        return match op {
            "delete" => app.session.execute("edit.clear", json!({})).map_err(|e| e.to_string()),
            "rippleDelete" => app.session.execute("edit.rippleDelete", json!({})).map_err(|e| e.to_string()),
            _ => Ok(Value::Null),
        };
    }
    let words = filmcraft_engine::transcript::sequence_words(&app.session);
    if words.is_empty() {
        return Err("the sequence has no transcript".into());
    }
    let n = words.len();
    let paras = filmcraft_edit::transcript::paragraphs(&words, filmcraft_time::Tick::from_seconds_f64(1.5));
    let para_of = |i: usize| paras.iter().position(|p| p.contains(&i)).unwrap_or(0);
    // a selection left over from a longer transcript (e.g. after `transcript.extract` over the control channel) counts as none, as when drawing
    // without a selection, from the text cursor (a click, the last move), else the playhead's word
    let (anchor, cur) = app.ui.transcript_sel.filter(|(a, b)| *a < n && *b < n).unwrap_or_else(|| {
        let i = app.ui.transcript.caret.filter(|c| *c < n).or_else(|| filmcraft_edit::transcript::word_at(&words, app.session.playhead())).unwrap_or(0);
        (i, i)
    });
    let line = |i: usize, d: i64| -> usize {
        let p = para_of(i) as i64 + d;
        if p < 0 { 0 } else { paras.get(p as usize).map_or(n - 1, |r| r.start) }
    };
    let (to, extend) = match op {
        "prevWord" => (cur.saturating_sub(1), false),
        "nextWord" => ((cur + 1).min(n - 1), false),
        "selectPrevWord" => (cur.saturating_sub(1), true),
        "selectNextWord" => ((cur + 1).min(n - 1), true),
        "prevLine" => (line(cur, -1), false),
        "nextLine" => (line(cur, 1), false),
        "selectPrevLine" => (line(cur, -1), true),
        "selectNextLine" => (line(cur, 1), true),
        "segmentStart" => (paras[para_of(cur)].start, false),
        "segmentEnd" => (paras[para_of(cur)].end - 1, false),
        "selectToSegmentStart" => (paras[para_of(cur)].start, true),
        "selectToSegmentEnd" => (paras[para_of(cur)].end - 1, true),
        "delete" | "rippleDelete" => {
            // the selected text, or the selected pause
            let Some(params) = crate::panels::transcript::selection_params(app) else { return Err("select text in the transcript".into()) };
            let cmd = if op == "delete" { "transcript.lift" } else { "transcript.extract" };
            let r = app.session.execute(cmd, params).map_err(|e| e.to_string())?;
            app.ui.transcript_sel = None;
            app.ui.transcript.pause = None;
            return Ok(r);
        }
        _ => return Err(format!("unknown Text panel command `{op}`")),
    };
    // moving the cursor drops the selection; Shift extends it (and marks In/Out like a drag)
    app.ui.transcript_sel = extend.then_some((anchor, to));
    app.ui.transcript.pause = None;
    app.ui.transcript.caret = Some(to);
    if extend && app.session.prefs.transcript.auto_in_out {
        app.session.execute("transcript.select", json!({"from": anchor.min(to), "to": anchor.max(to)})).map_err(|e| e.to_string())?;
    }
    // the playhead follows the cursor
    app.session.set_playhead(words[to].start);
    Ok(json!({"selection": extend.then(|| [anchor.min(to), anchor.max(to)]), "word": to}))
}

#[cfg(test)]
mod transcript_selection_tests {
    use serde_json::{Value, json};

    #[test]
    fn prev_word_after_the_transcript_shrank_under_the_selection_does_not_panic() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let a = session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
        let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
        let words: Vec<Value> = ["one", "two", "three", "four"]
            .iter()
            .enumerate()
            .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5)}))
            .collect();
        session.execute("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}})).unwrap();
        let mut app = crate::FilmcraftApp::new(session);
        app.ui.text_tab = "Transcript".into();
        app.ui.transcript_sel = Some((3, 3));
        // the transcript shrinks behind the panel's back, as with `transcript.extract` over the control channel
        app.session.execute("transcript.extract", json!({"from": 0, "to": 1})).unwrap();
        assert_eq!(filmcraft_engine::transcript::sequence_words(&app.session).len(), 2);
        let r = super::text_panel(&mut app, "prevWord").unwrap();
        let caret = app.ui.transcript.caret.unwrap();
        assert!(caret < 2, "cursor {caret} outside the 2-word transcript");
        assert_eq!(r["word"], json!(caret));
        assert_eq!(app.ui.transcript_sel, None, "moving the cursor drops the selection");
        // a cursor left behind is checked the same way
        app.ui.transcript.caret = Some(7);
        let r = super::text_panel(&mut app, "selectNextWord").unwrap();
        let (sa, sb) = app.ui.transcript_sel.unwrap();
        assert!(sa < 2 && sb < 2, "selection {sa}..{sb} outside the 2-word transcript: {r}");
    }
}
