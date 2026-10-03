//! The Timeline panel.
//!
//! Layout (Premiere's): big blue playhead timecode + toggles top-left, ruler with markers /
//! in-out / render bar top-right, video tracks (V1 lowest) above audio tracks (A1 highest), track
//! headers on the left, a zoom scroll bar at the bottom.
//!
//! Interaction is tool-driven; every gesture ends in exactly one engine command, so it is undoable,
//! journaled and reproducible over the control channel. Zoom and scroll are *animated* (critically
//! damped exponential easing, anchored under the cursor) so navigation feels fluid; all geometry is
//! drawn as GPU meshes by egui's wgpu backend and culled to the visible range.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{ClipId, ItemId, Sequence, TrackId, TrackItem, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{TimelineView, Tool};
use crate::theme::Tokens;

const TOP_H: f32 = 58.0; // timecode + toolbar (left) / ruler (right)
const RULER_H: f32 = 44.0;
const SCROLLBAR_H: f32 = 17.0;
const DIVIDER_H: f32 = 5.0;
const MASTER_H: f32 = 34.0;
const SNAP_PX: f32 = 9.0;

/// Transient interaction state.
#[derive(Default)]
pub struct TlState {
    pub drag: Option<Drag>,
    /// Last layout (for hit-testing from the control channel).
    pub layout: Option<Layout>,
    /// Snap indicator x (screen) this frame.
    snap_x: Option<f32>,
    pub peaks: Arc<Mutex<HashMap<ItemId, Arc<Vec<(f32, f32)>>>>>,
    peaks_pending: Arc<Mutex<Vec<ItemId>>>,
    /// Source peak of each cached peak list (keyed by the list's address), for the waveform
    /// display gain: scanning the whole source per clip per frame cost more than drawing.
    peak_max: HashMap<ItemId, (usize, f32)>,
    zoom_anchor: Option<(f64, f32)>,
}

impl TlState {
    /// Forget cached waveform peaks: they are keyed by item id, and ids repeat across projects
    /// (opening another project would otherwise show the old project's waveforms). Peak jobs
    /// still running finish into the old maps and are dropped.
    pub fn reset_media_caches(&mut self) {
        self.peaks = Default::default();
        self.peaks_pending = Default::default();
        self.peak_max.clear();
    }
}

#[derive(Clone, Debug)]
pub enum Drag {
    Scrub,
    Move {
        clips: Vec<ClipId>,
        grab_tick: Tick,
        start_track: TrackId,
        offset: Tick,
        track_delta: i32,
    },
    Trim {
        clip: ClipId,
        edge: filmcraft_edit::Edge,
        mode: filmcraft_edit::TrimMode,
        delta: Tick,
    },
    Roll {
        left: ClipId,
        right: ClipId,
        delta: Tick,
    },
    Slip {
        clip: ClipId,
        delta: Tick,
    },
    Slide {
        clip: ClipId,
        delta: Tick,
    },
    Stretch {
        clip: ClipId,
        edge: filmcraft_edit::Edge,
        delta: Tick,
    },
    /// Remix tool: drag a music clip's Out edge; on release `clip.remix` re-plans it to the new duration.
    Remix {
        clip: ClipId,
        delta: Tick,
    },
    Pan {
        last: Pos2,
    },
    Marquee {
        start: Pos2,
    },
    Divider,
    ZoomBar {
        grab: f32,
        mode: u8,
    },
}

/// Geometry of one visible track row.
#[derive(Clone, Debug)]
pub struct Row {
    pub track: TrackId,
    pub kind: TrackKind,
    pub index: usize,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub content: Rect,
    pub ruler: Rect,
    pub rows: Vec<Row>,
    pub pps: f64,
    pub scroll: f64,
}

impl Layout {
    pub fn x_of(&self, t: Tick) -> f32 {
        self.content.min.x + ((t.seconds() - self.scroll) * self.pps) as f32
    }
    pub fn tick_at(&self, x: f32) -> Tick {
        Tick::from_seconds_f64(self.scroll + (x - self.content.min.x) as f64 / self.pps)
    }
    pub fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.iter().find(|r| r.rect.min.y <= y && y < r.rect.max.y)
    }
}

/// Zoom about a time (keeps it under the same screen x).
pub fn zoom_about(v: &mut TimelineView, factor: f64, anchor_secs: f64, width: f32) {
    let old = v.target_pps;
    let new = (old * factor).clamp(0.05, 24_000.0);
    let x = ((anchor_secs - v.target_scroll) * old) as f32;
    let x = if (0.0..=width).contains(&x) { x } else { width / 2.0 };
    v.target_pps = new;
    v.target_scroll = (anchor_secs - x as f64 / new).max(0.0);
}

