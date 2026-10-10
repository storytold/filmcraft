//! The Trim Monitor: while edit points are selected (trim mode) the Program monitor shows the
//! outgoing clip's last frame and the incoming clip's first frame side by side, with their
//! timecodes, the Out/In shift counters and the trim buttons (−N, −1, Apply Default Transitions,
//! +1, +N; N = Preferences ▸ Trim ▸ Large Trim Offset). Dragging in the outgoing picture trims the
//! outgoing side, in the incoming picture the incoming side, between them rolls.
//!
//! J/K/L and Space are routed to dynamic trimming / loop playback in `menus::invoke`; the engine
//! state machine (`filmcraft_engine::trim`) is advanced here once per frame with the UI clock.
//!
//! Automation ids: `trimMonitor.outgoing`, `trimMonitor.incoming`, `trimMonitor.roll`,
//! `trimMonitor.outTimecode`, `trimMonitor.inTimecode`, `trimMonitor.outShift`,
//! `trimMonitor.inShift`, `trimMonitor.backwardMany`, `trimMonitor.backward`,
//! `trimMonitor.applyTransition`, `trimMonitor.forward`, `trimMonitor.forwardMany`,
//! `trimMonitor.playAround`, `trimMonitor.exit`.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_edit::Edge;
use filmcraft_project::{ClipId, ItemKind, Sequence, Track, TrackId};
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::panels::timeline::{Layout, Row};
use crate::panels::timeline_hit::EdgeKind;
use crate::theme::Tokens;

/// Ripple edit points are red, roll / regular trims yellow (as in Premiere's timeline).
pub const RIPPLE_RED: Color32 = Color32::from_rgb(0xe5, 0x3e, 0x3e);
pub const ROLL_YELLOW: Color32 = Color32::from_rgb(0xf2, 0xc4, 0x3d);

/// True when J/K/L/Space should drive trim mode instead of normal playback.
pub fn trim_mode(app: &FilmcraftApp) -> bool {
    !app.session.state.edit_points.is_empty() && app.ui.focused != crate::dock::PanelKind::Source
}

/// Advance dynamic trimming / loop playback to the current UI time (once per frame).
pub fn advance(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.session.trim_play.active() {
        return;
    }
    let clock = ctx.input(|i| i.time);
    if let Err(e) = app.session.execute("trim.tick", json!({"clock": clock})) {
        app.ui.status = e.to_string();
    }
    ctx.request_repaint();
}

/// Route a transport command in trim mode (J/K/L/Space/Shift+K). `None` = not handled.
pub fn route_transport(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str) -> Option<Result<Value, String>> {
    let clock = ctx.input(|i| i.time);
    let trimming = trim_mode(app);
    let (cmd, params) = match id {
        "playback.forward" if trimming => ("trim.shuttle", json!({"direction": "forward", "clock": clock})),
        "playback.reverse" if trimming => ("trim.shuttle", json!({"direction": "reverse", "clock": clock})),
        "playback.slowForward" if trimming => ("trim.shuttle", json!({"direction": "forward", "slow": true, "clock": clock})),
        "playback.slowReverse" if trimming => ("trim.shuttle", json!({"direction": "reverse", "slow": true, "clock": clock})),
        "playback.stop" if app.session.trim_play.active() => ("trim.shuttleStop", json!({"clock": clock})),
        "playback.toggle" if trimming || app.session.trim_play.around.is_some() => ("trim.playAround", json!({"clock": clock, "toggle": true})),
        "playback.playAround" => ("trim.playAround", json!({"clock": clock, "loop": false})),
        _ => return None,
    };
    app.stop();
    Some(app.session.execute(cmd, params).map_err(|e| e.to_string()))
}

/// Paint the selected edit points that sit on `tr` as brackets at the clip edges.
pub fn paint_edit_points(app: &FilmcraftApp, p: &egui::Painter, seq: &Sequence, tr: &Track, row: &Row, layout: &Layout) {
    use filmcraft_engine::trim::{TrimKind, roll_pair};
    for ep in &app.session.state.edit_points {
        let mut marks = Vec::new(); // (clip on this track, out edge)
        if ep.kind == TrimKind::Roll
            && let Some((l, r)) = roll_pair(seq, ep)
        {
            marks.push((l, true));
            marks.push((r, false));
        } else {
            marks.push((ep.clip, ep.out));
        }
        let col = if ep.kind == TrimKind::Ripple { RIPPLE_RED } else { ROLL_YELLOW };
        for (clip, out) in marks {
            let Some(it) = tr.item(clip) else { continue };
            paint_bracket(p, layout.x_of(if out { it.end() } else { it.start }), row, out, col);
        }
    }
}

