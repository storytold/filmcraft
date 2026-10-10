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

use super::timeline_hit::{EdgeKind, Grab, edge_geometry, grab_at};
pub use super::timeline_hit::{Hit, ROLL_PX, hit, selection_trim_kind};

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
    /// What the primary button went down on in the Timeline (#259); see [`Pressed`].
    pub pressed: Option<Pressed>,
    /// Last layout (for hit-testing from the control channel).
    pub layout: Option<Layout>,
    /// Snap indicator x (screen) this frame.
    snap_x: Option<f32>,
    pub peaks: Arc<Mutex<HashMap<ItemId, Arc<Vec<(f32, f32)>>>>>,
    peaks_pending: Arc<Mutex<Vec<ItemId>>>,
    /// Nested sequences have waveforms too, of their mix. Unlike media a sequence changes: this is
    /// the content key (see [`sequence_audio_key`]) the cached peaks of each sequence were made
    /// from, and the key worked out for the session revision last seen.
    seq_peak_keys: Arc<Mutex<HashMap<ItemId, u64>>>,
    seq_keys_seen: HashMap<ItemId, (u64, u64)>,
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
        self.seq_peak_keys = Default::default();
        self.seq_keys_seen.clear();
        self.peak_max.clear();
    }
}

/// What was under the pointer when the primary button went down in the Timeline, worked out on
/// that frame (#259). egui reports a click when the button comes up, by when it has forgotten the
/// press point, and a drag only once the pointer has moved past its 6 pt drag threshold, nearly
/// the 7 px edge zone. Both act on this instead, so a press acts on what it pressed.
#[derive(Clone, Copy, Debug)]
pub struct Pressed {
    pub pos: Pos2,
    /// The time under the pointer.
    pub tick: Tick,
    pub grab: Grab,
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
        /// The time under the pointer when the button went down: the edge moves by how far the
        /// pointer has moved since (#259).
        from: Tick,
    },
    Roll {
        left: ClipId,
        right: ClipId,
        delta: Tick,
        /// As in [`Drag::Trim`].
        from: Tick,
    },
    Slip {
        clip: ClipId,
        delta: Tick,
    },
    Slide {
        clip: ClipId,
        delta: Tick,
    },
    /// An audio clip's Volume line (see [`super::timeline_volume`]).
    Volume(super::timeline_volume::LineDrag),
    Stretch {
        clip: ClipId,
        edge: filmcraft_edit::Edge,
        delta: Tick,
        /// As in [`Drag::Trim`].
        from: Tick,
    },
    /// Remix tool: drag a music clip's Out edge; on release `clip.remix` re-plans it to the new duration.
    Remix {
        clip: ClipId,
        delta: Tick,
        /// As in [`Drag::Trim`].
        from: Tick,
    },
    /// A transition dragged by an end (`edge`: its duration) or its middle (slides it over the
    /// cut); `start` / `duration` are where it is shown now, committed on release (#224).
    Transition {
        id: filmcraft_project::TransitionId,
        edge: Option<filmcraft_edit::Edge>,
        /// As in [`Drag::Trim`].
        from: Tick,
        start: Tick,
        duration: Tick,
    },
    Pan {
        last: Pos2,
    },
    Marquee {
        start: Pos2,
    },
    Divider,
    ZoomBar {
        /// Where the pointer was pressed, and the view's left edge (seconds) at that moment.
        grab: f32,
        start: f64,
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
    /// An audio track showing a track keyframe lane, which hides its clips' Volume lines.
    pub lane: bool,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub content: Rect,
    pub ruler: Rect,
    pub rows: Vec<Row>,
    pub pps: f64,
    pub scroll: f64,
    /// The y of the divider between the video tracks (above) and the audio tracks (below).
    pub split_y: f32,
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

/// How far the Timeline reaches, in seconds: ten minutes past the end of the sequence. Premiere
/// Pro's Timeline does the same (on a 6 s sequence its view stops with 00:10:06 at the right
/// edge). The view cannot be scrolled past it and the scroll bar spans it.
pub fn timeline_extent(sequence_seconds: f64) -> f64 {
    sequence_seconds.max(0.0) + 600.0
}

/// The latest time the left edge of a view `width` points wide can show at `pps` points a second.
pub fn max_scroll(sequence_seconds: f64, width: f32, pps: f64) -> f64 {
    if pps <= 0.0 || !pps.is_finite() {
        return 0.0;
    }
    (timeline_extent(sequence_seconds) - width as f64 / pps).max(0.0)
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
    // A damaged project can name an active sequence that no longer exists.
    let Some(seq) = app.session.active_sequence().cloned() else {
        empty_state(app, ui, rect);
        return;
    };
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
    // The view stays inside the Timeline's extent, however it was moved (wheel, scroll bar, zoom,
    // a script): scrolled past it, nothing is in view and the scroll bar has nowhere to show it.
    {
        let v = &mut app.ui.timeline;
        let seconds = seq.duration().seconds();
        if !v.scroll.is_finite() || !v.target_scroll.is_finite() {
            (v.scroll, v.target_scroll) = (0.0, 0.0);
        }
        v.scroll = v.scroll.clamp(0.0, max_scroll(seconds, content.width(), v.pps));
        v.target_scroll = v.target_scroll.clamp(0.0, max_scroll(seconds, content.width(), v.target_pps));
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
        let rect = Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + vh));
        rows.push(Row { track: tr.id, kind: TrackKind::Video, index: i, rect, lane: false });
    }
    let na = seq.audio_tracks.len();
    let max_as = (na as f32 * ah - audio_area.height()).max(0.0);
    app.ui.timeline.a_scroll = app.ui.timeline.a_scroll.clamp(0.0, max_as);
    for (i, tr) in seq.audio_tracks.iter().enumerate() {
        let top = audio_area.min.y + i as f32 * ah - app.ui.timeline.a_scroll;
        let rect = Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + ah));
        rows.push(Row { track: tr.id, kind: TrackKind::Audio, index: i, rect, lane: app.ui.timeline.track_lanes.contains_key(&tr.id.0) });
    }
    let layout = Layout { content, ruler, rows: rows.clone(), pps, scroll, split_y };
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
        let Some(tr) = seq.track(r.track) else { continue };
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip_rect);
        if row.height() <= 0.0 {
            continue;
        }
        let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, row.min.y), pos2(content.max.x, row.max.y)));
        let volume_line = r.kind == TrackKind::Audio && !r.lane;
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
            if volume_line {
                super::timeline_volume::paint(&p, body, it);
            }
            app.auto.add(&format!("timeline.clip.{}", it.id.0), body.intersect(content), &it.name);
            // a nest that runs past the end of its sequence's contents: that part is empty
            if !previews.contains_key(&it.id)
                && let Some(empty) = app.session.project.nest_overhang(it)
            {
                let er = Rect::from_min_max(pos2(layout.x_of(empty.start), body.min.y), pos2(layout.x_of(empty.end()), body.max.y)).intersect(body);
                if er.width() > 0.0 {
                    p.with_clip_rect(er.intersect(p.clip_rect())).rect_filled(er, 0.0, Color32::from_black_alpha(110));
                    paint_hatch(&p, er);
                    app.auto.add(&format!("timeline.clip.{}.empty", it.id.0), er.intersect(content), "past the end of the nested sequence");
                }
            }
        }
        // items dragged onto this track from another
        for (cid, (start, dur, mt)) in &previews {
            if *mt == Some(r.track)
                && tr.item(*cid).is_none()
                && let Some((_, it)) = seq.find_item(*cid)
            {
                let body = Rect::from_min_max(pos2(layout.x_of(*start), r.rect.min.y + 1.0), pos2(layout.x_of(*start + *dur), r.rect.max.y - 1.0));
                draw_clip(app, &ctx, &p, body, it, r.kind, true, &t, rate);
                if volume_line {
                    super::timeline_volume::paint(&p, body, it);
                }
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
        // transitions (one being dragged where the drag puts it; the selected ones outlined)
        for trn in &tr.transitions {
            let (start, end) = match &app.tl.drag {
                Some(Drag::Transition { id, start, duration, .. }) if *id == trn.id => (*start, *start + *duration),
                _ => (trn.start, trn.end()),
            };
            let x0 = layout.x_of(start);
            let x1 = layout.x_of(end);
            let tr_rect = Rect::from_min_max(pos2(x0, r.rect.min.y + 17.0), pos2(x1, r.rect.max.y - 1.0));
            let selected = app.session.state.transition_selection.contains(&trn.id);
            draw_transition(&p, tr_rect, trn, selected, &t);
            app.auto.add(&format!("timeline.transition.{}", trn.id.0), tr_rect, &trn.effect.effect);
            // its ends, for dragging the duration (the same zones as the hit test)
            let zone = super::timeline_hit::EDGE_PX.min(tr_rect.width() / 3.0);
            app.auto.add(&format!("timeline.transition.{}.in", trn.id.0), Rect::from_min_max(tr_rect.min, pos2(x0 + zone, tr_rect.max.y)), "start");
            app.auto.add(&format!("timeline.transition.{}.out", trn.id.0), Rect::from_min_max(pos2(x1 - zone, tr_rect.min.y), tr_rect.max), "end");
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
    painter.text(pos2(rect.min.x + 44.0, master.center().y), Align2::LEFT_CENTER, tl!("Mix"), Tokens::ui(11.5), t.text);
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
    zoom_scrollbar(
        app,
        ui,
        Rect::from_min_max(pos2(content.min.x, rect.max.y - SCROLLBAR_H + 2.0), pos2(content.max.x, rect.max.y - 2.0)),
        seq.duration().seconds(),
        &t,
    );
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
    super::timeline_volume::interact(app, ui, &seq, &layout, aclip);
    // track keyframes (drawn and edited on top of the clips)
    super::timeline_automation::show(app, ui, &seq, &layout, aclip);
    if cap_n > 0 {
        super::timeline_captions::interact(app, ui, &seq, caption_area, &layout);
    }
    let _ = (visible, TICKS_PER_SECOND);
}

fn empty_state(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, tl!("Drop media here to create sequence."), Tokens::ui(13.0), t.text_dim);
    if app.session.project.items.values().any(|i| matches!(i.kind, filmcraft_project::ItemKind::Sequence(_))) {
        let hint = tl!("Double-click a sequence in the Project panel to open it here.");
        ui.painter().text(rect.center() + vec2(0.0, 50.0), Align2::CENTER_CENTER, hint, Tokens::ui(12.0), t.text_dim);
    }
    let b = Rect::from_center_size(rect.center() + vec2(0.0, 20.0), vec2(170.0, 26.0));
    let resp = ui.interact(b, egui::Id::new("tl-open-demo"), Sense::click());
    app.auto.add("timeline.openDemo", b, "Open Demo Project");
    ui.painter().rect_filled(b, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(b.center(), Align2::CENTER_CENTER, tl!("Open Demo Project"), Tokens::semibold(12.0), Color32::WHITE);
    if resp.clicked() {
        let _ = app.session.execute("file.openDemoProject", json!({}));
    }
    // accept drops from the project panel
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && ui.rect_contains_pointer(rect)
        && ui.input(|i| i.pointer.any_released())
    {
        // a multi-selection makes the sequence from its first item and follows it, back to back, in one undo step
        let items: Vec<u64> = crate::panels::dragged_selection(app, item).iter().map(|i| i.0).collect();
        if let Err(e) = app.session.execute("file.newSequence", json!({"fromItems": items})) {
            app.ui.status = e.to_string();
        }
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
        t.disabled_clip_bg
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
        // as in Premiere, the waveform shows the clip's Volume
        m *= super::timeline_volume::gain_at(it, t0);
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
}

/// Display gain for a clip's waveform: clip gain, plus normalisation of the source to full scale
/// so quiet recordings stay readable, but by at most +12 dB, so a near-silent track (room tone at
/// −50 dBFS, a silent film's empty audio stream) does not look like a loud one.
fn waveform_display_gain(source_peak: f32, clip_gain_db: f64) -> f32 {
    const MAX_BOOST: f32 = 3.981_072; // +12 dB
    let boost = (1.0 / source_peak.max(1e-9)).min(MAX_BOOST);
    filmcraft_render::audio::db_to_gain(clip_gain_db) * boost
}

/// A key that changes when the sound of sequence `item` does: its audio segments (clips,
/// transitions, mixer state; see `filmcraft_render::preview::audio_segments`) and its length.
fn sequence_audio_key(project: &filmcraft_project::Project, item: ItemId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for seg in filmcraft_render::preview::audio_segments(project, item) {
        (seg.first_sample, seg.samples, seg.hash).hash(&mut h);
    }
    project.sequence(item).map(|q| (q.duration().0, q.settings.sample_rate)).hash(&mut h);
    h.finish()
}

/// Waveform peaks of a nested sequence's mix, in the same form as a media item's (min, max per
/// 256 samples at 48 kHz). They are mixed in the background, and again whenever the sequence's
/// sound changes; until the new ones are ready the old ones stay on show.
fn request_sequence_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    let revision = app.session.revision;
    let key = match app.tl.seq_keys_seen.get(&item) {
        Some(&(rev, key)) if rev == revision => key,
        _ => {
            let key = sequence_audio_key(&app.session.project, item);
            app.tl.seq_keys_seen.insert(item, (revision, key));
            key
        }
    };
    let have = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&item).cloned();
    let current = app.tl.seq_peak_keys.lock().unwrap_or_else(|e| e.into_inner()).get(&item) == Some(&key);
    if current && have.is_some() {
        return have;
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&item) {
            return have;
        }
        pend.push(item);
    }
    let (peaks, keys, pending) = (app.tl.peaks.clone(), app.tl.seq_peak_keys.clone(), app.tl.peaks_pending.clone());
    let project = app.session.project.clone();
    let provider = app.session.media.provider(project.clone(), app.session.services.clone());
    let run = move || {
        // a panic in the mix must not take the app down with it, nor leave the item pending for good
        let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let seq = project.sequence(item)?;
            let sr = seq.settings.sample_rate.max(1);
            let total = seq.duration().to_units_floor(sr as i64).max(0) as usize;
            // one peak per 256 samples at 48 kHz, whatever the sequence's own rate
            let bucket = ((256u64 * sr as u64) / 48_000).max(1) as usize;
            let chunk = (bucket * 750).max(1);
            let mut out = Vec::with_capacity(total / bucket + 1);
            let mut s = 0usize;
            while s < total {
                let n = chunk.min(total - s);
                let buf = filmcraft_render::audio::mix_sequence(&project, seq, s as i64, n, &provider);
                let ch = buf.channels.first()?;
                for c in ch.chunks(bucket) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
                s += n;
            }
            Some(out)
        }));
        if let Ok(Some(out)) = mixed {
            peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(item, Arc::new(out));
            keys.lock().unwrap_or_else(|e| e.into_inner()).insert(item, key);
        }
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != item);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    have
}

pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    if app.session.project.sequence(item).is_some() {
        return request_sequence_peaks(app, item);
    }
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

/// 45° hatching over `r` (transitions, and the empty end of a nested sequence clip).
fn paint_hatch(p: &egui::Painter, r: Rect) {
    let cp = p.with_clip_rect(r.intersect(p.clip_rect()));
    let step = 4.0;
    let mut x = r.min.x - r.height();
    while x < r.max.x {
        cp.line_segment([pos2(x, r.max.y), pos2(x + r.height(), r.min.y)], Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xd9, 0xd9, 0xd9, 110)));
        x += step;
    }
}

fn draw_transition(p: &egui::Painter, r: Rect, trn: &filmcraft_project::Transition, selected: bool, t: &Tokens) {
    let cp = p.with_clip_rect(r.intersect(p.clip_rect()));
    cp.rect_filled(r, 0.0, Color32::from_black_alpha(90));
    paint_hatch(p, r);
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
    if selected {
        cp.rect_stroke(r, 0.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    }
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
        let Some(tr) = seq.track(r.track) else { continue };
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
            Some(if tr.locked { t.icon_active } else { t.text_dim }),
        );
        app.auto.add(&format!("timeline.track.{label}.locked"), lock_r, "Toggle Track Lock");
        if lresp.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "locked": !tr.locked})));
        }
        if tr.locked {
            // diagonal hatch over locked lanes is drawn by the lane painter; here a subtle tint
            p.rect_filled(Rect::from_min_max(pos2(hrect.max.x - 4.0, hrect.min.y), hrect.max), 0.0, t.pressed);
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
    // current timecode: Premiere's big blue timecode
    let tc = format_time(app.session.playhead(), rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    let tc_rect = Rect::from_min_size(pos2(rect.min.x + 14.0, rect.min.y + 4.0), vec2(hw - 20.0, 20.0));
    let tc_id = egui::Id::new(("timeline-tc", seq_id));
    match crate::widgets::timecode_field(ui, tc_id, tc_rect, &tc, app.session.playhead(), Tick::ZERO, rate, seq.settings.drop_frame, t.timecode) {
        Some(Ok(time)) => {
            if let Err(e) = app.session.execute("playhead.set", json!({"time": time.0})) {
                app.ui.status = e.to_string();
            }
        }
        Some(Err(e)) => app.ui.status = e,
        None => {}
    }
    app.auto.add("timeline.timecode", tc_rect, &tc);
    // toolbar: 30 × 30 buttons, "on" = #4b4b4b fill
    let mut x = rect.min.x + 12.0;
    let y = rect.min.y + 26.0;
    let toggles: [(Icon, &str, bool, bool, &str); 6] = [
        (Icon::Nest, "nest", !app.session.state.sequences_as_clips, true, tl!("Insert and overwrite sequences as nests or individual clips")),
        (Icon::Magnet, "snap", app.session.state.snapping, true, tl!("Snap in Timeline (S)")),
        (Icon::Link, "linked", app.session.state.linked_selection, true, tl!("Linked Selection")),
        (Icon::Captions, "captions", false, false, tl!("Caption track options")),
        (Icon::Marker, "marker", false, false, tl!("Add Marker (M)")),
        (Icon::Wrench, "settings", false, false, tl!("Timeline Display Settings")),
    ];
    for (icon, key, on, toggle, tip) in toggles {
        let r = Rect::from_min_size(pos2(x, y), vec2(28.0, 28.0));
        let resp = ui.interact(r, egui::Id::new(("tl-toggle", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("timeline.toggle.{key}"), r, tip);
        if toggle && on {
            p.rect_filled(r, 4.0, t.pressed);
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, t.hover);
        }
        icons::paint(&p, r.shrink(7.0), icon, if toggle && on { t.text } else { t.text_dim });
        if resp.clicked() {
            let _ = match key {
                "nest" => app.session.execute("sequence.nestSequences", json!({})),
                "snap" => app.session.execute("sequence.snap", json!({})),
                "linked" => app.session.execute("sequence.linkedSelection", json!({})),
                "marker" => app.session.execute("markers.add", json!({})),
                _ => Ok(Value::Null),
            };
        }
        if key == "settings" {
            egui::Popup::menu(&resp).show(|ui| {
                ui.checkbox(&mut app.ui.timeline.show_thumbnails, tl!("Show Video Thumbnails"));
                ui.checkbox(&mut app.ui.timeline.show_waveforms, tl!("Show Audio Waveform"));
                ui.separator();
                let mut te = app.session.state.show_through_edits;
                let c = ui.checkbox(&mut te, tl!("Show Through Edits"));
                app.auto.add("timeline.settings.showThroughEdits", c.rect, "Show Through Edits");
                if c.changed() {
                    let _ = app.session.execute("sequence.showThroughEdits", json!({"on": te}));
                }
                ui.separator();
                if ui.button(tl!("Expand All Tracks")).clicked() {
                    app.ui.timeline.video_track_h = 64.0;
                    app.ui.timeline.audio_track_h = 64.0;
                }
                if ui.button(tl!("Minimize All Tracks")).clicked() {
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
            BarState::Yellow => (t.render_yellow, tl!("Unrendered: should play back in real time")),
            BarState::Red => (t.render_red, tl!("Unrendered: render to play back in real time")),
            BarState::Green => (t.render_green, tl!("Rendered preview")),
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

/// The narrowest the zoom scroll bar's thumb gets: room for its two handles and a part between
/// them to grab.
const MIN_THUMB_W: f32 = 34.0;

/// Where the zoom scroll bar's thumb is: (left edge, width, seconds of scroll per point of thumb
/// travel). The thumb is as wide as the share of the Timeline in view (never narrower than
/// [`MIN_THUMB_W`]) and its travel covers the whole scroll range, so it reaches both ends of the
/// bar and moves with the pointer when dragged.
fn zoom_thumb(bar: Rect, seq_seconds: f64, scroll: f64, pps: f64) -> (f32, f32, f64) {
    let vis = bar.width() as f64 / pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let w = ((vis / total) as f32 * bar.width()).clamp(MIN_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (total - vis).max(0.0);
    let at = if range > 0.0 { (scroll / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    (bar.min.x + at, w, if travel > 0.5 { range / travel as f64 } else { 0.0 })
}

fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, seq_seconds: f64, t: &Tokens) {
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let v = &mut app.ui.timeline;
    let vis = bar.width() as f64 / v.pps;
    let total = timeline_extent(seq_seconds).max(vis);
    let (left, w, per_point) = zoom_thumb(bar, seq_seconds, v.scroll, v.pps);
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
        p.circle_filled(pos2(x, thumb.center().y), 4.5, t.panel_bg);
        p.circle_stroke(pos2(x, thumb.center().y), 4.5, Stroke::new(1.5, t.icon_active));
    }
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    app.auto.add("timeline.zoomBar.track", bar, "zoom scroll bar track");
    let resp = ui.interact(bar, egui::Id::new("tl-zoombar"), Sense::click_and_drag());
    let range = (total - vis).max(0.0);
    // a press beside the thumb brings the thumb's middle under the pointer (and a drag goes on
    // from there)
    let jump = |v: &mut crate::state::TimelineView, x: f32| {
        if per_point > 0.0 {
            v.target_scroll = ((x - bar.min.x - w / 2.0) as f64 * per_point).clamp(0.0, range);
            v.scroll = v.target_scroll;
        }
    };
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos().filter(|pos| !thumb.contains(*pos))
    {
        jump(&mut app.ui.timeline, pos.x);
    }
    // (a drag is known to be one only after the pointer has moved: what was grabbed is what was
    // under the pointer when the button went down)
    if resp.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let mode = if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            jump(&mut app.ui.timeline, pos.x);
            0
        } else if (pos.x - thumb.min.x).abs() < 9.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            2
        } else {
            0
        };
        app.tl.drag = Some(Drag::ZoomBar { grab: pos.x, start: app.ui.timeline.target_scroll, mode });
    }
    if let Some(Drag::ZoomBar { grab, start, mode }) = app.tl.drag.clone()
        && resp.dragged()
    {
        let v = &mut app.ui.timeline;
        match mode {
            // the thumb's middle: scroll, the thumb staying under the pointer
            0 => {
                if let Some(pos) = resp.interact_pointer_pos() {
                    v.target_scroll = (start + (pos.x - grab) as f64 * per_point).clamp(0.0, range);
                    v.scroll = v.target_scroll;
                }
            }
            // a handle: zoom, the other end of the view staying where it is
            _ => {
                let dx = resp.drag_delta().x as f64 / bar.width().max(1.0) as f64 * total;
                if mode == 1 {
                    let end = v.scroll + vis;
                    let ns = (v.scroll + dx).clamp(0.0, (end - 0.05).max(0.0));
                    v.pps = (bar.width() as f64 / (end - ns).max(0.05)).clamp(0.05, 24_000.0);
                    v.scroll = ns;
                    v.target_scroll = ns;
                } else {
                    let ne = (v.scroll + vis + dx).max(v.scroll + 0.05);
                    v.pps = (bar.width() as f64 / (ne - v.scroll)).clamp(0.05, 24_000.0);
                }
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
    p.rect_filled(thumb, 3.0, t.vertical_scroll_thumb);
    let resp = ui.interact(bar, egui::Id::new(("tl-vs", id)), Sense::drag());
    if resp.dragged() {
        let d = resp.drag_delta().y / (bar.height() - h).max(1.0) * max;
        *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
    }
}

/// Snap `t` to nearby candidates (edits, playhead, markers, in/out). Returns snapped tick.
fn snap(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId]) -> Tick {
    snap_to(app, seq, layout, t, exclude, true)
}

/// Where a dragged playhead lands: [`snap`], but not onto itself, or it would stick where it is
/// and only move in jumps of `SNAP_PX` (#164).
fn snap_playhead(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick) -> Tick {
    snap_to(app, seq, layout, t, &[], false)
}

fn snap_to(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId], to_playhead: bool) -> Tick {
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
    if to_playhead {
        cands.push(app.session.playhead());
    }
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

/// What the Timeline shows at `pos` for the control channel's `ui.timeline.hit`, with `kind`: the
/// trim a press of the current tool with `mods` starts there (null when it would not trim).
pub fn hit_json(app: &FilmcraftApp, pos: Pos2, mods: egui::Modifiers) -> Value {
    let (Some(layout), Some(seq)) = (app.tl.layout.as_ref(), app.session.active_sequence()) else { return json!({"hit": "none"}) };
    let t = layout.tick_at(pos.x);
    let grab = grab_at(seq, layout, pos, app.ui.tool, mods, app.session.prefs.trim.selection_tool_roll_ripple);
    let kind = match grab {
        Grab::Edge { kind, .. } => json!(kind.name()),
        Grab::Volume { .. } => json!("volume"),
        Grab::Other(_) => Value::Null,
    };
    match grab.hit() {
        Hit::Ruler => json!({"hit": "ruler", "time": t.0}),
        Hit::Clip { track, clip, edge } => {
            json!({"hit": "clip", "track": track.0, "clip": clip.0, "edge": edge.map(|e| format!("{e:?}")), "kind": kind, "time": t.0})
        }
        Hit::Transition { track, id, edge } => json!({"hit": "transition", "track": track.0, "transition": id.0, "edge": edge.map(|e| format!("{e:?}"))}),
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
        Drag::Remix { clip, delta, .. } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                out.insert(*clip, (it.start, it.duration + *delta, None));
            }
        }
        Drag::Trim { clip, edge, delta, .. } | Drag::Stretch { clip, edge, delta, .. } => {
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
        Drag::Roll { left, right, delta, .. } => {
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

/// The clip context menu: groups (separated by rules) of (label, command id). Entries marked `…`
/// open their dialog through `menus::invoke`, like the same item in the Clip menu.
pub(crate) const CLIP_MENU: &[&[(&str, &str)]] = &[
    &[
        ("Cut", "edit.cut"),
        ("Copy", "edit.copy"),
        ("Paste Attributes…", "edit.pasteAttributes"),
        ("Remove Attributes…", "edit.removeAttributes"),
        ("Clear", "edit.clear"),
        ("Ripple Delete", "edit.rippleDelete"),
    ],
    &[("Edit Original", "edit.editOriginal"), ("Replace With Clip From Source Monitor", "clip.replaceFromSource")],
    &[
        ("Enable", "clip.enable"),
        ("Link", "clip.link"),
        ("Group", "clip.group"),
        ("Ungroup", "clip.ungroup"),
        ("Synchronize…", "clip.synchronize"),
        ("Merge Clips…", "clip.mergeClips"),
        ("Nest…", "clip.nest"),
        ("Make Subsequence", "sequence.makeSubsequence"),
        ("Reveal Nested Sequence", "sequence.revealNested"),
        ("Multi-Camera", "clip.multicam"),
    ],
    &[("Label", "edit.label")],
    &[("Speed/Duration…", "clip.speedDuration")],
    &[
        ("Frame Hold Options…", "clip.frameHoldOptions"),
        ("Add Frame Hold", "clip.frameHold"),
        ("Insert Frame Hold Segment", "clip.insertFrameHoldSegment"),
        ("Field Options…", "clip.fieldOptions"),
        ("Scale to Frame Size", "clip.scaleToFrameSize"),
        ("Fit to frame", "clip.fitToFrame"),
        ("Fill frame", "clip.fillFrame"),
    ],
    &[("Reveal in Project", "clip.revealInProject"), ("Join Through Edits", "sequence.joinThroughEdits")],
];

/// The cut right-clicked on a track (#219): the clip ending there and the clip starting there,
/// either missing at the ends of a gap. Its menu offers the edit point's trim types, Apply Default
/// Transitions and Join Through Edits, like Premiere's edit point menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditPointTarget {
    left: Option<ClipId>,
    right: Option<ClipId>,
}

impl EditPointTarget {
    fn at(seq: &Sequence, track: TrackId, clip: ClipId, edge: filmcraft_edit::Edge) -> Option<Self> {
        let tr = seq.track(track)?;
        let it = tr.item(clip)?;
        Some(match edge {
            filmcraft_edit::Edge::Out => Self { left: Some(clip), right: tr.items.iter().find(|x| x.start == it.end()).map(|x| x.id) },
            filmcraft_edit::Edge::In => Self { left: tr.items.iter().find(|x| x.end() == it.start).map(|x| x.id), right: Some(clip) },
        })
    }
}

/// The edit point menu's trim types: label, id (`timeline.editPointMenu.<id>`), trim kind, and
/// which side of the cut it selects (the incoming clip's In or the outgoing clip's Out).
pub(crate) const EDIT_POINT_TYPES: &[(&str, &str, &str, filmcraft_edit::Edge)] = &[
    ("Ripple Trim In", "rippleIn", "ripple", filmcraft_edit::Edge::In),
    ("Ripple Trim Out", "rippleOut", "ripple", filmcraft_edit::Edge::Out),
    ("Roll Edit", "roll", "roll", filmcraft_edit::Edge::Out),
    ("Trim In", "trimIn", "trim", filmcraft_edit::Edge::In),
    ("Trim Out", "trimOut", "trim", filmcraft_edit::Edge::Out),
];

/// The edit point context menu (#219).
fn edit_point_menu(app: &mut FilmcraftApp, ctx: &egui::Context, ui: &mut egui::Ui, seq: &Sequence, target: EditPointTarget) {
    let selected = app.session.state.edit_points.first().copied();
    for &(label, id, kind, edge) in EDIT_POINT_TYPES {
        let clip = match edge {
            filmcraft_edit::Edge::In => target.right,
            filmcraft_edit::Edge::Out => target.left,
        };
        // a roll needs a clip on both sides of the cut
        let clip = clip.filter(|_| kind != "roll" || (target.left.is_some() && target.right.is_some()));
        let out = edge == filmcraft_edit::Edge::Out;
        let current = selected.is_some_and(|ep| Some(ep.clip) == clip && ep.out == out && serde_json::to_value(ep.kind).ok() == Some(json!(kind)));
        let label = crate::i18n::t(label);
        let text = if current { format!("✓ {label}") } else { label.to_string() };
        let r = ui.add_enabled(clip.is_some(), egui::Button::new(text));
        app.auto.add(&format!("timeline.editPointMenu.{id}"), r.rect, label);
        if r.clicked()
            && let Some(c) = clip
        {
            let e = if out { "out" } else { "in" };
            if let Err(e) = app.session.execute("trim.selectEditPoint", json!({"clip": c.0, "edge": e, "kind": kind})) {
                app.ui.status = e.to_string();
            }
            ui.close();
        }
    }
    ui.separator();
    let label = tl!("Apply Default Transitions");
    let r = ui.add_enabled(app.session.is_enabled("trim.applyDefaultTransition"), egui::Button::new(label));
    app.auto.add("timeline.editPointMenu.applyDefaultTransitions", r.rect, label);
    if r.clicked() {
        if let Err(e) = crate::menus::invoke(app, ctx, "trim.applyDefaultTransition", json!({})) {
            app.ui.status = e;
        }
        ui.close();
    }
    // only this cut (and its linked partners), and only when it is a through edit
    let through = filmcraft_edit::through::through_edits(seq).iter().any(|e| Some(e.left) == target.left && Some(e.right) == target.right);
    let label = tl!("Join Through Edits");
    let r = ui.add_enabled(through, egui::Button::new(label));
    app.auto.add("timeline.editPointMenu.joinThroughEdits", r.rect, label);
    if r.clicked()
        && let (Some(left), Some(right)) = (target.left, target.right)
    {
        if let Err(e) = app.session.execute("sequence.joinThroughEdits", json!({"cut": [left.0, right.0]})) {
            app.ui.status = e.to_string();
        }
        ui.close();
    }
}

/// The clip menu's Multi-Camera submenu: Enable, Flatten, and the cameras of the selected nested
/// sequence clips (greyed out when none of the selected clips is a nested sequence).
fn multicam_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, picked: &[&TrackItem]) {
    let nests: Vec<&TrackItem> = picked.iter().copied().filter(|it| app.session.project.sequence(it.item).is_some()).collect();
    let enabled = !nests.is_empty() && nests.iter().all(|it| it.multicam.is_some_and(|m| m.enabled));
    // camera names of the first selected nest, and the angle it shows
    let cameras: Vec<(usize, String)> = nests
        .first()
        .and_then(|it| app.session.project.sequence(it.item))
        .map(|q| q.cameras().video_angles().map(|(i, c)| (i, c.name.clone())).collect())
        .unwrap_or_default();
    let shown = nests.first().and_then(|it| it.multicam).filter(|m| m.enabled).map(|m| m.angle as usize);
    let mut run: Option<(&str, Value)> = None;
    ui.add_enabled_ui(!nests.is_empty(), |ui| {
        let r = ui.menu_button(tl!("Multi-Camera"), |ui| {
            for (label, cmd) in [(if enabled { tl!("✓ Enable") } else { tl!("Enable") }, "clip.multicamEnable"), (tl!("Flatten"), "clip.multicamFlatten")] {
                let r = ui.add_enabled(app.session.is_enabled(cmd), egui::Button::new(label));
                app.auto.add(&format!("timeline.clipMenu.{cmd}"), r.rect, label);
                if r.clicked() {
                    run = Some((cmd, json!({})));
                }
            }
            if enabled && !cameras.is_empty() {
                ui.separator();
                for (angle, name) in &cameras {
                    let label = if shown == Some(*angle) { format!("✓ {name}") } else { name.clone() };
                    let r = ui.button(&label);
                    app.auto.add(&format!("timeline.clipMenu.multicam.camera.{angle}"), r.rect, &label);
                    if r.clicked() {
                        run = Some(("multicam.switchAngle", json!({"angle": angle, "clips": nests.iter().map(|it| it.id.0).collect::<Vec<_>>()})));
                    }
                }
            }
        });
        app.auto.add("timeline.clipMenu.clip.multicam", r.response.rect, "Multi-Camera");
    });
    if let Some((cmd, params)) = run {
        if let Err(e) = app.session.execute(cmd, params) {
            app.ui.status = e.to_string();
        }
        ui.close();
    }
}

/// What the wheel and the trackpad did this frame.
struct WheelInput {
    /// Scroll in points (wheel lines and pages converted), positive = content moves right / down.
    delta: egui::Vec2,
    /// Pinch factor (1 = none).
    pinch: f32,
    command: bool,
    alt: bool,
    shift: bool,
}

/// The wheel events of this frame, read from the events themselves: egui turns Cmd + wheel into a
/// zoom and Shift + wheel into a sideways scroll before anyone asks, and the Timeline has its own
/// meaning for both. The modifiers are those of the events (a script's wheel carries its own).
fn wheel_input(ctx: &egui::Context) -> WheelInput {
    ctx.input(|i| {
        let mut w = WheelInput { delta: egui::Vec2::ZERO, pinch: 1.0, command: false, alt: false, shift: false };
        for e in &i.events {
            match e {
                egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let points = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 400.0,
                    };
                    if delta.x.is_finite() && delta.y.is_finite() {
                        w.delta += *delta * points;
                    }
                    w.command |= modifiers.command || modifiers.mac_cmd || modifiers.ctrl;
                    w.alt |= modifiers.alt;
                    w.shift |= modifiers.shift;
                }
                egui::Event::Zoom(z) if z.is_finite() && *z > 0.0 => w.pinch *= *z,
                _ => {}
            }
        }
        w
    })
}

fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, rect: Rect) {
    let ctx = ui.ctx().clone();
    let area = Rect::from_min_max(pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
    let resp = ui.interact(area, egui::Id::new("timeline-area"), Sense::click_and_drag());
    let pos = resp.hover_pos().or(resp.interact_pointer_pos());
    let mods = ctx.input(|i| i.modifiers);
    let tool = app.ui.tool;
    let rate = seq.settings.frame_rate;
    let roll_ripple = app.session.prefs.trim.selection_tool_roll_ripple;

    // ---- what the button went down on (#259): kept from the press frame until it comes up
    if ctx.input(|i| !i.pointer.any_down() && !i.pointer.any_released()) {
        app.tl.pressed = None;
    }
    if resp.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.primary_pressed())
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        app.tl.pressed = Some(Pressed { pos: p, tick: layout.tick_at(p.x), grab: grab_at(seq, layout, p, tool, mods, roll_ripple) });
    }

    // ---- wheel, as in Premiere Pro on macOS (checked in 26.5.2):
    //   wheel            the tracks under the pointer, up and down
    //   Cmd + wheel      the Timeline, sideways
    //   Option + wheel   zoom about the pointer
    // Settings ▸ Timeline ▸ Timeline Mouse Scrolling "Horizontal" swaps the first two. A sideways
    // gesture (trackpad swipe, tilt wheel, Shift + wheel) always scrolls sideways; a pinch zooms.
    if ui.rect_contains_pointer(rect)
        && let Some(p) = ctx.pointer_hover_pos()
    {
        let w = wheel_input(&ctx);
        if (w.pinch - 1.0).abs() > 1e-4 || (w.alt && w.delta.y != 0.0) {
            let f = if (w.pinch - 1.0).abs() > 1e-4 { w.pinch as f64 } else { (1.0_f64 + w.delta.y as f64 * 0.01).clamp(0.5, 2.0) };
            let anchor_t = layout.tick_at(p.x).seconds();
            let v = &mut app.ui.timeline;
            v.target_pps = (v.target_pps * f).clamp(0.05, 24_000.0);
            app.tl.zoom_anchor = Some((anchor_t, p.x));
        } else if w.delta != egui::Vec2::ZERO {
            let wheel_sideways = (app.session.prefs.timeline.mouse_scrolling == "horizontal") != w.command;
            let sideways = if w.delta.x.abs() > w.delta.y.abs() {
                Some(w.delta.x)
            } else if wheel_sideways || w.shift {
                Some(w.delta.y)
            } else {
                None
            };
            match sideways {
                Some(d) => {
                    let v = &mut app.ui.timeline;
                    let limit = max_scroll(seq.duration().seconds(), layout.content.width(), v.pps);
                    v.target_scroll = (v.target_scroll - d as f64 / v.pps).clamp(0.0, limit);
                    v.scroll = v.target_scroll;
                }
                // up and down: the video tracks above the divider, the audio tracks below it (the
                // lowest video track is at the bottom, so wheeling up brings higher ones in)
                None if p.y < layout.split_y => app.ui.timeline.v_scroll += w.delta.y,
                None => app.ui.timeline.a_scroll -= w.delta.y,
            }
        }
    }

    // ---- cursor feedback: what a press here would grab (#259)
    if app.tl.drag.is_none()
        && let Some(p) = pos
        && area.contains(p)
    {
        let g = grab_at(seq, layout, p, tool, mods, roll_ripple);
        let cur = match (tool, g) {
            (_, Grab::Edge { .. }) => CursorIcon::ResizeColumn,
            (Tool::Selection, Grab::Other(Hit::Transition { edge: Some(_), .. })) => CursorIcon::ResizeColumn,
            (Tool::Selection, Grab::Other(Hit::Transition { edge: None, .. })) => CursorIcon::Grab,
            (_, Grab::Volume { .. }) if tool == Tool::Pen || mods.command => CursorIcon::Crosshair,
            (_, Grab::Volume { .. }) => CursorIcon::ResizeVertical,
            (Tool::Razor, Grab::Other(Hit::Clip { .. })) => CursorIcon::Crosshair,
            (Tool::Slip | Tool::Slide, Grab::Other(Hit::Clip { .. })) => CursorIcon::ResizeHorizontal,
            (Tool::Hand, _) => CursorIcon::Grab,
            (Tool::Zoom, _) => CursorIcon::ZoomIn,
            _ => CursorIcon::Default,
        };
        ctx.set_cursor_icon(cur);
        // the edge a press here would grab, and the trim it starts (#259)
        if let Grab::Edge { track, clip, edge, kind } = g {
            super::trim_monitor::paint_hover_bracket(ui.painter(), seq, layout, track, clip, edge, kind);
        }
        // razor preview line
        if tool == Tool::Razor
            && let Grab::Other(Hit::Clip { track, .. }) = g
            && let Some(row) = layout.rows.iter().find(|r| r.track == track)
        {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x)), &[]);
            let x = layout.x_of(t);
            ui.painter().line_segment([pos2(x, row.rect.min.y), pos2(x, row.rect.max.y)], Stroke::new(1.0, Color32::WHITE));
        }
    }

    // ---- press: act on what was under the pointer when the button went down (#259)
    if resp.drag_started() || (resp.clicked() && app.tl.drag.is_none()) {
        let pressed = match app.tl.pressed.take() {
            Some(pr) => pr,
            // a press and release in the same frame is never stored: it happened where egui says
            None => {
                let Some(p) = resp.interact_pointer_pos() else { return };
                Pressed { pos: p, tick: layout.tick_at(p.x), grab: grab_at(seq, layout, p, tool, mods, roll_ripple) }
            }
        };
        let (p, t) = (pressed.pos, pressed.tick);
        let adding = tool == Tool::Pen || mods.command;
        // a plain click on a Volume line selects the clip, as anywhere else on it
        let grab = match pressed.grab {
            Grab::Volume { .. } if resp.clicked() && !adding => Grab::Other(pressed.grab.hit()),
            g => g,
        };
        let started = match (tool, grab) {
            (_, Grab::Other(Hit::Ruler)) => Some(Drag::Scrub),
            (Tool::Hand, _) => Some(Drag::Pan { last: p }),
            (Tool::Zoom, _) => {
                if resp.clicked() {
                    let f = if mods.alt { 1.0 / 2.0 } else { 2.0 };
                    app.ui.timeline.target_pps = (app.ui.timeline.target_pps * f).clamp(0.05, 24_000.0);
                    app.tl.zoom_anchor = Some((t.seconds(), p.x));
                }
                None
            }
            (Tool::Razor, Grab::Other(Hit::Clip { clip, .. })) => {
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
            (Tool::TrackSelectForward | Tool::TrackSelectBackward, Grab::Other(Hit::Clip { track, .. } | Hit::Empty { track })) => {
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
            (Tool::Selection | Tool::Ripple | Tool::Rolling, Grab::Edge { clip, edge, track, .. }) if resp.clicked() => {
                // click an edge = select it as an edit point (trim mode); Shift adds
                let kind = match tool {
                    Tool::Rolling => "roll",
                    Tool::Ripple => "ripple",
                    _ => {
                        let (dist, neighbour) = edge_geometry(seq, layout, track, clip, edge, p.x);
                        selection_trim_kind(roll_ripple, mods.command, mods.shift, dist, neighbour)
                    }
                };
                let e = if edge == filmcraft_edit::Edge::In { "in" } else { "out" };
                let r = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": e, "kind": kind, "add": mods.shift && !mods.command}));
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                }
                None
            }
            (_, Grab::Edge { clip, edge, kind, .. }) => Some(match kind {
                EdgeKind::Trim => Drag::Trim { clip, edge, mode: filmcraft_edit::TrimMode::Regular, delta: Tick::ZERO, from: t },
                EdgeKind::Ripple => Drag::Trim { clip, edge, mode: filmcraft_edit::TrimMode::Ripple, delta: Tick::ZERO, from: t },
                EdgeKind::Roll { left, right } => Drag::Roll { left, right, delta: Tick::ZERO, from: t },
                EdgeKind::Stretch => Drag::Stretch { clip, edge, delta: Tick::ZERO, from: t },
                EdgeKind::Remix => Drag::Remix { clip, delta: Tick::ZERO, from: t },
            }),
            (_, Grab::Volume { clip, .. }) if resp.drag_started() => super::timeline_volume::start(seq, layout, clip, p).map(Drag::Volume),
            (_, Grab::Volume { clip, .. }) => {
                super::timeline_volume::add_keyframe(app, seq, layout, clip, p.x);
                None
            }
            (Tool::Slip, Grab::Other(Hit::Clip { clip, .. })) => Some(Drag::Slip { clip, delta: Tick::ZERO }),
            (Tool::Slide, Grab::Other(Hit::Clip { clip, .. })) => Some(Drag::Slide { clip, delta: Tick::ZERO }),
            (_, Grab::Other(Hit::Clip { clip, track, .. })) => {
                // select (shift toggles; alt selects one side of a link)
                let sel = &app.session.state.selection;
                if mods.shift {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0], "toggle": true}));
                } else if !sel.contains(&clip) {
                    if mods.alt {
                        app.session.state.selection = vec![clip];
                        app.session.state.transition_selection.clear();
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
            // click a transition to select it (Shift adds / removes); drag an end to change its
            // duration or its middle to slide it over the cut (#430, #224)
            (Tool::Selection, Grab::Other(Hit::Transition { id, edge, .. })) => {
                // a drag of a selected transition (Shift too) moves it rather than toggling it out first
                if !(app.session.state.transition_selection.contains(&id) && resp.drag_started()) {
                    let r = app.session.execute("timeline.select", json!({"transitions": [id.0], "toggle": mods.shift}));
                    if let Err(e) = r {
                        app.ui.status = e.to_string();
                    }
                }
                let span = seq.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == id)).map(|x| (x.start, x.duration));
                match span {
                    Some((start, duration)) if resp.drag_started() => Some(Drag::Transition { id, edge, from: t, start, duration }),
                    _ => None,
                }
            }
            (_, Grab::Other(Hit::Transition { .. })) => None,
            (_, Grab::Other(Hit::Empty { .. })) => {
                if resp.drag_started() {
                    Some(Drag::Marquee { start: p })
                } else {
                    app.session.state.selection.clear();
                    app.session.state.edit_points.clear();
                    app.session.state.transition_selection.clear();
                    None
                }
            }
            (_, Grab::Other(Hit::None)) => None,
        };
        if let Some(d) = started {
            if matches!(d, Drag::Scrub) {
                app.stop();
                let mut tt = rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO));
                if mods.shift || app.session.prefs.timeline.snap_playhead {
                    tt = snap_playhead(app, seq, layout, tt);
                }
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
                    tt = snap_playhead(app, seq, layout, tt);
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
            Drag::Trim { clip, edge, mode, from, .. } => seq.find_item(clip).map(|(_, it)| {
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(base + (t_here - from)), &[clip]);
                Drag::Trim { clip, edge, mode, delta: target - base, from }
            }),
            Drag::Remix { clip, from, .. } => seq.find_item(clip).map(|(_, it)| {
                let target = rate.snap_nearest(it.end() + (t_here - from)).max(it.start + rate.frame_duration());
                Drag::Remix { clip, delta: target - it.end(), from }
            }),
            Drag::Stretch { clip, edge, from, .. } => seq.find_item(clip).map(|(_, it)| {
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(base + (t_here - from)), &[clip]);
                Drag::Stretch { clip, edge, delta: target - base, from }
            }),
            Drag::Roll { left, right, from, .. } => seq.find_item(left).map(|(_, l)| {
                let target = snap(app, seq, layout, rate.snap_nearest(l.end() + (t_here - from)), &[left, right]);
                Drag::Roll { left, right, delta: target - l.end(), from }
            }),
            Drag::Slip { clip, .. } => {
                let start = resp.interact_pointer_pos().map(|_| ()).and(ctx.input(|i| i.pointer.press_origin()));
                let origin = start.map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slip { clip, delta: -(rate.snap_nearest(t_here - origin)) })
            }
            Drag::Slide { clip, .. } => {
                let origin = ctx.input(|i| i.pointer.press_origin()).map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slide { clip, delta: rate.snap_nearest(t_here - origin) })
            }
            Drag::Transition { id, edge, from, start, duration } => {
                let delta = rate.snap_nearest(t_here - from);
                let span = seq.all_tracks().find_map(|tr| {
                    tr.transitions.iter().find(|x| x.id == id).and_then(|x| filmcraft_edit::transitions::drag_span(tr, x, edge, delta, rate.frame_duration()))
                });
                let (start, duration) = span.unwrap_or((start, duration));
                Some(Drag::Transition { id, edge, from, start, duration })
            }
            Drag::Volume(v) => Some(Drag::Volume(super::timeline_volume::drag(app, ui, seq, layout, v, p.y))),
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
        if !matches!(app.tl.drag, Some(Drag::Pan { .. }) | Some(Drag::ZoomBar { .. }) | Some(Drag::Volume(_))) {
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
                Some(app.session.execute("timeline.move", json!({"moves": moves, "insert": mods.command, "linked": false})))
            }
            Drag::Trim { clip, edge, mode, delta, .. } if delta != Tick::ZERO => Some(app.session.execute(
                "timeline.trim",
                json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "mode": if mode == filmcraft_edit::TrimMode::Ripple {"ripple"} else {"regular"}, "delta": delta.0}),
            )),
            Drag::Stretch { clip, edge, delta, .. } if delta != Tick::ZERO => Some(app.session.execute("timeline.rateStretch", json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "delta": delta.0}))),
            Drag::Remix { clip, delta, .. } if delta != Tick::ZERO => {
                let d = seq.find_item(clip).map(|(_, it)| it.duration + delta).unwrap_or(Tick::ZERO);
                Some(app.session.execute("clip.remix", json!({"clip": clip.0, "duration": d.0})))
            }
            Drag::Roll { left, right, delta, .. } if delta != Tick::ZERO => Some(app.session.execute("timeline.roll", json!({"left": left.0, "right": right.0, "delta": delta.0}))),
            Drag::Slip { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slip", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Slide { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slide", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Transition { id, start, duration, .. } => {
                let moved = seq.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == id)).is_some_and(|x| (x.start, x.duration) != (start, duration));
                moved.then(|| app.session.execute("sequence.setTransition", json!({"transition": id.0, "start": start.0, "duration": duration.0})))
            }
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

    // ---- double-click a transition: Set Transition Duration (Premiere)
    if resp.double_clicked()
        && tool == Tool::Selection
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Transition { id, .. } = hit(seq, layout, p)
    {
        crate::panels::transition_controls::open_duration(app, id);
    }

    // ---- double-click a nested sequence clip: open it in its own Timeline tab
    if resp.double_clicked()
        && tool == Tool::Selection
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Clip { clip, .. } = hit(seq, layout, p)
        && seq.find_item(clip).is_some_and(|(_, it)| app.session.project.sequence(it.item).is_some())
    {
        let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
        if let Err(e) = app.session.execute("sequence.revealNested", json!({})) {
            app.ui.status = e.to_string();
        }
    }

    // ---- context menu on clips (right-clicking an unselected clip selects it first); on a clip's
    // edge, the edit point menu instead (right-clicking an unselected edit point selects it first)
    let edit_point_menu_id = egui::Id::new("timeline.editPointMenu.target");
    // the clip right-clicked, remembered while its menu is open: Unlink leaves only it selected
    let menu_clip_id = egui::Id::new("timeline.clipMenu.clip");
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Clip { clip, edge, track } = hit(seq, layout, p)
    {
        ctx.data_mut(|d| d.insert_temp(menu_clip_id, clip));
        let target = edge.and_then(|e| EditPointTarget::at(seq, track, clip, e).map(|t| (t, e)));
        ctx.data_mut(|d| d.insert_temp(edit_point_menu_id, target.map(|(t, _)| t)));
        if let Some((t, e)) = target {
            let on_it = app.session.state.edit_points.iter().any(|ep| (ep.out && Some(ep.clip) == t.left) || (!ep.out && Some(ep.clip) == t.right));
            if !on_it {
                let kind = match tool {
                    Tool::Rolling => "roll",
                    Tool::Ripple => "ripple",
                    _ => {
                        let (dist, neighbour) = edge_geometry(seq, layout, track, clip, e, p.x);
                        selection_trim_kind(app.session.prefs.trim.selection_tool_roll_ripple, false, false, dist, neighbour)
                    }
                };
                let edge = if e == filmcraft_edit::Edge::In { "in" } else { "out" };
                let _ = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": edge, "kind": kind}));
            }
        } else if !app.session.state.selection.contains(&clip) {
            let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
        }
    }
    resp.context_menu(|ui| {
        ui.set_min_width(220.0);
        if let Some(target) = ctx.data(|d| d.get_temp::<Option<EditPointTarget>>(edit_point_menu_id)).flatten() {
            edit_point_menu(app, &ctx, ui, seq, target);
            return;
        }
        let sel = app.session.state.selection.clone();
        let picked: Vec<&filmcraft_project::TrackItem> = sel.iter().filter_map(|c| seq.find_item(*c).map(|(_, it)| it)).collect();
        let all_enabled = !picked.is_empty() && picked.iter().all(|it| it.enabled);
        let linked = picked.iter().any(|it| it.link.is_some());
        let mut first = true;
        for group in CLIP_MENU {
            if !std::mem::take(&mut first) {
                ui.separator();
            }
            for &(label, cmd) in *group {
                if cmd == "edit.label" {
                    ui.menu_button(crate::i18n::t(label), |ui| {
                        for l in filmcraft_project::Label::ALL {
                            if ui.button(crate::i18n::t(l.name())).clicked() {
                                let _ = app.session.execute("edit.label", json!({"label": l.name()}));
                                ui.close();
                            }
                        }
                    });
                    continue;
                }
                if cmd == "clip.multicam" {
                    multicam_menu(app, ui, &picked);
                    continue;
                }
                let label = match cmd {
                    "clip.enable" if all_enabled => tl!("✓ Enable"),
                    "clip.link" if linked => tl!("Unlink"),
                    _ => crate::i18n::t(label),
                };
                let r = ui.add_enabled(!sel.is_empty() && app.session.is_enabled(cmd), egui::Button::new(label));
                app.auto.add(&format!("timeline.clipMenu.{cmd}"), r.rect, label);
                if r.clicked() {
                    match crate::menus::invoke(app, &ctx, cmd, json!({})) {
                        // Unlink (#220): the clips were selected together because they were linked;
                        // keep only the one right-clicked, so it can be dragged away from the others
                        Ok(v) if cmd == "clip.link" && v["linked"] == json!(false) => {
                            if let Some(clip) = ctx.data(|d| d.get_temp::<ClipId>(menu_clip_id))
                                && app.session.state.selection.contains(&clip)
                            {
                                let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => app.ui.status = e,
                    }
                    ui.close();
                }
            }
        }
    });

    // ---- drops: project items and effects
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
    {
        let source = crate::panels::dragged_source(ui);
        let row = layout.row_at(p.y).cloned().filter(|r| {
            source.is_none_or(|s| match r.kind {
                TrackKind::Video => s.video,
                TrackKind::Audio => s.audio,
            })
        });
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let still = app.session.prefs.timeline.still_duration(rate);
        let is_still = app.session.project.item(item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == filmcraft_media::MediaKind::Still);
        let dur = source
            .map(|s| s.range.duration)
            .unwrap_or_else(|| app.session.project.item(item).map(|i| i.duration()).filter(|d| d.0 > 0 && !is_still).unwrap_or(still));
        if let Some(row) = &row {
            let r = Rect::from_min_max(pos2(layout.x_of(t), row.rect.min.y + 1.0), pos2(layout.x_of(t + dur), row.rect.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            if mods.command {
                ui.painter().text(r.left_top() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, tl!("Insert"), Tokens::ui(10.0), Color32::WHITE);
            }
        }
        if ctx.input(|i| i.pointer.any_released())
            && let Some(row) = row
        {
            let (vt, at) = match row.kind {
                TrackKind::Video => (Some(row.track.0), seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0)),
                TrackKind::Audio => (seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0), Some(row.track.0)),
            };
            // dragging one of several selected project items places them all, back to back, as one edit
            let items = match source {
                None => crate::panels::dragged_selection(app, item),
                Some(_) => vec![item],
            };
            let mut params = json!({"track": vt, "audioTrack": at, "time": t.0, "insert": mods.command});
            match items.as_slice() {
                [one] => params["item"] = json!(one.0),
                many => params["items"] = json!(many.iter().map(|i| i.0).collect::<Vec<_>>()),
            }
            if let Some(source) = source {
                params["sourceIn"] = json!(source.range.start.0);
                params["duration"] = json!(source.range.duration.0);
                params["video"] = json!(source.video);
                params["audio"] = json!(source.audio);
            }
            if let Err(e) = app.session.execute("timeline.place", params) {
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
            let transition_kind = filmcraft_project::find_effect(&effect).and_then(|d| match d.kind {
                filmcraft_project::EffectKind::VideoTransition => Some(TrackKind::Video),
                filmcraft_project::EffectKind::AudioTransition => Some(TrackKind::Audio),
                _ => None,
            });
            let edge = if p.x - layout.x_of(it.start) < layout.x_of(it.end()) - p.x { "in" } else { "out" };
            let params = json!({"effect": effect, "clip": clip.0, "edge": edge});
            if let Some(kind) = transition_kind {
                if let Ok((_, tr)) = filmcraft_engine::commands::preview_transition(&app.session, &params, kind) {
                    let r = Rect::from_min_max(pos2(layout.x_of(tr.start), row.rect.min.y + 1.0), pos2(layout.x_of(tr.end()), row.rect.max.y - 1.0));
                    let painter = ui.painter().with_clip_rect(layout.content);
                    painter.rect_filled(r, 2.0, app.tokens.accent.gamma_multiply(0.35));
                    paint_hatch(&painter, r.intersect(layout.content));
                    painter.rect_stroke(r, 2.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
                    let cut = if edge == "in" { it.start } else { it.end() };
                    let x = layout.x_of(cut);
                    painter.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(3.0, app.tokens.accent));
                    // Keep the edge label readable even when the clip/transition is only a few pixels wide.
                    let label = if edge == "in" { tl!("In") } else { tl!("Out") };
                    let label_width = painter.layout_no_wrap(label.to_string(), Tokens::ui(11.0), app.tokens.accent).size().x + 8.0;
                    let label_rect = Rect::from_center_size(pos2(p.x, row.rect.top() + 10.0), vec2(label_width, 15.0));
                    painter.rect_filled(label_rect, 2.0, app.tokens.panel_bg);
                    painter.text(label_rect.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.0), app.tokens.accent);
                    app.auto.add("timeline.transitionDropPreview", r.intersect(layout.content), edge);
                }
            } else {
                let r = Rect::from_min_max(pos2(layout.x_of(it.start), row.rect.min.y), pos2(layout.x_of(it.end()), row.rect.max.y));
                ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
            }
            if ctx.input(|i| i.pointer.any_released()) {
                let r = if let Some(name) = effect.strip_prefix("preset:") {
                    app.session.execute("presets.apply", json!({"preset": name, "clips": [clip.0]}))
                } else if transition_kind.is_some() {
                    app.session.execute("effects.apply", params)
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
    // ---- OS file drops imported with "drop imports to the timeline": placed at the drop point, one after another
    if let Some((items, p)) = app.pending_timeline_drop.clone()
        && layout.content.contains(p)
    {
        app.pending_timeline_drop = None;
        let row = layout.row_at(p.y).cloned().or_else(|| layout.rows.iter().find(|r| r.kind == TrackKind::Video).cloned());
        if let Some(row) = row {
            let (vt, at) = match row.kind {
                TrackKind::Video => (Some(row.track.0), seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0)),
                TrackKind::Audio => (seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0), Some(row.track.0)),
            };
            let still = app.session.prefs.timeline.still_duration(rate);
            let durations: Vec<Tick> = items
                .iter()
                .map(|&item| {
                    let is_still = app.session.project.item(item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == filmcraft_media::MediaKind::Still);
                    app.session.project.item(item).map(|i| i.duration()).filter(|d| d.0 > 0 && !is_still).unwrap_or(still)
                })
                .collect();
            let start = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
            for (item, t) in items.iter().zip(chain_starts(start, &durations)) {
                let params = json!({"item": item.0, "track": vt, "audioTrack": at, "time": t.0, "insert": false});
                if let Err(e) = app.session.execute("timeline.place", params) {
                    app.ui.status = e.to_string();
                }
            }
        }
    }
    // preview of OS files being dragged over the Timeline: a placeholder where the drop would start
    if let Some((count, p)) = app.file_drag_hover
        && layout.content.contains(p)
    {
        let row = layout.row_at(p.y).cloned().or_else(|| layout.rows.iter().find(|r| r.kind == TrackKind::Video).cloned());
        if let Some(row) = row {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
            let x = layout.x_of(t);
            let r = Rect::from_min_max(pos2(x, row.rect.min.y + 1.0), pos2((x + 120.0).min(layout.content.max.x), row.rect.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            let label = if count == 1 { tl!("1 file").to_string() } else { tlf!("{n} files", n = count) };
            ui.painter().text(r.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, label, Tokens::ui(10.0), Color32::WHITE);
        }
    }
    app.auto.add("timeline.tracks", layout.content, "tracks");
}

/// Start tick of each clip when the clips are laid end to end from `start`. Saturates rather than overflows.
pub(crate) fn chain_starts(start: Tick, durations: &[Tick]) -> Vec<Tick> {
    let mut t = start;
    durations
        .iter()
        .map(|d| {
            let at = t;
            t = Tick(t.0.saturating_add(d.0.max(0)));
            at
        })
        .collect()
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

#[cfg(test)]
mod chain_starts_tests {
    use super::chain_starts;
    use filmcraft_time::Tick;

    #[test]
    fn empty_list_has_no_starts() {
        assert!(chain_starts(Tick(100), &[]).is_empty());
    }

    #[test]
    fn one_clip_starts_at_the_drop_point() {
        assert_eq!(chain_starts(Tick(7), &[Tick(30)]), vec![Tick(7)]);
    }

    #[test]
    fn several_clips_are_laid_end_to_end() {
        let durations = [Tick(10), Tick(25), Tick(5)];
        assert_eq!(chain_starts(Tick(100), &durations), vec![Tick(100), Tick(110), Tick(135)]);
    }

    #[test]
    fn start_near_i64_max_saturates_without_panicking() {
        let starts = chain_starts(Tick(i64::MAX - 5), &[Tick(10), Tick(10), Tick(10)]);
        assert_eq!(starts, vec![Tick(i64::MAX - 5), Tick(i64::MAX), Tick(i64::MAX)]);
    }

    #[test]
    fn zero_length_clips_share_one_start() {
        assert_eq!(chain_starts(Tick(42), &[Tick(0), Tick(0), Tick(9)]), vec![Tick(42), Tick(42), Tick(42)]);
    }

    #[test]
    fn negative_durations_count_as_zero() {
        assert_eq!(chain_starts(Tick(5), &[Tick(-3), Tick(4)]), vec![Tick(5), Tick(5)]);
    }
}