fn label_color(app: &FilmcraftApp, l: filmcraft_project::Label) -> Color32 {
    crate::panels::settings::label_color(app, l)
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let Some(seq_id) = app.session.state.active_sequence else {
        empty_state(app, ui, rect);
        return;
    };
    let seq = app.session.active_sequence().expect("active").clone();
    let rate = seq.settings.frame_rate;
    let dt = ctx.input(|i| i.stable_dt).min(0.05) as f64;
    let header_w = app.ui.timeline.header_w;
    let content = Rect::from_min_max(pos2(rect.min.x + header_w, rect.min.y + TOP_H), pos2(rect.max.x - 10.0, rect.max.y - SCROLLBAR_H));
    let ruler = Rect::from_min_max(pos2(content.min.x, rect.min.y + TOP_H - RULER_H), pos2(content.max.x, rect.min.y + TOP_H));
    app.last_timeline_width = content.width();
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.panel_bg);

    // ---- sequence tabs strip (top-left header block)
    // ---- animated zoom / scroll
    let empty = seq.duration().seconds() <= 0.0;
    let dur_s = seq.duration().seconds().max(1.0);
    {
        let v = &mut app.ui.timeline;
        if let Some(p) = v.fit_empty
            && (!empty || v.target_pps != p)
        {
            v.fit_pending |= v.target_pps == p;
            v.fit_empty = None;
        }
        if v.fit_pending && content.width() > 50.0 {
            v.target_pps = (content.width() as f64 * 0.94 / dur_s).clamp(0.05, 24_000.0);
            v.target_scroll = 0.0;
            v.fit_pending = false;
            v.fit_empty = empty.then_some(v.target_pps);
        }
        let k = 1.0 - (-dt * 20.0).exp();
        let zooming = (v.pps - v.target_pps).abs() / v.target_pps > 0.001;
        v.pps += (v.target_pps - v.pps) * k;
        if let Some((anchor_t, anchor_x)) = app.tl.zoom_anchor.filter(|_| zooming) {
            v.scroll = (anchor_t - (anchor_x - content.min.x) as f64 / v.pps).max(0.0);
            v.target_scroll = (anchor_t - (anchor_x - content.min.x) as f64 / v.target_pps).max(0.0);
        } else {
            v.scroll += (v.target_scroll - v.scroll) * k;
            app.tl.zoom_anchor = None;
        }
        if (v.pps - v.target_pps).abs() > 1e-6 || (v.scroll - v.target_scroll).abs() > 1e-6 {
            ctx.request_repaint();
        }
        if (v.pps - v.target_pps).abs() / v.target_pps < 0.0005 {
            v.pps = v.target_pps;
        }
        if (v.scroll - v.target_scroll).abs() * v.pps < 0.05 {
            v.scroll = v.target_scroll;
        }
    }
    // follow playhead while playing (Settings ▸ Timeline ▸ Timeline Playback Auto-Scrolling)
    let auto_scroll = app.session.prefs.timeline.auto_scroll.clone();
    if app.playback.playing && app.ui.timeline.follow && auto_scroll != "noScroll" {
        let v = &mut app.ui.timeline;
        let ph = app.session.playhead().seconds();
        let w = content.width() as f64;
        let px = (ph - v.scroll) * v.pps;
        if auto_scroll == "smoothScroll" {
            // the playhead stays in the middle once it gets there
            if px > w * 0.5 || px < 0.0 {
                v.target_scroll = (ph - w * 0.5 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        } else if px > w * 0.95 || px < 0.0 {
            v.target_scroll = (ph - w * 0.05 / v.pps).max(0.0);
            v.scroll = v.target_scroll;
        }
    }
    let pps = app.ui.timeline.pps;
    let scroll = app.ui.timeline.scroll;

    // ---- rows
    // caption tracks sit in their own area above the video tracks
    let cap_n = seq.caption_tracks.len();
    let caption_area = Rect::from_min_max(pos2(rect.min.x, content.min.y), pos2(content.max.x, content.min.y + cap_n as f32 * super::timeline_captions::ROW_H));
    let cap_h = if cap_n > 0 { caption_area.height() + DIVIDER_H } else { 0.0 };
    let tracks_area = Rect::from_min_max(pos2(content.min.x, content.min.y + cap_h), content.max);
    let split_y = tracks_area.min.y + (tracks_area.height() - DIVIDER_H) * app.ui.timeline.split;
    let video_area = Rect::from_min_max(pos2(rect.min.x, tracks_area.min.y), pos2(content.max.x, split_y));
    let audio_area = Rect::from_min_max(pos2(rect.min.x, split_y + DIVIDER_H), pos2(content.max.x, tracks_area.max.y - MASTER_H));
    let vh = app.ui.timeline.video_track_h;
    let ah = app.ui.timeline.audio_track_h;
    let mut rows = Vec::new();
    // Video: V1 at the bottom of the video area, stacking upward; v_scroll shifts up.
    let nv = seq.video_tracks.len();
    let video_total = nv as f32 * vh;
    let v_off = (video_area.height() - video_total).max(0.0);
    let max_vs = (video_total - video_area.height()).max(0.0);
    app.ui.timeline.v_scroll = app.ui.timeline.v_scroll.clamp(0.0, max_vs);
    for (i, tr) in seq.video_tracks.iter().enumerate() {
        let top = video_area.min.y + v_off + (nv - 1 - i) as f32 * vh - (max_vs - app.ui.timeline.v_scroll);
        rows.push(Row { track: tr.id, kind: TrackKind::Video, index: i, rect: Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + vh)) });
    }
    let na = seq.audio_tracks.len();
    let max_as = (na as f32 * ah - audio_area.height()).max(0.0);
    app.ui.timeline.a_scroll = app.ui.timeline.a_scroll.clamp(0.0, max_as);
    for (i, tr) in seq.audio_tracks.iter().enumerate() {
        let top = audio_area.min.y + i as f32 * ah - app.ui.timeline.a_scroll;
        rows.push(Row { track: tr.id, kind: TrackKind::Audio, index: i, rect: Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + ah)) });
    }
    let layout = Layout { content, ruler, rows: rows.clone(), pps, scroll };
    app.tl.layout = Some(layout.clone());

    // ---- backgrounds
    painter.rect_filled(Rect::from_min_max(pos2(content.min.x, content.min.y), content.max), 0.0, t.tl_bg);
    let vclip = Rect::from_min_max(pos2(rect.min.x, video_area.min.y), video_area.max);
    let aclip = Rect::from_min_max(pos2(rect.min.x, audio_area.min.y), audio_area.max);
    for r in &rows {
        let clip = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip);
        if row.height() <= 0.0 {
            continue;
        }
        let bg = if r.index % 2 == 0 { t.tl_track_bg } else { t.tl_track_bg_alt };
        painter.rect_filled(row, 0.0, bg);
        painter.line_segment([pos2(row.min.x, r.rect.max.y - 0.5), pos2(row.max.x, r.rect.max.y - 0.5)], Stroke::new(1.0, t.tl_bg));
    }
    // in/out shading across tracks
    if seq.mark_in.is_some() || seq.mark_out.is_some() {
        let a = layout.x_of(seq.mark_in.unwrap_or(Tick::ZERO)).max(content.min.x);
        let b = layout.x_of(seq.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(seq.duration())).min(content.max.x);
        if b > a {
            painter.rect_filled(Rect::from_min_max(pos2(a, content.min.y), pos2(b, content.max.y)), 0.0, t.in_out_shade);
        }
    }

    // ---- clips
    let visible = (layout.tick_at(content.min.x - 2.0), layout.tick_at(content.max.x + 2.0));
    let selection: Vec<ClipId> = app.session.state.selection.clone();
    let mut previews: HashMap<ClipId, (Tick, Tick, Option<TrackId>)> = HashMap::new(); // live drag preview: (start, dur, track)
    preview_drag(app, &seq, &layout, &mut previews);
    for r in &rows {
        let tr = seq.track(r.track).expect("track");
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip_rect);
        if row.height() <= 0.0 {
            continue;
        }
        let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, row.min.y), pos2(content.max.x, row.max.y)));
        for it in &tr.items {
            let (start, dur, moved_track) = previews.get(&it.id).copied().unwrap_or((it.start, it.duration, None));
            if moved_track.is_some_and(|m| m != r.track) {
                continue;
            }
            if start + dur < visible.0 || start > visible.1 {
                continue;
            }
            let x0 = layout.x_of(start);
            let x1 = layout.x_of(start + dur);
            let body = Rect::from_min_max(pos2(x0, r.rect.min.y + 1.0), pos2(x1.max(x0 + 1.0), r.rect.max.y - 1.0));
            draw_clip(app, &ctx, &p, body, it, r.kind, selection.contains(&it.id), &t, rate);
            app.auto.add(&format!("timeline.clip.{}", it.id.0), body.intersect(content), &it.name);
        }
        // items dragged onto this track from another
        for (cid, (start, dur, mt)) in &previews {
            if *mt == Some(r.track)
                && tr.item(*cid).is_none()
                && let Some((_, it)) = seq.find_item(*cid)
            {
                let body = Rect::from_min_max(pos2(layout.x_of(*start), r.rect.min.y + 1.0), pos2(layout.x_of(*start + *dur), r.rect.max.y - 1.0));
                draw_clip(app, &ctx, &p, body, it, r.kind, true, &t, rate);
            }
        }
        // Show Through Edits: a small bow-tie on cuts between continuous pieces of one clip
        if app.session.state.show_through_edits {
            for te in filmcraft_edit::through::track_through_edits(tr) {
                let x = layout.x_of(te.time);
                if x < content.min.x - 6.0 || x > content.max.x + 6.0 {
                    continue;
                }
                let r = paint_through_edit(&p, x, r.rect);
                app.auto.add(&format!("timeline.throughEdit.{}", te.right.0), r, "Through edit");
            }
        }
        // selected edit points (trim mode): red brackets for ripple, yellow for roll / regular trim
        super::trim_monitor::paint_edit_points(app, &p, &seq, tr, r, &layout);
        // transitions
        for trn in &tr.transitions {
            let x0 = layout.x_of(trn.start);
            let x1 = layout.x_of(trn.end());
            let tr_rect = Rect::from_min_max(pos2(x0, r.rect.min.y + 17.0), pos2(x1, r.rect.max.y - 1.0));
            draw_transition(&p, tr_rect, trn, &t);
            app.auto.add(&format!("timeline.transition.{}", trn.id.0), tr_rect, &trn.effect.effect);
        }
    }

    // ---- track headers
    draw_headers(app, ui, &seq, &rows, rect, vclip, aclip, &t);
    // divider between video and audio
    let div = Rect::from_min_max(pos2(rect.min.x, split_y), pos2(content.max.x, split_y + DIVIDER_H));
    painter.rect_filled(div, 0.0, t.app_bg);
    let div_resp = ui.interact(div, egui::Id::new("tl-divider"), Sense::drag());
    if div_resp.hovered() || div_resp.dragged() {
        ctx.set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if div_resp.dragged() {
        let f = app.ui.timeline.split + div_resp.drag_delta().y / (tracks_area.height() - DIVIDER_H).max(1.0);
        app.ui.timeline.split = f.clamp(0.1, 0.9);
    }
    // master track
    let master = Rect::from_min_max(pos2(rect.min.x, tracks_area.max.y - MASTER_H), pos2(content.max.x, tracks_area.max.y));
    painter.rect_filled(master, 0.0, t.tl_header_bg);
    painter.text(pos2(rect.min.x + 44.0, master.center().y), Align2::LEFT_CENTER, "Mix", Tokens::ui(11.5), t.text);
    painter.text(
        pos2(rect.min.x + header_w - 12.0, master.center().y),
        Align2::RIGHT_CENTER,
        format!("{:.1}", seq.master_volume_db),
        Tokens::ui(11.5),
        t.hot_text,
    );

    // ---- caption tracks
    if cap_n > 0 {
        painter.rect_filled(Rect::from_min_max(pos2(rect.min.x, caption_area.max.y), pos2(content.max.x, caption_area.max.y + DIVIDER_H)), 0.0, t.app_bg);
        super::timeline_captions::paint(app, ui, &seq, caption_area, &layout, &t);
    }

    // ---- top block: timecode + toggles, ruler
    draw_top(app, ui, rect, &seq, &layout, &t, seq_id);

    // ---- playhead
    let ph = app.session.playhead();
    let px = layout.x_of(ph);
    if px >= content.min.x - 1.0 && px <= content.max.x + 1.0 {
        let head = [
            pos2(px - 6.0, ruler.min.y + 2.0),
            pos2(px + 6.0, ruler.min.y + 2.0),
            pos2(px + 6.0, ruler.max.y - 10.0),
            pos2(px, ruler.max.y - 4.0),
            pos2(px - 6.0, ruler.max.y - 10.0),
        ];
        painter.add(egui::Shape::convex_polygon(head.to_vec(), t.playhead, Stroke::NONE));
        painter.line_segment([pos2(px, ruler.max.y - 4.0), pos2(px, content.max.y)], Stroke::new(1.0, t.playhead));
    }
    // snap indicator
    if let Some(sx) = app.tl.snap_x.take() {
        painter.line_segment([pos2(sx, ruler.max.y), pos2(sx, content.max.y)], Stroke::new(1.0, Color32::from_rgb(250, 250, 250)));
        for y in [ruler.max.y, content.max.y] {
            let d = if y == ruler.max.y { 1.0 } else { -1.0 };
            painter.add(egui::Shape::convex_polygon(vec![pos2(sx - 4.0, y), pos2(sx + 4.0, y), pos2(sx, y + 5.0 * d)], Color32::WHITE, Stroke::NONE));
        }
    }

    // ---- scroll bars
    zoom_scrollbar(app, ui, Rect::from_min_max(pos2(content.min.x, rect.max.y - SCROLLBAR_H + 2.0), pos2(content.max.x, rect.max.y - 2.0)), dur_s, &t);
    vertical_scrollbar(
        ui,
        Rect::from_min_max(pos2(content.max.x + 2.0, video_area.min.y), pos2(rect.max.x - 1.0, video_area.max.y)),
        &mut app.ui.timeline.v_scroll,
        max_vs,
        true,
        &t,
        "vs",
    );
    vertical_scrollbar(
        ui,
        Rect::from_min_max(pos2(content.max.x + 2.0, audio_area.min.y), pos2(rect.max.x - 1.0, audio_area.max.y)),
        &mut app.ui.timeline.a_scroll,
        max_as,
        false,
        &t,
        "as",
    );

    // ---- interaction
    interact(app, ui, &seq, &layout, rect);
    // track keyframes (drawn and edited on top of the clips)
    super::timeline_automation::show(app, ui, &seq, &layout, aclip);
    if cap_n > 0 {
        super::timeline_captions::interact(app, ui, &seq, caption_area, &layout);
    }
    let _ = (visible, TICKS_PER_SECOND);
}

fn empty_state(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, "Drop media here to create sequence.", Tokens::ui(13.0), t.text_dim);
    let b = Rect::from_center_size(rect.center() + vec2(0.0, 20.0), vec2(170.0, 26.0));
    let resp = ui.interact(b, egui::Id::new("tl-open-demo"), Sense::click());
    app.auto.add("timeline.openDemo", b, "Open Demo Project");
    ui.painter().rect_filled(b, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(b.center(), Align2::CENTER_CENTER, "Open Demo Project", Tokens::semibold(12.0), Color32::WHITE);
    if resp.clicked() {
        let _ = app.session.execute("file.openDemoProject", json!({}));
    }
    // accept drops from the project panel
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && ui.rect_contains_pointer(rect)
        && ui.input(|i| i.pointer.any_released())
    {
        let _ = app.session.execute("file.newSequence", json!({"fromItem": item.0}));
    }
}

/// The through-edit mark: two small triangles pointing at the cut, centred on the track, with a
/// dashed white line through the clip body. Returns the mark's rect.
fn paint_through_edit(p: &egui::Painter, x: f32, row: Rect) -> Rect {
    let cy = row.center().y.max(row.min.y + 10.0);
    let (w, h) = (4.0, 4.0);
    let col = Color32::from_white_alpha(230);
    let mut y = row.min.y + 2.0;
    while y < row.max.y - 2.0 {
        p.line_segment([pos2(x, y), pos2(x, (y + 3.0).min(row.max.y - 2.0))], Stroke::new(1.0, Color32::from_white_alpha(140)));
        y += 6.0;
    }
    p.add(egui::Shape::convex_polygon(vec![pos2(x - w, cy - h), pos2(x, cy), pos2(x - w, cy + h)], col, Stroke::new(0.5, Color32::BLACK)));
    p.add(egui::Shape::convex_polygon(vec![pos2(x + w, cy - h), pos2(x + w, cy + h), pos2(x, cy)], col, Stroke::new(0.5, Color32::BLACK)));
    Rect::from_center_size(pos2(x, cy), vec2(2.0 * w + 2.0, 2.0 * h + 2.0))
}