/// A trim bracket at `x` on `row`, its arms pointing into the clip (left for an Out edge).
pub fn paint_bracket(p: &egui::Painter, x: f32, row: &Row, out: bool, col: Color32) {
    let (y0, y1) = (row.rect.min.y + 2.0, row.rect.max.y - 2.0);
    let dir = if out { -1.0 } else { 1.0 };
    let xi = x + dir * 1.5;
    let w = 6.0 * dir;
    let s = Stroke::new(3.0, col);
    p.line_segment([pos2(xi, y0), pos2(xi, y1)], s);
    p.line_segment([pos2(xi, y0 + 1.0), pos2(xi + w, y0 + 1.0)], s);
    p.line_segment([pos2(xi, y1 - 1.0), pos2(xi + w, y1 - 1.0)], s);
}

/// The bracket on the edge a press would grab (#259): an edit point's bracket at half strength,
/// red for a ripple and yellow otherwise; a roll marks both sides of the cut.
pub fn paint_hover_bracket(p: &egui::Painter, seq: &Sequence, layout: &Layout, track: TrackId, clip: ClipId, edge: Edge, kind: EdgeKind) {
    let Some(row) = layout.rows.iter().find(|r| r.track == track) else { return };
    let Some(tr) = seq.track(track) else { return };
    let full = if kind == EdgeKind::Ripple { RIPPLE_RED } else { ROLL_YELLOW };
    let col = full.gamma_multiply(0.5);
    let marks = match kind {
        EdgeKind::Roll { left, right } => vec![(left, true), (right, false)],
        _ => vec![(clip, edge == Edge::Out)],
    };
    for (c, out) in marks {
        let Some(it) = tr.item(c) else { continue };
        paint_bracket(p, layout.x_of(if out { it.end() } else { it.start }), row, out, col);
    }
}

/// Fit a w×h picture into `area`.
fn fit(area: Rect, w: f32, h: f32) -> Rect {
    let s = (area.width() / w.max(1.0)).min(area.height() / h.max(1.0));
    Rect::from_center_size(area.center(), vec2(w * s, h * s))
}