fn lighten(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f).round() as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

/// Whether a clip lies inside the sequence In/Out range (Premiere brightens those).
fn in_range(app: &FilmcraftApp, it: &TrackItem) -> bool {
    let Some(q) = app.session.active_sequence() else { return false };
    if q.mark_in.is_none() && q.mark_out.is_none() {
        return false;
    }
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + q.settings.frame_rate.frame_duration()).unwrap_or(Tick::MAX);
    it.start < b && it.end() > a
}

#[allow(clippy::too_many_arguments)]
fn draw_clip(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    p: &egui::Painter,
    body: Rect,
    it: &TrackItem,
    kind: TrackKind,
    selected: bool,
    t: &Tokens,
    rate: FrameRate,
) {
    let base = label_color(app, it.label);
    let inr = in_range(app, it);
    let fill = if !it.enabled {
        Color32::from_rgb(0x2a, 0x2a, 0x2a)
    } else if inr {
        lighten(base, 0.12)
    } else if selected {
        lighten(base, 0.06)
    } else {
        base
    };
    p.rect_filled(body, 0.0, fill);
    // 1 pt lighter top edge
    p.line_segment([pos2(body.min.x, body.min.y + 0.5), pos2(body.max.x, body.min.y + 0.5)], Stroke::new(1.0, lighten(fill, 0.15)));
    let name_h = 16.0;
    let w = body.width();
    // head thumbnail (video): left-aligned, aspect-correct, below the name band
    if kind == TrackKind::Video && app.ui.timeline.show_thumbnails && body.height() > 28.0 && w > 20.0 && it.enabled {
        let th = Rect::from_min_max(pos2(body.min.x + 1.0, body.min.y + name_h), pos2(body.max.x - 1.0, body.max.y - 1.0));
        let aspect = app.session.project.item(it.item).and_then(|pi| match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref().map(|v| v.width as f32 / v.height as f32),
            filmcraft_project::ItemKind::Sequence(s) => Some(s.settings.width as f32 / s.settings.height as f32),
            _ => None,
        });
        if let Some(aspect) = aspect {
            let tw = (th.height() * aspect).min(th.width());
            let mt = rate.snap(it.source_in);
            // a multi-camera clip shows its angle's clip
            let (thumb_item, mt) = app
                .session
                .project
                .sequence(it.item)
                .and_then(|q| {
                    let ti = q.angle_video_track_index(it.multicam_angle(q)?)?;
                    let inner = q.video_tracks[ti].item_at(mt)?;
                    Some((inner.item, inner.source_time_at(mt)))
                })
                .unwrap_or((it.item, mt));
            if let Some((tex, _)) = app.thumbnail(ctx, thumb_item, mt, 160) {
                let r = Rect::from_min_size(th.min, vec2(tw, th.height()));
                let cp = p.with_clip_rect(th.intersect(p.clip_rect()));
                cp.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2((tw / (th.height() * aspect)).min(1.0), 1.0)), Color32::WHITE);
            }
        }
    }
    // waveform (audio): rectified, lower 40 %
    if kind == TrackKind::Audio && app.ui.timeline.show_waveforms && w > 3.0 {
        draw_waveform(app, p, body, it, if inr { Color32::from_rgb(0xc7, 0xe9, 0xfa) } else { Color32::from_rgb(0x79, 0xc0, 0xf9) });
    }
    let clip_p = p.with_clip_rect(body.shrink2(vec2(1.0, 0.0)).intersect(p.clip_rect()));
    // name band
    if w > 24.0 {
        let mut label = it.name.clone();
        if let Some(m) = it.multicam.filter(|m| m.enabled) {
            let cam = app.session.project.sequence(it.item).and_then(|q| q.cameras().cameras.get(m.angle as usize).map(|c| c.name.clone()));
            label = format!("[MC{}] {}", m.angle + 1, cam.unwrap_or(label));
        }
        if (it.speed - 1.0).abs() > 1e-6 || it.reverse {
            label = format!("{label} [{}%]", ((if it.reverse { -1.0 } else { 1.0 }) * it.speed * 100.0).round());
        }
        clip_p.text(
            pos2(body.min.x + 6.0, body.min.y + 8.5),
            Align2::LEFT_CENTER,
            label,
            Tokens::ui(11.5),
            if it.enabled { Color32::from_rgb(0xd9, 0xd9, 0xd9) } else { t.text_faint },
        );
    }
    // fx badge right-aligned in the header (italic-ish "fx")
    if (it.has_standard_effects() || it.has_modified_intrinsics()) && w > 40.0 {
        let col = if it.has_standard_effects() { Color32::from_rgb(0xe8, 0xc5, 0x4a) } else { Color32::from_rgb(0x91, 0xa2, 0xac) };
        clip_p.text(
            pos2(body.max.x - 6.0, body.min.y + 8.5),
            Align2::RIGHT_CENTER,
            "fx",
            egui::FontId::new(11.0, egui::FontFamily::Name("semibold".into())),
            col,
        );
    }
    // corner triangles where the clip reaches the media's first/last frame (no handles)
    let media_dur = app.session.project.item(it.item).map(|i| i.duration());
    let no_head = it.source_in <= Tick::ZERO;
    let no_tail = media_dur.is_some_and(|d| d.0 > 0 && it.source_out() >= d - rate.frame_duration());
    let tri = 5.0;
    if no_head && w > 12.0 {
        clip_p.add(egui::Shape::convex_polygon(
            vec![body.min, body.min + vec2(tri, 0.0), body.min + vec2(0.0, tri)],
            Color32::from_white_alpha(220),
            Stroke::NONE,
        ));
    }
    if no_tail && w > 12.0 {
        let tr = pos2(body.max.x, body.min.y);
        clip_p.add(egui::Shape::convex_polygon(vec![tr, tr + vec2(0.0, tri), tr - vec2(tri, 0.0)], Color32::from_white_alpha(220), Stroke::NONE));
    }
    // 1 pt black separator at the clip's left edge
    p.line_segment([pos2(body.min.x + 0.5, body.min.y), pos2(body.min.x + 0.5, body.max.y)], Stroke::new(1.0, Color32::BLACK));
    if selected {
        p.rect_stroke(body.shrink(1.0), 0.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
        p.rect_stroke(body, 0.0, Stroke::new(2.0, t.clip_selected_border), StrokeKind::Inside);
    }
}

fn draw_waveform(app: &mut FilmcraftApp, p: &egui::Painter, body: Rect, it: &TrackItem, col: Color32) {
    let Some(peaks) = request_peaks(app, it.item) else { return };
    let spp = 256.0;
    let sr = 48_000.0;
    let zone_h = (body.height() * 0.42).max(8.0);
    let area = Rect::from_min_max(pos2(body.min.x + 1.0, body.max.y - zone_h - 1.0), pos2(body.max.x - 1.0, body.max.y - 1.0));
    let clip = p.clip_rect().intersect(area);
    if clip.width() <= 0.0 {
        return;
    }
    let key = Arc::as_ptr(&peaks) as usize;
    let peak = match app.tl.peak_max.get(&it.item) {
        Some(&(k, v)) if k == key => v,
        _ => {
            let v = peaks.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()));
            app.tl.peak_max.insert(it.item, (key, v));
            v
        }
    };
    let gain = waveform_display_gain(peak, it.gain_db);
    let dynamic = app.ui.extras.dynamic_waveforms;
    let mut mesh = egui::Mesh::default();
    let dur_px = body.width().max(1.0);
    for x in (clip.min.x.floor() as i32)..(clip.max.x.ceil() as i32) {
        let f0 = (x as f32 - body.min.x) / dur_px;
        let f1 = (x as f32 + 1.0 - body.min.x) / dur_px;
        let t0 = it.start + Tick((it.duration.0 as f64 * f0 as f64) as i64);
        let t1 = it.start + Tick((it.duration.0 as f64 * f1 as f64) as i64);
        let s0 = (it.source_time_at(t0).seconds() * sr / spp) as usize;
        let s1 = ((it.source_time_at(t1).seconds() * sr / spp) as usize).max(s0 + 1);
        let mut m = 0f32;
        for (a, b) in peaks.iter().skip(s0).take(s1 - s0) {
            m = m.max(a.abs()).max(b.abs());
        }
        // View ▸ Dynamic Audio Waveforms (default): logarithmic scale, −48 dB → 0, 0 dB → full;
        // off: linear amplitude
        let h = if dynamic {
            let db = 20.0 * (m * gain).max(1e-5).log10();
            ((db + 48.0) / 48.0).clamp(0.0, 1.0) * area.height()
        } else {
            (m * gain).clamp(0.0, 1.0) * area.height()
        };
        if h > 0.3 {
            mesh.add_colored_rect(Rect::from_min_max(pos2(x as f32, area.max.y - h), pos2(x as f32 + 1.0, area.max.y)), col);
        }
    }
    p.add(mesh);
    // channel label box
    let cb = Rect::from_min_size(pos2(body.min.x + 3.0, body.max.y - 11.0), vec2(8.0, 9.0));
    if body.width() > 30.0 {
        p.rect_filled(cb, 1.0, Color32::from_black_alpha(160));
        p.text(cb.center(), Align2::CENTER_CENTER, "1", Tokens::ui(7.5), Color32::from_rgb(0xd9, 0xd9, 0xd9));
    }
    // volume rubber band (white line with a black shadow) at mid-height of the upper zone
    let level = it.effect("volume").map(|e| e.f64_at("level", it.source_in)).unwrap_or(0.0);
    let upper = Rect::from_min_max(pos2(body.min.x, body.min.y + 16.0), pos2(body.max.x, area.min.y));
    if upper.height() > 6.0 {
        let norm = ((level + 60.0) / 66.0).clamp(0.0, 1.0) as f32;
        let y = upper.max.y - norm * upper.height();
        let cp = p.with_clip_rect(body.intersect(p.clip_rect()));
        cp.line_segment([pos2(body.min.x, y + 1.0), pos2(body.max.x, y + 1.0)], Stroke::new(1.0, Color32::BLACK));
        cp.line_segment([pos2(body.min.x, y), pos2(body.max.x, y)], Stroke::new(1.0, Color32::WHITE));
    }
}

/// Display gain for a clip's waveform: clip gain, plus normalisation of the source to full scale
/// so quiet recordings stay readable, but by at most +12 dB, so a near-silent track (room tone at
/// −50 dBFS, a silent film's empty audio stream) does not look like a loud one.
fn waveform_display_gain(source_peak: f32, clip_gain_db: f64) -> f32 {
    const MAX_BOOST: f32 = 3.981_072; // +12 dB
    let boost = (1.0 / source_peak.max(1e-9)).min(MAX_BOOST);
    filmcraft_render::audio::db_to_gain(clip_gain_db) * boost
}

pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    if let Some(p) = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&item) {
        return Some(p.clone());
    }
    // No source: try again on a later frame instead of leaving the item pending forever.
    let src = app.session.source(item)?;
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&item) {
            return None;
        }
        pend.push(item);
    }
    let peaks = app.tl.peaks.clone();
    let pending = app.tl.peaks_pending.clone();
    let dur = src.info().duration;
    let run = move || {
        let sr = 48_000u32;
        let total = dur.to_units_floor(sr as i64).max(0) as usize;
        let mut out = Vec::with_capacity(total / 256 + 1);
        let chunk = 48_000 * 4;
        let mut s = 0usize;
        while s < total {
            let n = chunk.min(total - s);
            match src.audio(s as i64, n, sr) {
                Ok(buf) => {
                    let ch = buf.channels.first().cloned().unwrap_or_default();
                    for c in ch.chunks(256) {
                        let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                        out.push((lo, hi));
                    }
                }
                Err(_) => break,
            }
            s += n;
        }
        peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(item, Arc::new(out));
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != item);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    None
}

fn draw_transition(p: &egui::Painter, r: Rect, trn: &filmcraft_project::Transition, _t: &Tokens) {
    let cp = p.with_clip_rect(r.intersect(p.clip_rect()));
    cp.rect_filled(r, 0.0, Color32::from_black_alpha(90));
    // 45° hatching
    let step = 4.0;
    let mut x = r.min.x - r.height();
    while x < r.max.x {
        cp.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xd9, 0xd9, 0xd9, 110)));
        x += step;
    }
    let audio = matches!(trn.effect.def().map(|d| d.kind), Some(filmcraft_project::EffectKind::AudioTransition));
    if audio {
        // crossing fade curves
        let n = 16;
        let a: Vec<Pos2> = (0..=n)
            .map(|i| {
                pos2(r.min.x + r.width() * i as f32 / n as f32, r.min.y + r.height() * (1.0 - ((i as f32 / n as f32) * std::f32::consts::FRAC_PI_2).sin()))
            })
            .collect();
        let b: Vec<Pos2> = (0..=n)
            .map(|i| {
                pos2(r.min.x + r.width() * i as f32 / n as f32, r.min.y + r.height() * (1.0 - ((i as f32 / n as f32) * std::f32::consts::FRAC_PI_2).cos()))
            })
            .collect();
        cp.add(egui::Shape::line(a, Stroke::new(1.0, Color32::WHITE)));
        cp.add(egui::Shape::line(b, Stroke::new(1.0, Color32::WHITE)));
    }
    cp.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(0xeb, 0xeb, 0xeb)), StrokeKind::Inside);
    if r.width() > 34.0 && !audio {
        let name = trn.effect.def().map(|d| d.name).unwrap_or(&trn.effect.effect);
        let tr = Rect::from_min_size(r.min + vec2(2.0, 2.0), vec2((r.width() - 4.0).min(80.0), 13.0));
        cp.rect_filled(tr, 0.0, Color32::from_black_alpha(170));
        cp.text(pos2(tr.min.x + 3.0, tr.center().y), Align2::LEFT_CENTER, name, Tokens::ui(10.5), Color32::from_rgb(0xd9, 0xd9, 0xd9));
    }
}

/// A Premiere track-header button (patch / target): blue fill when on, full track height.
fn patch_button(ui: &mut egui::Ui, r: Rect, clip: Rect, label: &str, on: bool, show_off: bool, id: egui::Id, t: &Tokens) -> egui::Response {
    let resp = ui.interact(r.intersect(clip), id, Sense::click());
    let p = ui.painter().with_clip_rect(clip);
    if on {
        p.rect_filled(r, 2.0, Color32::from_rgb(0x26, 0x5b, 0xc1));
        p.text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(10.0), Color32::from_rgb(0xeb, 0xeb, 0xeb));
    } else if show_off {
        if resp.hovered() {
            p.rect_filled(r, 2.0, t.hover);
        }
        p.text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(10.0), t.text_dim);
    } else if resp.hovered() {
        p.rect_stroke(r, 2.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn draw_headers(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, rows: &[Row], rect: Rect, vclip: Rect, aclip: Rect, t: &Tokens) {
    let hw = app.ui.timeline.header_w;
    let tg = app.session.targeting();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut vo_action = None;
    for r in rows {
        let tr = seq.track(r.track).expect("track");
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let hrect = Rect::from_min_max(pos2(rect.min.x, r.rect.min.y), pos2(rect.min.x + hw, r.rect.max.y));
        let visible = hrect.intersect(clip_rect);
        if visible.height() <= 0.0 {
            continue;
        }
        let p = ui.painter().with_clip_rect(visible);
        p.rect_filled(hrect, 0.0, t.tl_header_bg);
        p.line_segment([pos2(hrect.min.x, hrect.max.y - 0.5), pos2(hrect.max.x, hrect.max.y - 0.5)], Stroke::new(1.0, t.separator));
        let label = format!("{}{}", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1);
        let btn_rect = |x0: f32| Rect::from_min_max(pos2(hrect.min.x + x0, hrect.min.y + 1.0), pos2(hrect.min.x + x0 + 24.0, hrect.max.y - 2.0));
        // 1. source patch (absent when unpatched)
        let patched = if r.kind == TrackKind::Video { tg.video_dest == Some(r.track) } else { tg.audio_dest == Some(r.track) };
        let pr = btn_rect(13.0);
        app.auto.add(&format!("timeline.track.{label}.sourcePatch"), pr, "Source patch");
        if patch_button(ui, pr, visible, &label, patched, false, egui::Id::new(("patch", r.track.0)), t).clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "sourcePatch": !patched})));
        }
        // 2. track lock
        let small = |x: f32, y: f32| Rect::from_center_size(pos2(hrect.min.x + x, y), vec2(18.0, 18.0));
        let upper_y = hrect.min.y + (16.0f32).min(hrect.height() / 2.0);
        let lock_r = small(49.0, hrect.center().y);
        let lresp = crate::widgets::icon_toggle(
            ui,
            lock_r.intersect(visible),
            if tr.locked { Icon::Lock } else { Icon::Unlock },
            true,
            t,
            egui::Id::new(("locked", r.track.0)),
            Some(if tr.locked { Color32::from_rgb(0xd1, 0xd1, 0xd1) } else { t.text_dim }),
        );
        app.auto.add(&format!("timeline.track.{label}.locked"), lock_r, "Toggle Track Lock");
        if lresp.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "locked": !tr.locked})));
        }
        if tr.locked {
            // diagonal hatch over locked lanes is drawn by the lane painter; here a subtle tint
            p.rect_filled(Rect::from_min_max(pos2(hrect.max.x - 4.0, hrect.min.y), hrect.max), 0.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
        }
        // 3. target
        let targeted = tg.targeted.contains(&r.track);
        let trr = btn_rect(61.0);
        app.auto.add(&format!("timeline.track.{label}.target"), trr, "Toggle track targeting");
        if patch_button(ui, trr, visible, &label, targeted, true, egui::Id::new(("target", r.track.0)), t).clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "targeted": !targeted})));
        }
        // 4. upper line icons
        let mut x = 102.0;
        let sr = small(x, upper_y);
        let sresp =
            crate::widgets::icon_toggle(ui, sr.intersect(visible), Icon::SyncLock, tr.sync_lock, t, egui::Id::new(("syncLock", r.track.0)), Some(t.text_dim));
        app.auto.add(&format!("timeline.track.{label}.syncLock"), sr, "Toggle Sync Lock");
        if sresp.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "syncLock": !tr.sync_lock})));
        }
        x += 24.0;
        if r.kind == TrackKind::Video {
            let er = small(x, upper_y);
            let eresp = crate::widgets::icon_toggle(
                ui,
                er.intersect(visible),
                if tr.enabled { Icon::Eye } else { Icon::EyeOff },
                true,
                t,
                egui::Id::new(("enabled", r.track.0)),
                Some(t.text_dim),
            );
            app.auto.add(&format!("timeline.track.{label}.enabled"), er, "Toggle Track Output");
            if eresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "enabled": !tr.enabled})));
            }
        } else {
            let mr = Rect::from_center_size(pos2(hrect.min.x + x, upper_y), vec2(14.0, 14.0));
            let mresp = crate::widgets::letter_toggle(
                ui,
                mr.intersect(visible),
                "M",
                tr.muted,
                Color32::from_rgb(0x2d, 0x9d, 0x78),
                t,
                egui::Id::new(("mute", r.track.0)),
            );
            app.auto.add(&format!("timeline.track.{label}.muted"), mr, "Mute Track");
            if mresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "muted": !tr.muted})));
            }
            let s2 = Rect::from_center_size(pos2(hrect.min.x + x + 20.0, upper_y), vec2(14.0, 14.0));
            let so = crate::widgets::letter_toggle(
                ui,
                s2.intersect(visible),
                "S",
                tr.solo,
                Color32::from_rgb(0xf0, 0xf0, 0x4f),
                t,
                egui::Id::new(("solo", r.track.0)),
            );
            app.auto.add(&format!("timeline.track.{label}.solo"), s2, "Solo Track");
            if so.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "solo": !tr.solo})));
            }
            let vr = small(x + 42.0, upper_y);
            if let Some(a) = super::voiceover::header_button(app, ui, r.track, vr, visible, &label, t) {
                vo_action = Some(a);
            }
            let kr = small(x + 64.0, upper_y);
            super::timeline_automation::header_button(app, ui, seq, r, kr, visible, &label, t);
        }
        // 5. name on the lower line (or inline when short)
        if hrect.height() >= 40.0 {
            p.text(pos2(hrect.min.x + 94.0, hrect.min.y + 34.0), Align2::LEFT_CENTER, &tr.name, Tokens::ui(11.0), t.text_dim);
        }
        // resize track height by dragging the header's bottom edge
        let edge = Rect::from_min_max(pos2(hrect.min.x, hrect.max.y - 3.0), pos2(hrect.max.x, hrect.max.y + 2.0)).intersect(visible);
        if edge.height() > 0.0 {
            let eresp = ui.interact(edge, egui::Id::new(("trackh", r.track.0)), Sense::drag());
            if eresp.hovered() || eresp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if eresp.dragged() {
                let d = eresp.drag_delta().y * if r.kind == TrackKind::Video { -1.0 } else { 1.0 };
                let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
                *h = (*h + d).clamp(22.0, 220.0);
            }
        }
        let resp = ui.interact(
            Rect::from_min_max(pos2(hrect.min.x + 90.0, hrect.min.y + 26.0), hrect.max).intersect(visible),
            egui::Id::new(("hdr", r.track.0)),
            Sense::click(),
        );
        if resp.double_clicked() {
            let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
            *h = if *h < 50.0 { 64.0 } else { 30.0 };
        }
    }
    // column separator
    ui.painter().line_segment([pos2(rect.min.x + hw - 0.5, vclip.min.y), pos2(rect.min.x + hw - 0.5, aclip.max.y + MASTER_H)], Stroke::new(1.0, t.separator));
    for (cmd, params) in actions {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
    if let Some(a) = vo_action {
        super::voiceover::run(app, &ui.ctx().clone(), a);
    }
}