/// Draw one side's frame (the clip's media at `media_time`).
fn picture(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect, side: &Value, tex_name: &str) -> Rect {
    let t = app.tokens;
    let item = filmcraft_project::ItemId(side["item"].as_u64().unwrap_or(0));
    let Some(pi) = app.session.project.item(item) else { return area };
    let size = match &pi.kind {
        ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0)),
        ItemKind::Sequence(s) => (s.settings.width, s.settings.height),
        _ => (1920, 1080),
    };
    let seq_size = app.session.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
    let pic = fit(area, seq_size.0 as f32, seq_size.1 as f32);
    ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
    if size.0 == 0 || !side["video"].as_bool().unwrap_or(false) {
        ui.painter().text(pic.center(), Align2::CENTER_CENTER, format!("♪  {}", side["name"].as_str().unwrap_or("")), Tokens::ui(13.0), t.text_dim);
        return pic;
    }
    let img_rect = fit(pic, size.0 as f32, size.1 as f32);
    let rate = pi.frame_rate();
    let media_time = Tick(side["mediaTime"].as_i64().unwrap_or(0));
    let frame = rate.frame_at(media_time);
    let ppp = ui.ctx().pixels_per_point();
    let want = (img_rect.width() * ppp / size.0 as f32).clamp(1.0 / 32.0, 1.0);
    let buckets = [1.0 / 32.0, 1.0 / 16.0, 1.0 / 8.0, 0.1875, 0.25, 0.375, 0.5, 0.75, 1.0];
    let scale = *buckets.iter().find(|b| **b >= want - 1e-4).unwrap_or(&1.0);
    let scale = scale.min(app.ui.program.res.scale());
    let size_key = (scale * 1000.0) as u32;
    let rev = app.item_revision(item);
    let key = FrameKey { target: Target::Item(item), frame, size: size_key, revision: rev, draft: false };
    let project = app.session.project.clone();
    app.frames.request(key, rate.tick_of(frame), scale, &project, 0);
    let ctx = ui.ctx().clone();
    let tex = if let Some(img) = app.frames.get(&key) {
        Some(app.texture_for(&ctx, tex_name, key, &img))
    } else {
        match app.frames.nearest(Target::Item(item), frame, size_key, rev, 48) {
            Some(img) => Some(app.texture_for(&ctx, tex_name, FrameKey { frame: frame - 1, ..key }, &img)),
            None => app.texture_existing(tex_name).map(|(id, _)| id),
        }
    };
    if let Some(tex) = tex {
        ui.painter().image(tex, img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    pic
}

fn shift_text(frames: i64) -> String {
    if frames > 0 { format!("+{frames}") } else { frames.to_string() }
}

/// The two-up Trim Monitor in the Program monitor's `rect`.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let info = filmcraft_engine::trim::monitor_info(&app.session);
    let Some(seq) = app.session.active_sequence() else { return };
    let rate = seq.settings.frame_rate;
    let df = seq.settings.drop_frame;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let controls_h = 26.0 + 40.0;
    let header_h = 22.0;
    let gap = 14.0;
    let area = Rect::from_min_max(rect.min + vec2(6.0, 4.0 + header_h), pos2(rect.max.x - 6.0, rect.max.y - controls_h));
    let half = (area.width() - gap) / 2.0;
    let left = Rect::from_min_size(area.min, vec2(half, area.height()));
    let right = Rect::from_min_size(pos2(left.max.x + gap, area.min.y), vec2(half, area.height()));
    let kind = info["kind"].as_str().unwrap_or("trim").to_string();
    let out_edge = info["edge"] == json!("out");
    // which sides move: roll = both; otherwise the side the edit point belongs to
    let (hot_out, hot_in) = match kind.as_str() {
        "roll" => (true, true),
        _ => (out_edge, !out_edge),
    };
    let col = if kind == "ripple" { RIPPLE_RED } else { ROLL_YELLOW };
    // headers: clip names
    for (side, r, key) in [(&info["outgoing"], left, "outgoing"), (&info["incoming"], right, "incoming")] {
        let name = side["name"].as_str().map(|n| format!("{} ({})", n, side["track"].as_str().unwrap_or(""))).unwrap_or_else(|| tl!("(gap)").into());
        let label = if key == "outgoing" { tlf!("Out: {name}", name) } else { tlf!("In: {name}", name) };
        ui.painter().text(pos2(r.min.x + 2.0, rect.min.y + 4.0 + header_h / 2.0), Align2::LEFT_CENTER, label, Tokens::ui(11.5), t.text_dim);
    }
    let mut pics = [left, right];
    for (i, (side, r)) in [(&info["outgoing"], left), (&info["incoming"], right)].into_iter().enumerate() {
        if side.is_null() {
            let p = fit(r, 16.0, 9.0);
            ui.painter().rect_filled(p, 0.0, Color32::BLACK);
            ui.painter().text(p.center(), Align2::CENTER_CENTER, tl!("(gap)"), Tokens::ui(12.0), t.text_faint);
            pics[i] = p;
        } else {
            pics[i] = picture(app, ui, r, side, if i == 0 { "trim-outgoing" } else { "trim-incoming" });
        }
    }
    let [lp, rp] = pics;
    let hot = [hot_out, hot_in];
    for (i, p) in pics.iter().enumerate() {
        if hot[i] {
            ui.painter().rect_stroke(*p, 0.0, Stroke::new(2.0, col), StrokeKind::Outside);
        }
    }
    // dynamic trim indicator
    if let Some(d) = info["dynamic"].as_object() {
        let sp = d.get("speed").and_then(Value::as_f64).unwrap_or(0.0);
        let txt = if sp == 0.0 {
            "TRIM ■".to_string()
        } else if sp > 0.0 {
            format!("TRIM ▶ {sp}×")
        } else {
            format!("TRIM ◀ {}×", -sp)
        };
        let at = pos2((lp.max.x + rp.min.x) / 2.0, area.min.y + 12.0);
        ui.painter().text(at, Align2::CENTER_CENTER, txt, Tokens::semibold(11.0), col);
    }
    app.auto.add("trimMonitor.outgoing", lp, "Outgoing clip (drag to trim the Out point)");
    app.auto.add("trimMonitor.incoming", rp, "Incoming clip (drag to trim the In point)");
    let mid = Rect::from_min_max(pos2(lp.max.x, area.min.y), pos2(rp.min.x, area.max.y));
    app.auto.add("trimMonitor.roll", mid, "Drag to roll the edit");

    // drag to trim: px → frames (6 px per frame), applied on release as one trim
    let fpp = 1.0 / 6.0;
    for (r, which) in [(lp, "out"), (rp, "in"), (mid, "roll")] {
        let resp = ui.interact(r, egui::Id::new(("trim-monitor-drag", which)), Sense::drag());
        if resp.dragged() || resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
        }
        if resp.dragged() {
            // read the delta before locking: drag_delta() takes the context's write lock,
            // the same one data_mut() holds, and that lock is not reentrant (deadlock)
            let dx = resp.drag_delta().x;
            let total = ui.ctx().data_mut(|d| {
                let v = d.get_temp_mut_or_default::<f32>(egui::Id::new("trim-monitor-dx"));
                *v += dx;
                *v
            });
            let n = (total * fpp).round() as i64;
            ui.painter().text(pos2(r.center().x, r.max.y - 14.0), Align2::CENTER_CENTER, shift_text(n), Tokens::semibold(14.0), col);
        }
        if resp.drag_stopped() {
            let total = ui.ctx().data_mut(|d| d.remove_temp::<f32>(egui::Id::new("trim-monitor-dx")).unwrap_or(0.0));
            let n = (total * fpp).round() as i64;
            if n != 0 {
                drag_trim(app, which, n);
            }
        }
    }

    // ---- counters row: outgoing Out timecode + shift | incoming In timecode + shift
    let row = Rect::from_min_size(pos2(rect.min.x + 10.0, area.max.y + 2.0), vec2(rect.width() - 20.0, 24.0));
    let tc = |v: &Value, k: &str| v[k].as_i64().map(|x| format_time(Tick(x), rate, df, TimeDisplay::Timecode, 48000)).unwrap_or_else(|| "--:--:--:--".into());
    let out_tc = tc(&info["outgoing"], "time");
    let in_tc = tc(&info["incoming"], "time");
    let out_shift = shift_text(info["outShift"].as_i64().unwrap_or(0));
    let in_shift = shift_text(info["inShift"].as_i64().unwrap_or(0));
    let p = ui.painter();
    let tcr = p.text(pos2(lp.min.x, row.center().y), Align2::LEFT_CENTER, &out_tc, Tokens::semibold(14.0), t.hot_text);
    app.auto.add("trimMonitor.outTimecode", tcr, &out_tc);
    let sr = p.text(pos2(lp.max.x, row.center().y), Align2::RIGHT_CENTER, &out_shift, Tokens::semibold(14.0), if hot_out { col } else { t.text_dim });
    app.auto.add("trimMonitor.outShift", sr, &out_shift);
    let sr = p.text(pos2(rp.min.x, row.center().y), Align2::LEFT_CENTER, &in_shift, Tokens::semibold(14.0), if hot_in { col } else { t.text_dim });
    app.auto.add("trimMonitor.inShift", sr, &in_shift);
    let tcr = p.text(pos2(rp.max.x, row.center().y), Align2::RIGHT_CENTER, &in_tc, Tokens::semibold(14.0), t.hot_text);
    app.auto.add("trimMonitor.inTimecode", tcr, &in_tc);

    // ---- trim buttons
    let n = info["largeTrimOffset"].as_i64().unwrap_or(5);
    let bar = Rect::from_min_size(pos2(rect.min.x, row.max.y + 6.0), vec2(rect.width(), 28.0));
    let buttons: [(&str, String, &str, f32); 6] = [
        ("trimMonitor.playAround", String::new(), tl!("Play Around Edit (Space loops, Shift+K once)"), 30.0),
        ("trimMonitor.backwardMany", format!("−{n}"), tl!("Trim Backward Many"), 40.0),
        ("trimMonitor.backward", "−1".into(), tl!("Trim Backward"), 40.0),
        ("trimMonitor.applyTransition", tl!("Apply Default Transitions").into(), tl!("Apply Default Transitions to Selection (Shift+D)"), 176.0),
        ("trimMonitor.forward", "+1".into(), tl!("Trim Forward"), 40.0),
        ("trimMonitor.forwardMany", format!("+{n}"), tl!("Trim Forward Many"), 40.0),
    ];
    let total: f32 = buttons.iter().map(|b| b.3 + 4.0).sum();
    let mut x = bar.center().x - total / 2.0;
    let ctx = ui.ctx().clone();
    for (id, label, tip, w) in buttons {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(w, bar.height()));
        let resp = ui.interact(r, egui::Id::new(id), Sense::click()).on_hover_text(tip);
        let bg = if resp.is_pointer_button_down_on() {
            t.pressed
        } else if resp.hovered() {
            t.hover
        } else {
            t.field_bg
        };
        ui.painter().rect_filled(r, 4.0, bg);
        if label.is_empty() {
            let playing = app.session.trim_play.around.is_some();
            icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(14.0, 14.0)), if playing { Icon::Pause } else { Icon::Loop }, t.icon);
        } else {
            ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, Tokens::ui(12.0), t.text);
        }
        app.auto.add(id, r, tip);
        if resp.clicked() {
            let res = match id {
                "trimMonitor.playAround" => app.session.execute("trim.playAround", json!({"clock": ctx.input(|i| i.time), "toggle": true})),
                "trimMonitor.backwardMany" => app.session.execute("trim.backwardMany", json!({})),
                "trimMonitor.backward" => app.session.execute("trim.backward", json!({})),
                "trimMonitor.forward" => app.session.execute("trim.forward", json!({})),
                "trimMonitor.forwardMany" => app.session.execute("trim.forwardMany", json!({})),
                _ => app.session.execute("trim.applyDefaultTransition", json!({})),
            };
            if let Err(e) = res {
                app.ui.status = e.to_string();
            }
        }
        x += w + 4.0;
    }
    // exit trim mode
    let r = Rect::from_min_size(pos2(rect.max.x - 30.0, bar.min.y + 3.0), vec2(22.0, 22.0));
    let resp = ui.interact(r, egui::Id::new("trimMonitor.exit"), Sense::click()).on_hover_text(tl!("Exit Trim Mode"));
    let c = if resp.hovered() { t.tab_text_active } else { t.text_dim };
    ui.painter().line_segment([r.min + vec2(6.0, 6.0), r.max - vec2(6.0, 6.0)], Stroke::new(1.5, c));
    ui.painter().line_segment([pos2(r.max.x - 6.0, r.min.y + 6.0), pos2(r.min.x + 6.0, r.max.y - 6.0)], Stroke::new(1.5, c));
    app.auto.add("trimMonitor.exit", r, "Exit Trim Mode");
    if resp.clicked() {
        let _ = app.session.execute("trim.clear", json!({}));
    }
}