fn draw_top(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, seq: &Sequence, layout: &Layout, t: &Tokens, seq_id: ItemId) {
    let p = ui.painter().clone();
    let rate = seq.settings.frame_rate;
    let hw = app.ui.timeline.header_w;
    let ruler = layout.ruler;
    // background of the whole top block
    p.rect_filled(Rect::from_min_max(rect.min, pos2(rect.max.x, ruler.max.y)), 0.0, t.panel_bg);
    // current timecode: 14 pt semibold accent
    let tc = format_time(app.session.playhead(), rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    let tc_rect = Rect::from_min_size(pos2(rect.min.x + 14.0, rect.min.y + 4.0), vec2(hw - 20.0, 20.0));
    p.text(pos2(tc_rect.min.x, tc_rect.center().y), Align2::LEFT_CENTER, &tc, Tokens::semibold(15.0), t.hot_text);
    app.auto.add("timeline.timecode", tc_rect, &tc);
    // toolbar: 30 × 30 buttons, "on" = #4b4b4b fill
    let mut x = rect.min.x + 12.0;
    let y = rect.min.y + 26.0;
    let toggles: [(Icon, &str, bool, bool, &str); 6] = [
        (Icon::Nest, "nest", true, true, "Insert and overwrite sequences as nests or individual clips"),
        (Icon::Magnet, "snap", app.session.state.snapping, true, "Snap in Timeline (S)"),
        (Icon::Link, "linked", app.session.state.linked_selection, true, "Linked Selection"),
        (Icon::Captions, "captions", false, false, "Caption track options"),
        (Icon::Marker, "marker", false, false, "Add Marker (M)"),
        (Icon::Wrench, "settings", false, false, "Timeline Display Settings"),
    ];
    for (icon, key, on, toggle, tip) in toggles {
        let r = Rect::from_min_size(pos2(x, y), vec2(28.0, 28.0));
        let resp = ui.interact(r, egui::Id::new(("tl-toggle", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("timeline.toggle.{key}"), r, tip);
        if toggle && on {
            p.rect_filled(r, 4.0, Color32::from_rgb(0x4b, 0x4b, 0x4b));
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, t.hover);
        }
        icons::paint(&p, r.shrink(7.0), icon, if toggle && on { t.text } else { t.text_dim });
        if resp.clicked() {
            let _ = match key {
                "snap" => app.session.execute("sequence.snap", json!({})),
                "linked" => app.session.execute("sequence.linkedSelection", json!({})),
                "marker" => app.session.execute("markers.add", json!({})),
                _ => Ok(Value::Null),
            };
        }
        if key == "settings" {
            egui::Popup::menu(&resp).show(|ui| {
                ui.checkbox(&mut app.ui.timeline.show_thumbnails, "Show Video Thumbnails");
                ui.checkbox(&mut app.ui.timeline.show_waveforms, "Show Audio Waveform");
                ui.separator();
                let mut te = app.session.state.show_through_edits;
                let c = ui.checkbox(&mut te, "Show Through Edits");
                app.auto.add("timeline.settings.showThroughEdits", c.rect, "Show Through Edits");
                if c.changed() {
                    let _ = app.session.execute("sequence.showThroughEdits", json!({"on": te}));
                }
                ui.separator();
                if ui.button("Expand All Tracks").clicked() {
                    app.ui.timeline.video_track_h = 64.0;
                    app.ui.timeline.audio_track_h = 64.0;
                }
                if ui.button("Minimize All Tracks").clicked() {
                    app.ui.timeline.video_track_h = 26.0;
                    app.ui.timeline.audio_track_h = 26.0;
                }
            });
        }
        x += 30.0;
    }
    // ----- ruler: markers row · labels · ticks · 2 pt render bar
    let clip = p.with_clip_rect(ruler);
    let marker_y = ruler.min.y + 2.0;
    let label_y = ruler.min.y + 24.0;
    let tick_base = ruler.max.y - 3.0;
    let fps = rate.as_f64();
    let base = rate.timecode_base();
    let steps: Vec<i64> = vec![
        1,
        2,
        5,
        10,
        base / 2,
        base,
        base * 2,
        base * 5,
        base * 10,
        base * 15,
        base * 30,
        base * 60,
        base * 120,
        base * 300,
        base * 600,
        base * 1800,
        base * 3600,
    ];
    let frame_px = layout.pps / fps;
    let label_step = *steps.iter().find(|s| **s as f64 * frame_px >= 110.0).unwrap_or(&(base * 3600));
    let minor = *steps.iter().find(|s| **s as f64 * frame_px >= 12.0).unwrap_or(&label_step);
    // In/Out band
    if seq.mark_in.is_some() || seq.mark_out.is_some() {
        let a = layout.x_of(seq.mark_in.unwrap_or(Tick::ZERO));
        let b = layout.x_of(seq.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(seq.duration()));
        clip.rect_filled(Rect::from_min_max(pos2(a, label_y + 7.0), pos2(b, tick_base)), 0.0, Color32::from_rgb(0x5c, 0x5c, 0x5c));
    }
    // split points: a short bracket with a V or A tag (video above, audio below)
    let sp = seq.split;
    for (t, is_in, tag, low) in
        [(sp.video_in, true, "V", false), (sp.video_out, false, "V", false), (sp.audio_in, true, "A", true), (sp.audio_out, false, "A", true)]
    {
        let Some(t) = t else { continue };
        let x = layout.x_of(if is_in { t } else { t + rate.frame_duration() });
        let y0 = if low { tick_base - 7.0 } else { label_y + 7.0 };
        let col = Color32::from_rgb(0xd0, 0xd0, 0xd0);
        let dx = if is_in { 4.0 } else { -4.0 };
        clip.line_segment([pos2(x, y0), pos2(x, y0 + 7.0)], Stroke::new(1.5, col));
        clip.line_segment([pos2(x, y0), pos2(x + dx, y0)], Stroke::new(1.5, col));
        clip.text(pos2(x + 2.0 * dx, y0 + 3.5), Align2::CENTER_CENTER, tag, Tokens::ui(8.5), col);
        let kind = if low { "audio" } else { "video" };
        let side = if is_in { "In" } else { "Out" };
        app.auto.add(&format!("timeline.split.{kind}{side}"), Rect::from_center_size(pos2(x, y0 + 3.5), vec2(12.0, 9.0)), &format!("Split {tag} {side}"));
    }
    let f0 = rate.frame_at(layout.tick_at(ruler.min.x)).max(0);
    let f1 = rate.frame_at(layout.tick_at(ruler.max.x)) + 1;
    let mut f = (f0 / minor) * minor;
    while f <= f1 {
        let x = layout.x_of(rate.tick_of(f));
        let major = f % label_step == 0;
        let h = if major { 10.0 } else { 4.0 };
        clip.line_segment([pos2(x, tick_base - h), pos2(x, tick_base)], Stroke::new(1.0, t.tl_ruler_tick));
        if major {
            let label = format_time(rate.tick_of(f), rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
            clip.text(pos2(x + 1.0, label_y), Align2::CENTER_CENTER, label, Tokens::ui(11.0), t.tl_ruler_text);
        }
        f += minor;
    }
    // render bar
    // Per segment (engine `previews`): green = rendered preview, yellow = should play in real time,
    // red = needs rendering, nothing = plays natively.
    let rb = Rect::from_min_max(pos2(ruler.min.x, ruler.max.y - 2.0), ruler.max);
    let bar = app.session.previews.bar(&app.session.project, seq_id);
    for (i, span) in bar.iter().enumerate() {
        use filmcraft_engine::previews::BarState;
        let (c, what) = match span.state {
            BarState::None => continue,
            BarState::Yellow => (t.render_yellow, "Unrendered: should play back in real time"),
            BarState::Red => (t.render_red, "Unrendered: render to play back in real time"),
            BarState::Green => (t.render_green, "Rendered preview"),
        };
        let r = Rect::from_min_max(pos2(layout.x_of(span.start), rb.min.y), pos2(layout.x_of(span.end), rb.max.y)).intersect(ruler);
        if r.width() <= 0.0 {
            continue;
        }
        clip.rect_filled(r, 0.0, c);
        let hit = r.expand2(vec2(0.0, 2.0));
        app.auto.add(&format!("timeline.renderBar.{i}"), hit, what);
        if ui.rect_contains_pointer(hit) {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new(("rb", i)), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(what);
            });
        }
    }
    // markers: 8 × 12 pt pentagons in the marker colour, on the top row
    for m in &seq.markers {
        let x = layout.x_of(m.start);
        let c = m.color.marker_rgb();
        let c = Color32::from_rgb(c[0], c[1], c[2]);
        let shape =
            vec![pos2(x - 4.0, marker_y), pos2(x + 4.0, marker_y), pos2(x + 4.0, marker_y + 8.0), pos2(x, marker_y + 12.0), pos2(x - 4.0, marker_y + 8.0)];
        if m.duration > Tick::ZERO {
            clip.rect_filled(Rect::from_min_max(pos2(x, marker_y), pos2(layout.x_of(m.start + m.duration), marker_y + 8.0)), 0.0, c.gamma_multiply(0.6));
        }
        clip.add(egui::Shape::convex_polygon(shape, c, Stroke::NONE));
        let mr = Rect::from_center_size(pos2(x, marker_y + 6.0), vec2(10.0, 13.0));
        app.auto.add(&format!("timeline.marker.{}", m.id.0), mr, &m.name);
        if ui.rect_contains_pointer(mr) && !m.name.is_empty() {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new(("mk", m.id.0)), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(&m.name);
            });
        }
    }
    app.auto.add("timeline.ruler", ruler, "time ruler");
}

fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, dur_s: f64, t: &Tokens) {
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let v = &mut app.ui.timeline;
    let total = (dur_s * 1.15).max(v.scroll + bar.width() as f64 / v.pps);
    let vis = bar.width() as f64 / v.pps;
    let a = bar.min.x + (v.scroll / total) as f32 * bar.width();
    let b = bar.min.x + ((v.scroll + vis) / total).min(1.0) as f32 * bar.width();
    let thumb = Rect::from_min_max(pos2(a, bar.min.y), pos2(b.max(a + 20.0), bar.max.y));
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { Color32::from_rgb(0x6a, 0x6a, 0x6a) } else { Color32::from_rgb(0x4b, 0x4b, 0x4b) });
    for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
        p.circle_filled(pos2(x, thumb.center().y), 4.5, t.panel_bg);
        p.circle_stroke(pos2(x, thumb.center().y), 4.5, Stroke::new(1.5, Color32::from_rgb(0xd1, 0xd1, 0xd1)));
    }
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    let resp = ui.interact(bar, egui::Id::new("tl-zoombar"), Sense::drag());
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let mode = if (pos.x - thumb.min.x).abs() < 9.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            2
        } else {
            0
        };
        app.tl.drag = Some(Drag::ZoomBar { grab: pos.x, mode });
    }
    if let Some(Drag::ZoomBar { mode, .. }) = app.tl.drag.clone()
        && resp.dragged()
    {
        let dx = resp.drag_delta().x as f64 / bar.width() as f64 * total;
        let v = &mut app.ui.timeline;
        match mode {
            0 => {
                v.target_scroll = (v.target_scroll + dx).max(0.0);
                v.scroll = v.target_scroll;
            }
            1 => {
                let end = v.scroll + vis;
                let ns = (v.scroll + dx).clamp(0.0, end - 0.05);
                v.pps = bar.width() as f64 / (end - ns);
                v.target_pps = v.pps;
                v.scroll = ns;
                v.target_scroll = ns;
            }
            _ => {
                let ne = (v.scroll + vis + dx).max(v.scroll + 0.05);
                v.pps = bar.width() as f64 / (ne - v.scroll);
                v.target_pps = v.pps;
            }
        }
    }
    if resp.drag_stopped() {
        app.tl.drag = None;
    }
}

fn vertical_scrollbar(ui: &mut egui::Ui, bar: Rect, value: &mut f32, max: f32, invert: bool, t: &Tokens, id: &str) {
    if max <= 0.5 || bar.height() < 20.0 {
        return;
    }
    let p = ui.painter();
    let frac_vis = bar.height() / (bar.height() + max);
    let h = (bar.height() * frac_vis).max(18.0);
    let pos = if invert { 1.0 - *value / max } else { *value / max };
    let y = bar.min.y + (bar.height() - h) * pos;
    let thumb = Rect::from_min_size(pos2(bar.min.x + 1.0, y), vec2(bar.width() - 2.0, h));
    p.rect_filled(thumb, 3.0, Color32::from_rgb(80, 80, 80));
    let resp = ui.interact(bar, egui::Id::new(("tl-vs", id)), Sense::drag());
    if resp.dragged() {
        let d = resp.drag_delta().y / (bar.height() - h).max(1.0) * max;
        *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
    }
    let _ = t;
}

/// How close (px) to a cut the Selection tool rolls instead of rippling when Settings ▸ Trim ▸
/// "Allow Selection tool to choose Roll and Ripple trims without modifier key" is on.
pub const ROLL_PX: f32 = 2.5;

/// The trim the Selection tool starts at an edit point: Cmd+Shift = roll, Cmd = ripple. With the
/// Trim setting on (`no_modifier`), no modifier is needed: right on the cut (when a clip is on the
/// other side) rolls, elsewhere on the edge ripples. Otherwise a plain drag is a regular trim.
pub fn selection_trim_kind(no_modifier: bool, cmd: bool, shift: bool, dist_px: f32, has_neighbour: bool) -> &'static str {
    if cmd && shift {
        "roll"
    } else if cmd {
        "ripple"
    } else if no_modifier {
        if has_neighbour && dist_px <= ROLL_PX { "roll" } else { "ripple" }
    } else {
        "trim"
    }
}

/// Distance (px) from `x` to the cut at `clip`'s `edge`, and whether a clip touches that cut on
/// the other side.
fn edge_geometry(seq: &Sequence, layout: &Layout, track: filmcraft_project::TrackId, clip: ClipId, edge: filmcraft_edit::Edge, x: f32) -> (f32, bool) {
    let Some(tr) = seq.track(track) else { return (f32::MAX, false) };
    let Some(it) = tr.item(clip) else { return (f32::MAX, false) };
    let (cut, neighbour) = match edge {
        filmcraft_edit::Edge::Out => (it.end(), tr.items.iter().any(|x| x.start == it.end())),
        filmcraft_edit::Edge::In => (it.start, tr.items.iter().any(|x| x.end() == it.start)),
    };
    ((layout.x_of(cut) - x).abs(), neighbour)
}

/// Snap `t` to nearby candidates (edits, playhead, markers, in/out). Returns snapped tick.
fn snap(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId]) -> Tick {
    if !app.session.state.snapping {
        return t;
    }
    let mut cands: Vec<Tick> = Vec::with_capacity(64);
    for tr in seq.all_tracks() {
        for it in &tr.items {
            if exclude.contains(&it.id) {
                continue;
            }
            cands.push(it.start);
            cands.push(it.end());
        }
    }
    cands.push(app.session.playhead());
    cands.extend(seq.markers.iter().map(|m| m.start));
    cands.extend(seq.mark_in);
    cands.extend(seq.mark_out);
    let x = layout.x_of(t);
    let mut best: Option<(f32, Tick)> = None;
    for c in cands {
        let d = (layout.x_of(c) - x).abs();
        if d < SNAP_PX && best.is_none_or(|b| d < b.0) {
            best = Some((d, c));
        }
    }
    match best {
        Some((_, c)) => {
            app.tl.snap_x = Some(layout.x_of(c));
            c
        }
        None => t,
    }
}

/// What is at a screen point.
#[derive(Clone, Debug)]
pub enum Hit {
    Ruler,
    Clip { track: TrackId, clip: ClipId, edge: Option<filmcraft_edit::Edge> },
    Transition { track: TrackId, id: filmcraft_project::TransitionId },
    Empty { track: TrackId },
    None,
}

pub fn hit(seq: &Sequence, layout: &Layout, pos: Pos2) -> Hit {
    if layout.ruler.contains(pos) {
        return Hit::Ruler;
    }
    if !layout.content.contains(pos) {
        return Hit::None;
    }
    let Some(row) = layout.row_at(pos.y) else { return Hit::None };
    let tr = seq.track(row.track).expect("track");
    for trn in &tr.transitions {
        let x0 = layout.x_of(trn.start);
        let x1 = layout.x_of(trn.end());
        if pos.x >= x0 && pos.x <= x1 && pos.y > row.rect.min.y + 17.0 {
            return Hit::Transition { track: row.track, id: trn.id };
        }
    }
    let t = layout.tick_at(pos.x);
    let edge_px = 7.0f32;
    // prefer edges
    for it in &tr.items {
        let x0 = layout.x_of(it.start);
        let x1 = layout.x_of(it.end());
        let w = x1 - x0;
        let e = edge_px.min(w / 3.0);
        if (pos.x - x0).abs() <= e && pos.x >= x0 - e {
            return Hit::Clip { track: row.track, clip: it.id, edge: Some(filmcraft_edit::Edge::In) };
        }
        if (pos.x - x1).abs() <= e && pos.x <= x1 + e {
            return Hit::Clip { track: row.track, clip: it.id, edge: Some(filmcraft_edit::Edge::Out) };
        }
    }
    if let Some(it) = tr.item_at(t) {
        return Hit::Clip { track: row.track, clip: it.id, edge: None };
    }
    Hit::Empty { track: row.track }
}

pub fn hit_json(app: &FilmcraftApp, pos: Pos2) -> Value {
    let (Some(layout), Some(seq)) = (app.tl.layout.as_ref(), app.session.active_sequence()) else { return json!({"hit": "none"}) };
    let t = layout.tick_at(pos.x);
    match hit(seq, layout, pos) {
        Hit::Ruler => json!({"hit": "ruler", "time": t.0}),
        Hit::Clip { track, clip, edge } => json!({"hit": "clip", "track": track.0, "clip": clip.0, "edge": edge.map(|e| format!("{e:?}")), "time": t.0}),
        Hit::Transition { track, id } => json!({"hit": "transition", "track": track.0, "transition": id.0}),
        Hit::Empty { track } => json!({"hit": "empty", "track": track.0, "time": t.0}),
        Hit::None => json!({"hit": "none"}),
    }
}

/// Screen point of a clip (centre, or its in/out edge).
pub fn locate(app: &FilmcraftApp, clip: u64, edge: Option<&str>) -> Option<(f32, f32)> {
    let layout = app.tl.layout.as_ref()?;
    let seq = app.session.active_sequence()?;
    let (tid, it) = seq.find_item(ClipId(clip))?;
    let row = layout.rows.iter().find(|r| r.track == tid)?;
    let y = row.rect.center().y;
    let x = match edge {
        Some("in") => layout.x_of(it.start) + 2.0,
        Some("out") => layout.x_of(it.end()) - 2.0,
        _ => (layout.x_of(it.start) + layout.x_of(it.end())) / 2.0,
    };
    Some((x, y))
}

fn preview_drag(app: &FilmcraftApp, seq: &Sequence, _layout: &Layout, out: &mut HashMap<ClipId, (Tick, Tick, Option<TrackId>)>) {
    let Some(d) = &app.tl.drag else { return };
    match d {
        Drag::Move { clips, offset, track_delta, .. } => {
            for c in clips {
                if let Some((tid, it)) = seq.find_item(*c) {
                    let dest = shift_track(seq, tid, *track_delta);
                    out.insert(*c, ((it.start + *offset).max(Tick::ZERO), it.duration, dest));
                }
            }
        }
        Drag::Remix { clip, delta } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                out.insert(*clip, (it.start, it.duration + *delta, None));
            }
        }
        Drag::Trim { clip, edge, delta, .. } | Drag::Stretch { clip, edge, delta } => {
            let ids = filmcraft_engine::commands::with_links(&app.session, &[*clip]);
            for c in ids {
                if let Some((_, it)) = seq.find_item(c) {
                    let v = match edge {
                        filmcraft_edit::Edge::In => (it.start + *delta, it.duration - *delta, None),
                        filmcraft_edit::Edge::Out => (it.start, it.duration + *delta, None),
                    };
                    out.insert(c, v);
                }
            }
        }
        Drag::Roll { left, right, delta } => {
            if let Some((_, l)) = seq.find_item(*left) {
                out.insert(*left, (l.start, l.duration + *delta, None));
            }
            if let Some((_, r)) = seq.find_item(*right) {
                out.insert(*right, (r.start + *delta, r.duration - *delta, None));
            }
        }
        Drag::Slide { clip, delta } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                out.insert(*clip, (it.start + *delta, it.duration, None));
            }
        }
        _ => {}
    }
}