/// A drag in the Trim Monitor: re-target the primary edit point to the dragged side, then trim.
fn drag_trim(app: &mut FilmcraftApp, which: &str, frames: i64) {
    use filmcraft_engine::trim::{TrimKind, sides};
    let Some(seq) = app.session.active_sequence() else { return };
    let Some(ep) = app.session.state.edit_points.first().copied() else { return };
    let (out_c, in_c) = sides(seq, &ep);
    let target = match which {
        "out" => out_c.map(|c| (c, "out", if ep.kind == TrimKind::Roll { TrimKind::Ripple } else { ep.kind })),
        "in" => in_c.map(|c| (c, "in", if ep.kind == TrimKind::Roll { TrimKind::Ripple } else { ep.kind })),
        _ => out_c.or(in_c).map(|c| (c, if out_c.is_some() { "out" } else { "in" }, TrimKind::Roll)),
    };
    let Some((clip, edge, kind)) = target else { return };
    let multi = app.session.state.edit_points.len() > 1;
    if !multi && (clip != ep.clip || (edge == "out") != ep.out || kind != ep.kind) {
        let kind = serde_json::to_value(kind).unwrap_or(json!("trim"));
        let _ = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": edge, "kind": kind}));
    }
    if let Err(e) = app.session.execute("trim.nudge", json!({"frames": frames})) {
        app.ui.status = e.to_string();
    }
}