fn shift_track(seq: &Sequence, tid: TrackId, delta: i32) -> Option<TrackId> {
    if delta == 0 {
        return Some(tid);
    }
    for tracks in [&seq.video_tracks, &seq.audio_tracks] {
        if let Some(i) = tracks.iter().position(|t| t.id == tid) {
            let ni = (i as i32 + delta).clamp(0, tracks.len() as i32 - 1) as usize;
            return Some(tracks[ni].id);
        }
    }
    Some(tid)
}

fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, rect: Rect) {
    let ctx = ui.ctx().clone();
    let area = Rect::from_min_max(pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
    let resp = ui.interact(area, egui::Id::new("timeline-area"), Sense::click_and_drag());
    let pos = resp.hover_pos().or(resp.interact_pointer_pos());
    let mods = ctx.input(|i| i.modifiers);
    let tool = app.ui.tool;
    let rate = seq.settings.frame_rate;

    // ---- wheel: horizontal scroll; Alt/Cmd+wheel zoom about cursor; Shift+wheel vertical
    if ui.rect_contains_pointer(rect) {
        let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        if let Some(p) = ctx.pointer_hover_pos() {
            if (zoom - 1.0).abs() > 1e-4 || ((mods.alt || mods.command) && scroll.y.abs() > 0.0) {
                let f = if (zoom - 1.0).abs() > 1e-4 { zoom as f64 } else { (1.0 + scroll.y as f64 * 0.01).clamp(0.5, 2.0) };
                let anchor_t = layout.tick_at(p.x).seconds();
                let v = &mut app.ui.timeline;
                v.target_pps = (v.target_pps * f).clamp(0.05, 24_000.0);
                app.tl.zoom_anchor = Some((anchor_t, p.x));
            } else if mods.shift && scroll.y.abs() > 0.0 {
                if layout.rows.iter().find(|r| r.kind == TrackKind::Video).is_some_and(|r| p.y < r.rect.max.y + 200.0) && p.y < layout.content.center().y {
                    app.ui.timeline.v_scroll -= scroll.y;
                } else {
                    app.ui.timeline.a_scroll -= scroll.y;
                }
            } else if scroll.x.abs() > 0.0 || scroll.y.abs() > 0.0 {
                let d = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
                let v = &mut app.ui.timeline;
                v.target_scroll = (v.target_scroll - d as f64 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        }
    }

    // ---- cursor feedback
    if app.tl.drag.is_none()
        && let Some(p) = pos
        && area.contains(p)
    {
        let h = hit(seq, layout, p);
        let cur = match (tool, &h) {
            (Tool::Selection, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeColumn,
            (Tool::Ripple | Tool::Rolling | Tool::RateStretch, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeColumn,
            (Tool::Remix, Hit::Clip { edge: Some(filmcraft_edit::Edge::Out), .. }) => CursorIcon::ResizeColumn,
            (Tool::Razor, Hit::Clip { .. }) => CursorIcon::Crosshair,
            (Tool::Slip | Tool::Slide, Hit::Clip { .. }) => CursorIcon::ResizeHorizontal,
            (Tool::Hand, _) => CursorIcon::Grab,
            (Tool::Zoom, _) => CursorIcon::ZoomIn,
            _ => CursorIcon::Default,
        };
        ctx.set_cursor_icon(cur);
        // razor preview line
        if tool == Tool::Razor
            && let Hit::Clip { track, .. } = h
            && let Some(row) = layout.rows.iter().find(|r| r.track == track)
        {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x)), &[]);
            let x = layout.x_of(t);
            ui.painter().line_segment([pos2(x, row.rect.min.y), pos2(x, row.rect.max.y)], Stroke::new(1.0, Color32::WHITE));
        }
    }

    // ---- press
    if resp.drag_started() || (resp.clicked() && app.tl.drag.is_none()) {
        let Some(p) = resp.interact_pointer_pos() else { return };
        let h = hit(seq, layout, p);
        let t = layout.tick_at(p.x);
        let started = match (tool, h.clone()) {
            (_, Hit::Ruler) => Some(Drag::Scrub),
            (Tool::Hand, _) => Some(Drag::Pan { last: p }),
            (Tool::Zoom, _) => {
                if resp.clicked() {
                    let f = if mods.alt { 1.0 / 2.0 } else { 2.0 };
                    app.ui.timeline.target_pps = (app.ui.timeline.target_pps * f).clamp(0.05, 24_000.0);
                    app.tl.zoom_anchor = Some((t.seconds(), p.x));
                }
                None
            }
            (Tool::Razor, Hit::Clip { clip, .. }) => {
                if resp.clicked() || resp.drag_started() {
                    let tt = snap(app, seq, layout, rate.snap_nearest(t), &[]);
                    let r = if mods.shift {
                        app.session.execute("timeline.razor", json!({"time": tt.0}))
                    } else {
                        app.session.execute("timeline.razor", json!({"time": tt.0, "clip": clip.0}))
                    };
                    if let Err(e) = r {
                        app.ui.status = e.to_string();
                    }
                }
                None
            }
            (Tool::TrackSelectForward | Tool::TrackSelectBackward, Hit::Clip { track, .. } | Hit::Empty { track }) => {
                let fwd = tool == Tool::TrackSelectForward;
                let ids: Vec<u64> = seq
                    .all_tracks()
                    .filter(|tr| !mods.shift || tr.id == track)
                    .flat_map(|tr| tr.items.iter())
                    .filter(|i| if fwd { i.end() > t } else { i.start < t })
                    .map(|i| i.id.0)
                    .collect();
                let _ = app.session.execute("timeline.select", json!({"clips": ids}));
                None
            }
            (Tool::Selection | Tool::Ripple | Tool::Rolling, Hit::Clip { clip, edge: Some(edge), track }) if resp.clicked() => {
                // click an edge = select it as an edit point (trim mode); Shift adds
                let kind = match tool {
                    Tool::Rolling => "roll",
                    Tool::Ripple => "ripple",
                    _ => {
                        let (dist, neighbour) = edge_geometry(seq, layout, track, clip, edge, p.x);
                        selection_trim_kind(app.session.prefs.trim.selection_tool_roll_ripple, mods.command, mods.shift, dist, neighbour)
                    }
                };
                let e = if edge == filmcraft_edit::Edge::In { "in" } else { "out" };
                let r = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": e, "kind": kind, "add": mods.shift && !mods.command}));
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                }
                None
            }
            (Tool::Selection | Tool::Ripple | Tool::Rolling | Tool::RateStretch, Hit::Clip { clip, edge: Some(edge), track }) => {
                let tr = seq.track(track).expect("track");
                let sel_kind = if tool == Tool::Selection {
                    let (dist, neighbour) = edge_geometry(seq, layout, track, clip, edge, p.x);
                    selection_trim_kind(app.session.prefs.trim.selection_tool_roll_ripple, mods.command, mods.shift, dist, neighbour)
                } else {
                    ""
                };
                let mode = if tool == Tool::Ripple || sel_kind == "ripple" { filmcraft_edit::TrimMode::Ripple } else { filmcraft_edit::TrimMode::Regular };
                if tool == Tool::Rolling || sel_kind == "roll" {
                    // roll the cut between this and its neighbour
                    let it = tr.item(clip).expect("clip");
                    let (l, r) = match edge {
                        filmcraft_edit::Edge::Out => (Some(clip), tr.items.iter().find(|x| x.start == it.end()).map(|x| x.id)),
                        filmcraft_edit::Edge::In => (tr.items.iter().find(|x| x.end() == it.start).map(|x| x.id), Some(clip)),
                    };
                    match (l, r) {
                        (Some(left), Some(right)) => Some(Drag::Roll { left, right, delta: Tick::ZERO }),
                        _ => Some(Drag::Trim { clip, edge, mode, delta: Tick::ZERO }),
                    }
                } else if tool == Tool::RateStretch {
                    Some(Drag::Stretch { clip, edge, delta: Tick::ZERO })
                } else {
                    Some(Drag::Trim { clip, edge, mode, delta: Tick::ZERO })
                }
            }
            (Tool::Remix, Hit::Clip { clip, edge: Some(filmcraft_edit::Edge::Out), .. }) => Some(Drag::Remix { clip, delta: Tick::ZERO }),
            (Tool::Slip, Hit::Clip { clip, .. }) => Some(Drag::Slip { clip, delta: Tick::ZERO }),
            (Tool::Slide, Hit::Clip { clip, .. }) => Some(Drag::Slide { clip, delta: Tick::ZERO }),
            (_, Hit::Clip { clip, track, .. }) => {
                // select (shift toggles; alt selects one side of a link)
                let sel = &app.session.state.selection;
                if mods.shift {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0], "toggle": true}));
                } else if !sel.contains(&clip) {
                    if mods.alt {
                        app.session.state.selection = vec![clip];
                    } else {
                        let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                    }
                }
                if resp.drag_started() {
                    let clips = app.session.state.selection.clone();
                    Some(Drag::Move { clips, grab_tick: t, start_track: track, offset: Tick::ZERO, track_delta: 0 })
                } else {
                    None
                }
            }
            (_, Hit::Transition { .. }) => None,
            (_, Hit::Empty { .. }) => {
                if resp.drag_started() {
                    Some(Drag::Marquee { start: p })
                } else {
                    app.session.state.selection.clear();
                    app.session.state.edit_points.clear();
                    None
                }
            }
            (_, Hit::None) => None,
        };
        if let Some(d) = started {
            if matches!(d, Drag::Scrub) {
                app.stop();
                let tt = rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO));
                app.session.set_playhead(tt);
            }
            if resp.drag_started() {
                app.tl.drag = Some(d);
            }
        }
    }

    // ---- drag
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(d) = app.tl.drag.clone()
    {
        let t_here = layout.tick_at(p.x);
        let new = match d {
            Drag::Scrub => {
                let mut tt = rate.snap_nearest(t_here.max(Tick::ZERO));
                // Shift snaps; Settings ▸ Timeline ▸ "Snap playhead in Timeline when Snap is enabled"
                if mods.shift || app.session.prefs.timeline.snap_playhead {
                    tt = snap(app, seq, layout, tt, &[]);
                }
                app.session.set_playhead(tt);
                Some(Drag::Scrub)
            }
            Drag::Pan { last } => {
                let dx = p.x - last.x;
                let v = &mut app.ui.timeline;
                v.target_scroll = (v.target_scroll - dx as f64 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
                app.ui.timeline.v_scroll += p.y - last.y;
                Some(Drag::Pan { last: p })
            }
            Drag::Move { clips, grab_tick, start_track, .. } => {
                let raw = rate.snap_nearest(t_here - grab_tick);
                // snap the moved block's start or end
                let first = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.start)).min().unwrap_or_default();
                let last = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.end())).max().unwrap_or_default();
                let s1 = snap(app, seq, layout, first + raw, &clips) - first;
                let offset = if s1 != raw { s1 } else { snap(app, seq, layout, last + raw, &clips) - last };
                let offset = offset.max(-first);
                let cur_row = layout.row_at(p.y);
                let start_row = layout.rows.iter().find(|r| r.track == start_track);
                let track_delta = match (cur_row, start_row) {
                    (Some(c), Some(s)) if c.kind == s.kind => c.index as i32 - s.index as i32,
                    _ => 0,
                };
                Some(Drag::Move { clips, grab_tick, start_track, offset, track_delta })
            }
            Drag::Trim { clip, edge, mode, .. } => {
                let (_, it) = seq.find_item(clip).expect("clip");
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[clip]);
                let delta = target - base;
                Some(Drag::Trim { clip, edge, mode, delta })
            }
            Drag::Remix { clip, .. } => {
                let (_, it) = seq.find_item(clip).expect("clip");
                let target = rate.snap_nearest(t_here).max(it.start + rate.frame_duration());
                Some(Drag::Remix { clip, delta: target - it.end() })
            }
            Drag::Stretch { clip, edge, .. } => {
                let (_, it) = seq.find_item(clip).expect("clip");
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[clip]);
                Some(Drag::Stretch { clip, edge, delta: target - base })
            }
            Drag::Roll { left, right, .. } => {
                let (_, l) = seq.find_item(left).expect("clip");
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[left, right]);
                Some(Drag::Roll { left, right, delta: target - l.end() })
            }
            Drag::Slip { clip, .. } => {
                let start = resp.interact_pointer_pos().map(|_| ()).and(ctx.input(|i| i.pointer.press_origin()));
                let origin = start.map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slip { clip, delta: -(rate.snap_nearest(t_here - origin)) })
            }
            Drag::Slide { clip, .. } => {
                let origin = ctx.input(|i| i.pointer.press_origin()).map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slide { clip, delta: rate.snap_nearest(t_here - origin) })
            }
            Drag::Marquee { start } => {
                let r = Rect::from_two_pos(start, p);
                ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(18));
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(140)), StrokeKind::Inside);
                Some(Drag::Marquee { start })
            }
            other => Some(other),
        };
        app.tl.drag = new;
        // auto-scroll when dragging near the edges
        if !matches!(app.tl.drag, Some(Drag::Pan { .. }) | Some(Drag::ZoomBar { .. })) {
            let v = &mut app.ui.timeline;
            if p.x > layout.content.max.x - 20.0 {
                v.target_scroll += 6.0 / v.pps;
                v.scroll = v.target_scroll;
            } else if p.x < layout.content.min.x + 10.0 && v.scroll > 0.0 {
                v.target_scroll = (v.target_scroll - 6.0 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        }
    }

    // ---- release: commit exactly one command
    if resp.drag_stopped()
        && let Some(d) = app.tl.drag.take()
    {
        let r = match d {
            Drag::Move { clips, offset, track_delta, .. } if offset != Tick::ZERO || track_delta != 0 => {
                let moves: Vec<Value> = clips
                    .iter()
                    .filter_map(|c| seq.find_item(*c).map(|(tid, it)| json!({"clip": c.0, "track": shift_track(seq, tid, track_delta).unwrap_or(tid).0, "time": (it.start + offset).max(Tick::ZERO).0})))
                    .collect();
                Some(app.session.execute("timeline.move", json!({"moves": moves, "insert": mods.command})))
            }
            Drag::Trim { clip, edge, mode, delta } if delta != Tick::ZERO => Some(app.session.execute(
                "timeline.trim",
                json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "mode": if mode == filmcraft_edit::TrimMode::Ripple {"ripple"} else {"regular"}, "delta": delta.0}),
            )),
            Drag::Stretch { clip, edge, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.rateStretch", json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "delta": delta.0}))),
            Drag::Remix { clip, delta } if delta != Tick::ZERO => {
                let d = seq.find_item(clip).map(|(_, it)| it.duration + delta).unwrap_or(Tick::ZERO);
                Some(app.session.execute("clip.remix", json!({"clip": clip.0, "duration": d.0})))
            }
            Drag::Roll { left, right, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.roll", json!({"left": left.0, "right": right.0, "delta": delta.0}))),
            Drag::Slip { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slip", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Slide { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slide", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Marquee { start } => {
                if let Some(end) = resp.interact_pointer_pos() {
                    let r = Rect::from_two_pos(start, end);
                    let (a, b) = (layout.tick_at(r.min.x), layout.tick_at(r.max.x));
                    let ids: Vec<u64> = layout
                        .rows
                        .iter()
                        .filter(|row| row.rect.intersects(r))
                        .filter_map(|row| seq.track(row.track))
                        .flat_map(|tr| tr.items.iter().filter(|i| i.start < b && i.end() > a).map(|i| i.id.0))
                        .collect();
                    Some(app.session.execute("timeline.select", json!({"clips": ids, "add": mods.shift})))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(Err(e)) = r {
            app.ui.status = e.to_string();
        }
    }

    // ---- context menu on clips
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        let sel_n = app.session.state.selection.len();
        let items: [(&str, &str); 11] = [
            ("Cut", "edit.cut"),
            ("Copy", "edit.copy"),
            ("Clear", "edit.clear"),
            ("Ripple Delete", "edit.rippleDelete"),
            ("Enable", "clip.enable"),
            ("Unlink", "clip.link"),
            ("Group", "clip.group"),
            ("Speed/Duration…", "clip.speedDuration"),
            ("Nest…", "clip.nest"),
            ("Scale to Frame Size", "clip.scaleToFrameSize"),
            ("Join Through Edits", "sequence.joinThroughEdits"),
        ];
        for (label, cmd) in items {
            if ui.add_enabled(sel_n > 0 && app.session.is_enabled(cmd), egui::Button::new(label)).clicked() {
                if cmd == "clip.speedDuration" {
                    app.dialog = None;
                    let _ = app.session.execute(cmd, json!({"speed": 50.0}));
                } else if let Err(e) = app.session.execute(cmd, json!({})) {
                    app.ui.status = e.to_string();
                }
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button("Label", |ui| {
            for l in filmcraft_project::Label::ALL {
                if ui.button(l.name()).clicked() {
                    let _ = app.session.execute("edit.label", json!({"label": l.name()}));
                    ui.close();
                }
            }
        });
    });

    // ---- drops: project items and effects
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
    {
        let row = layout.row_at(p.y).cloned();
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let still = app.session.prefs.timeline.still_duration(rate);
        let is_still = app.session.project.item(item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == filmcraft_media::MediaKind::Still);
        let dur = app.session.project.item(item).map(|i| i.duration()).filter(|d| d.0 > 0 && !is_still).unwrap_or(still);
        if let Some(row) = &row {
            let r = Rect::from_min_max(pos2(layout.x_of(t), row.rect.min.y + 1.0), pos2(layout.x_of(t + dur), row.rect.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            if mods.command {
                ui.painter().text(r.left_top() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, "Insert", Tokens::ui(10.0), Color32::WHITE);
            }
        }
        if ctx.input(|i| i.pointer.any_released())
            && let Some(row) = row
        {
            let (vt, at) = match row.kind {
                TrackKind::Video => (Some(row.track.0), seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0)),
                TrackKind::Audio => (seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0), Some(row.track.0)),
            };
            let r = app.session.execute("timeline.place", json!({"item": item.0, "track": vt, "audioTrack": at, "time": t.0, "insert": mods.command}));
            if let Err(e) = r {
                app.ui.status = e.to_string();
            }
            crate::panels::clear_drag(ui);
        }
    }
    // graphics templates from Essential Graphics ▸ Browse: placed on the video track under the pointer
    if let Some(template) = crate::panels::dragged_template(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
    {
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let row = layout.row_at(p.y).cloned().filter(|r| r.kind == TrackKind::Video);
        if let Some(row) = &row {
            let r = Rect::from_min_max(pos2(layout.x_of(t), row.rect.min.y + 1.0), pos2(layout.x_of(t) + 60.0, row.rect.max.y - 1.0));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
        }
        if ctx.input(|i| i.pointer.any_released()) {
            let mut q = json!({"template": template, "time": t.0});
            if let Some(row) = row {
                q["track"] = json!(row.index);
            }
            if let Err(e) = app.session.execute("graphics.template.apply", q) {
                app.ui.status = e.to_string();
            }
            crate::panels::clear_drag(ui);
        }
    }
    if let Some(effect) = crate::panels::dragged_effect(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
        && let Hit::Clip { clip, .. } = hit(seq, layout, p)
        && let Some((tid, it)) = seq.find_item(clip)
        && let Some(row) = layout.rows.iter().find(|r| r.track == tid)
    {
        {
            let r = Rect::from_min_max(pos2(layout.x_of(it.start), row.rect.min.y), pos2(layout.x_of(it.end()), row.rect.max.y));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
            if ctx.input(|i| i.pointer.any_released()) {
                let is_transition = filmcraft_project::find_effect(&effect)
                    .is_some_and(|d| matches!(d.kind, filmcraft_project::EffectKind::VideoTransition | filmcraft_project::EffectKind::AudioTransition));
                let r = if let Some(name) = effect.strip_prefix("preset:") {
                    app.session.execute("presets.apply", json!({"preset": name, "clips": [clip.0]}))
                } else if is_transition {
                    let edge = if p.x - layout.x_of(it.start) < layout.x_of(it.end()) - p.x { "in" } else { "out" };
                    app.session.execute("effects.apply", json!({"effect": effect, "clip": clip.0, "edge": edge}))
                } else {
                    app.session.execute("effects.apply", json!({"effect": effect, "clips": [clip.0]}))
                };
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                } else {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                }
                crate::panels::clear_drag(ui);
            }
        }
    }
    app.auto.add("timeline.tracks", layout.content, "tracks");
}

#[cfg(test)]
mod waveform_tests {
    use super::waveform_display_gain;

    fn db(g: f32) -> f32 {
        20.0 * g.log10()
    }

    #[test]
    fn normalisation_is_capped_for_near_silent_sources() {
        // a −14 dBFS recording is normalised to full scale (+12 dB cap reached: −2 dBFS)
        assert!((db(0.2 * waveform_display_gain(0.2, 0.0)) - (-2.0)).abs() < 0.1);
        // a −3 dBFS recording is normalised exactly
        assert!((db(0.708 * waveform_display_gain(0.708, 0.0))).abs() < 0.1);
        // a −52 dBFS "silent" stream stays near the floor of the −48 dB display range
        assert!(db(0.0025 * waveform_display_gain(0.0025, 0.0)) < -39.0);
        // digital silence does not blow up
        assert!(waveform_display_gain(0.0, 0.0).is_finite());
        // clip gain still applies
        assert!((db(waveform_display_gain(1.0, -6.0)) - (-6.0)).abs() < 0.1);
    }
}
