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
use filmcraft_engine::sequence_tools::{DuplicateFrames, duplicate_frames};
use filmcraft_project::{ClipId, ItemId, MarkerId, Sequence, TrackId, TrackItem, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeDisplay, TimeRange, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{ThumbnailMode, TimelineView, Tool};
use crate::theme::Tokens;

use super::timeline_hit::{EDGE_PX, EdgeKind, Grab, edge_geometry, grab_at, marker_at, marker_range_rect, marker_rect, marker_under_pointer, work_area_rect};
pub use super::timeline_hit::{Hit, ROLL_PX, hit, selection_trim_kind};

mod clips;
mod headers;
mod interact;
mod layout;
mod marker_dialog;
mod menus;
mod ruler;
mod scrollbars;
mod snap;
mod waveforms;

#[cfg(test)]
use clips::MAX_THUMBNAIL_TILES;
use clips::{DuplicateMarkers, DuplicatesKey, draw_clip, draw_transition, duplicate_markers, paint_duplicate_stripes, paint_hatch, paint_through_edit};
pub use clips::{thumbnail_tiles, zoomed_thumbnail_tiles};
use headers::draw_headers;
pub(crate) use interact::wheel_input;
use interact::{interact, move_transition_preview, preview_drag};
#[cfg(test)]
use layout::new_track_at;
pub(crate) use layout::reveal_playhead;
use layout::video_row_tops;
pub use layout::{Layout, Row, max_scroll, min_pps, scroll_to_reveal, timeline_extent, zoom_about};
#[cfg(test)]
pub(crate) use menus::{CLIP_MENU, EDIT_POINT_TYPES};
use ruler::draw_top;
pub use ruler::ruler_steps;
use scrollbars::{vertical_scrollbar, zoom_scrollbar};
pub use snap::{hit_json, locate};
pub(super) use snap::{snap, snap_caption};
pub(crate) use waveforms::request_peaks;
#[cfg(test)]
use waveforms::{source_peaks, waveform_display_gain, waveform_zone_height};

const TOP_H: f32 = 58.0; // timecode + toolbar (left) / ruler (right)
const RULER_H: f32 = 44.0;
const SCROLLBAR_H: f32 = 17.0;
const DIVIDER_H: f32 = 5.0;
const MASTER_H: f32 = 34.0;
/// The selected gap between two clips (#648, #668).
const GAP_FILL: Color32 = Color32::from_gray(224);
/// Duplicate frame marker colors, one per master clip with duplicate frames in the order they
/// appear in the sequence.
const DUPLICATE_FRAME_COLORS: [Color32; filmcraft_engine::sequence_tools::DUPLICATE_FRAME_COLORS] = [
    Color32::from_rgb(0xe8, 0x4a, 0x4a),
    Color32::from_rgb(0x3d, 0x9b, 0xe9),
    Color32::from_rgb(0x4c, 0xc2, 0x5a),
    Color32::from_rgb(0xf2, 0xc2, 0x2e),
    Color32::from_rgb(0xb0, 0x5c, 0xe0),
    Color32::from_rgb(0xf0, 0x8a, 0x2c),
    Color32::from_rgb(0x2c, 0xd4, 0xc8),
    Color32::from_rgb(0xec, 0x5e, 0xb0),
    Color32::from_rgb(0x9a, 0xcd, 0x32),
    Color32::from_rgb(0xf5, 0xf5, 0xf5),
];
const SNAP_PX: f32 = 9.0;

/// Waveform peaks are kept per audio stream: (item, index into the media's `audio_streams`).
pub type PeakKey = (ItemId, usize);

/// Transient interaction state.
#[derive(Default)]
pub struct TlState {
    pub drag: Option<Drag>,
    /// What the primary button went down on in the Timeline (#259); see [`Pressed`].
    pub pressed: Option<Pressed>,
    /// A drag or press aborted with Escape; ignores release until the button comes up (#580).
    pub cancelled: bool,
    /// Last layout (for hit-testing from the control channel).
    pub layout: Option<Layout>,
    /// Snap indicator x (screen) this frame.
    snap_x: Option<f32>,
    pub peaks: Arc<Mutex<HashMap<PeakKey, Arc<Vec<(f32, f32)>>>>>,
    peaks_pending: Arc<Mutex<Vec<PeakKey>>>,
    /// Nested sequences have waveforms too, of their mix. Unlike media a sequence changes: this is
    /// the content key (see [`sequence_audio_key`]) the cached peaks of each sequence were made
    /// from, and the key worked out for the session revision last seen.
    seq_peak_keys: Arc<Mutex<HashMap<ItemId, u64>>>,
    seq_keys_seen: HashMap<ItemId, (u64, u64)>,
    /// Source peak of each cached peak list (keyed by the list's address), for the waveform
    /// display gain: scanning the whole source per clip per frame cost more than drawing.
    peak_max: HashMap<PeakKey, (usize, f32)>,
    zoom_anchor: Option<(f64, f32)>,
    /// The zoom (points a second) the video thumbnails are laid out at while the view is zooming (#1094).
    thumb_pps: Option<f64>,
    /// Show Duplicate Frame Markers: the markers of a sequence, with the session revision, the
    /// sequence and the project they were worked out for.
    duplicates: Option<(DuplicatesKey, DuplicateMarkers)>,
    /// The markers being worked out on a background thread, and where the thread leaves them.
    duplicates_pending: Option<DuplicatesKey>,
    duplicates_done: Arc<Mutex<Option<(DuplicatesKey, DuplicateMarkers)>>>,
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
        self.duplicates = None;
        self.duplicates_pending = None;
        self.duplicates_done = Default::default();
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
    /// The vertical scrolls when the button went down (a wheel turn before the drag starts must not move a box's corner).
    pub v_scroll: f32,
    pub a_scroll: f32,
}

/// Where a box selection began, in Timeline content coordinates: the time under the pointer, and
/// its y with the vertical scroll it was pressed at. Scrolling (wheel, auto-scroll, the bars)
/// then moves the corner with the clips rather than leaving it at its screen position (#3563).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarqueeAnchor {
    pub t: Tick,
    pub y: f32,
    /// The scroll of the tracks it was pressed over: `v_scroll` for the video tracks, `a_scroll` for the audio tracks.
    pub scroll: f32,
    /// Pressed over the video tracks; the audio tracks scroll on their own.
    pub video: bool,
}

#[derive(Clone, Debug)]
pub enum Drag {
    Scrub {
        initial: Tick,
    },
    Move {
        clips: Vec<ClipId>,
        grab_tick: Tick,
        start_track: TrackId,
        offset: Tick,
        track_delta: i32,
        /// #1104: the pointer is in the empty space past the last track of the dragged clips' kind:
        /// the drop moves them onto a new track there.
        new_track: Option<(TrackKind, Rect)>,
        /// Option is held: the drop copies the clips instead of moving them (as of the last
        /// drag frame, so letting go of both at once still copies).
        copy: bool,
    },
    Trim {
        clip: ClipId,
        edge: filmcraft_edit::Edge,
        mode: filmcraft_edit::TrimMode,
        delta: Tick,
        /// The time under the pointer when the button went down: the edge moves by how far the
        /// pointer has moved since (#259).
        from: Tick,
        /// Linked partners trim along; false when Alt was held as the drag started (#3300).
        linked: bool,
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
        /// As in [`Drag::Trim`].
        linked: bool,
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
    /// The work area bar dragged by an end (`edge`: that end moves) or its middle (the bar moves);
    /// `start` / `end` are where it is shown now, committed on release.
    WorkArea {
        edge: Option<filmcraft_edit::Edge>,
        /// As in [`Drag::Trim`].
        from: Tick,
        start: Tick,
        end: Tick,
    },
    /// The Timeline dragged by the middle button or the Hand tool: sideways in time, and up and down
    /// the half it began in (`audio`: the audio tracks below the divider, else the video tracks).
    Pan {
        last: Pos2,
        start_scroll: f64,
        start_v: f32,
        audio: bool,
        start_a: f32,
    },
    /// Box selection, shown live: `before` is the selection when the drag began, which Shift or
    /// Ctrl (`add`) keep; the release commits one `timeline.select`.
    Marquee {
        start: MarqueeAnchor,
        before: Vec<ClipId>,
        add: bool,
        individual: bool,
    },
    Divider,
    ZoomBar {
        /// Where the pointer was pressed, and the view's left edge (seconds) at that moment.
        grab: f32,
        start: f64,
        start_pps: f64,
        mode: u8,
    },
}

/// How strongly a track's colour label tints its header and its lane.
const TRACK_HEADER_TINT: f32 = 0.18;
const TRACK_LANE_TINT: f32 = 0.10;

fn label_color(app: &FilmcraftApp, l: filmcraft_project::Label) -> Color32 {
    crate::panels::settings::label_color(app, l)
}

/// Draw the Timeline and run its interaction. A missing or damaged active sequence shows the empty state; `stable_dt` is capped, and a project snapshot is kept
/// while commands mutate the session.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    marker_dialog::show(app, &ctx);
    let Some(seq_id) = app.session.state.active_sequence else {
        empty_state(app, ui, rect);
        return;
    };
    // A damaged project can name an active sequence that no longer exists.
    // Keep the immutable project snapshot alive while UI commands mutate the session.
    // Cloning a sequence here copies every clip and effect on every redraw.
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else {
        empty_state(app, ui, rect);
        return;
    };
    let rate = seq.settings.frame_rate;
    let header_w = app.ui.timeline.header_w;
    let content = Rect::from_min_max(pos2(rect.min.x + header_w, rect.min.y + TOP_H), pos2(rect.max.x - SCROLLBAR_H, rect.max.y - SCROLLBAR_H));
    let ruler = Rect::from_min_max(pos2(content.min.x, rect.min.y + TOP_H - RULER_H), pos2(content.max.x, rect.min.y + TOP_H));
    app.last_timeline_width = content.width();
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.panel_bg);

    // ---- sequence tabs strip (top-left header block)
    // ---- animated zoom / scroll
    animate_view(app, &ctx, content, seq_id, seq);
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
    let nv = seq.video_tracks.len();
    let (video_tops, max_vs) = video_row_tops(video_area, nv, vh, &mut app.ui.timeline.v_scroll);
    for ((i, tr), top) in seq.video_tracks.iter().enumerate().zip(video_tops) {
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
    let (v_scroll, a_scroll) = (app.ui.timeline.v_scroll, app.ui.timeline.a_scroll);
    let layout = Layout { content, ruler, rows: rows.clone(), pps, scroll, split_y, v_scroll, a_scroll };
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
        // a track's colour label tints its lane (#1186)
        if let Some(l) = seq.track(r.track).and_then(|tr| tr.label) {
            painter.rect_filled(row, 0.0, label_color(app, l).gamma_multiply(TRACK_LANE_TINT));
        }
        painter.line_segment([pos2(row.min.x, r.rect.max.y - 0.5), pos2(row.max.x, r.rect.max.y - 0.5)], Stroke::new(1.0, t.separator));
        // the top video track also gets a line above it
        if r.kind == TrackKind::Video && r.index + 1 == nv {
            painter.line_segment([pos2(row.min.x, r.rect.min.y + 0.5), pos2(row.max.x, r.rect.min.y + 0.5)], Stroke::new(1.0, t.separator));
        }
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
    draw_clips(app, &ctx, &painter, seq, seq_id, &rows, &layout, content, vclip, aclip, rate, t);

    // An existing clip dragged past the outer row previews its new destination (#1187).
    if let Some(Drag::Move { clips, offset, new_track: Some((kind, band)), .. }) = app.tl.drag.clone() {
        let preview = painter.with_clip_rect(band.intersect(content));
        for clip in clips {
            if let Some((track, it)) = seq.find_item(clip)
                && seq.track(track).is_some_and(|t| t.kind == kind)
            {
                let start = (it.start + offset).max(Tick::ZERO);
                let body = Rect::from_min_max(pos2(layout.x_of(start), band.min.y + 1.0), pos2(layout.x_of(start + it.duration), band.max.y - 1.0));
                preview.rect_filled(body, 3.0, t.accent.gamma_multiply(0.4));
                preview.rect_stroke(body, 3.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
                if body.height() >= 20.0 {
                    preview.text(body.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, &it.name, Tokens::ui(11.0), t.text);
                }
                app.auto.add(&format!("timeline.newTrackPreview.{}", clip.0), body.intersect(band), &it.name);
            }
        }
    }

    // ---- track headers
    draw_headers(app, ui, seq, &rows, rect, vclip, aclip, &t);
    // divider between video and audio
    let div = Rect::from_min_max(pos2(rect.min.x, split_y), pos2(content.max.x, split_y + DIVIDER_H));
    painter.rect_filled(div, 0.0, t.app_bg);
    let div_resp = ui.interact(div, egui::Id::unique("tl-divider"), Sense::drag());
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
        super::timeline_captions::paint(app, ui, seq, caption_area, &layout, &t);
    }

    // ---- top block: timecode + toggles, ruler
    draw_top(app, ui, rect, seq, &layout, &t, seq_id);

    // ---- playhead
    let ph = app.session.playhead();
    let px = layout.x_of(ph);
    if px >= content.min.x - 1.0 && px <= content.max.x + 1.0 {
        // a short head on the tick row, as in Effect Controls, so it leaves the markers and timecodes clear
        let (top, tip) = (ruler.max.y - 13.0, ruler.max.y);
        let head = [pos2(px - 5.0, top), pos2(px + 5.0, top), pos2(px + 5.0, tip - 5.0), pos2(px, tip), pos2(px - 5.0, tip - 5.0)];
        painter.add(egui::Shape::convex_polygon(head.to_vec(), t.playhead, Stroke::NONE));
        painter.line_segment([pos2(px, tip), pos2(px, content.max.y)], Stroke::new(1.0, t.playhead));
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
        Rect::from_min_max(pos2(content.max.x, video_area.min.y), pos2(rect.max.x, video_area.max.y)),
        &mut app.ui.timeline.v_scroll,
        max_vs,
        true,
        &t,
        "vs",
    );
    vertical_scrollbar(
        ui,
        Rect::from_min_max(pos2(content.max.x, audio_area.min.y), pos2(rect.max.x, audio_area.max.y)),
        &mut app.ui.timeline.a_scroll,
        max_as,
        false,
        &t,
        "as",
    );

    // ---- interaction
    interact(app, ui, seq, &layout, rect);
    super::timeline_volume::interact(app, ui, seq, &layout, aclip);
    // track keyframes (drawn and edited on top of the clips)
    super::timeline_automation::show(app, ui, seq, &layout, aclip);
    if cap_n > 0 {
        super::timeline_captions::interact(app, ui, seq, caption_area, &layout);
    }
    let _ = TICKS_PER_SECOND;
}

/// The timeline view's animated state: eases zoom and scroll toward their targets (or jumps
/// with animations off, #2325), follows the playhead while playing, brings it into view after a
/// jump while stopped, and keeps the view inside the sequence's extent.
fn animate_view(app: &mut FilmcraftApp, ctx: &egui::Context, content: Rect, seq_id: ItemId, seq: &Sequence) {
    let dt = ctx.input(|i| i.stable_dt).min(0.05) as f64;
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
            v.target_pps = (content.width() as f64 * 0.94 / dur_s).clamp(min_pps(seq.duration().seconds(), content.width()), 24_000.0);
            v.target_scroll = 0.0;
            v.fit_pending = false;
            v.fit_empty = empty.then_some(v.target_pps);
        }
        // however the zoom was set (wheel, keys, the bar, a script), zooming out stops at the extent
        let floor = min_pps(seq.duration().seconds(), content.width());
        v.target_pps = v.target_pps.max(floor);
        v.pps = v.pps.max(floor);
        // eased, or in one step with Settings ▸ Appearance ▸ animations off (#2325)
        let k = if app.session.prefs.appearance.animations { 1.0 - (-dt * 20.0).exp() } else { 1.0 };
        let zooming = (v.pps - v.target_pps).abs() / v.target_pps > 0.001;
        // video thumbnails keep the layout of the zoom a gesture or animation started at (#1094)
        if zooming || matches!(app.tl.drag, Some(Drag::ZoomBar { mode: 1 | 2, .. })) {
            app.tl.thumb_pps = Some(app.tl.thumb_pps.unwrap_or(v.pps));
        } else {
            app.tl.thumb_pps = None;
        }
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
    // A jump while stopped (Home, End, edit points, markers) brings the playhead into view.
    {
        let v = &mut app.ui.timeline;
        let ph = app.session.playhead().seconds();
        // another sequence's tab has its own playhead and keeps its own view
        let moved = v.seen_playhead.is_some_and(|(id, seen)| id == seq_id && seen != ph);
        v.seen_playhead = Some((seq_id, ph));
        if moved
            && !app.playback.playing
            && !v.fit_pending
            && let Some(s) = scroll_to_reveal(ph, v.target_scroll, v.target_pps, content.width() as f64)
        {
            v.target_scroll = s;
            v.scroll = s;
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
}

/// The clips in the visible rows: each track's items (with their drag previews, copies,
/// waveforms and thumbnails), duplicate-frame stripes, empty nested-sequence overhangs,
/// through edits, transitions and the locked-track hatch.
#[allow(clippy::too_many_arguments)]
fn draw_clips(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    seq: &Sequence,
    seq_id: ItemId,
    rows: &[Row],
    layout: &Layout,
    content: Rect,
    vclip: Rect,
    aclip: Rect,
    rate: filmcraft_time::FrameRate,
    t: Tokens,
) {
    // ---- clips
    let visible = (layout.tick_at(content.min.x - 2.0), layout.tick_at(content.max.x + 2.0));
    let selection: Vec<ClipId> = app.session.state.selection.clone();
    let mut previews: HashMap<ClipId, (Tick, Tick, Option<TrackId>)> = HashMap::new(); // live drag preview: (start, dur, track)
    // clips whose media changes in the preview (a trimmed or rolled edge): drawn from the edited
    // item, so the waveform and thumbnail stay where the media is instead of squeezing (#374)
    let mut edited: HashMap<ClipId, TrackItem> = HashMap::new();
    preview_drag(app, seq, &mut previews, &mut edited);
    // Option-drag copies: the clips stay where they are and their copies follow the pointer
    let copying = matches!(app.tl.drag, Some(Drag::Move { copy: true, .. }));
    let copies: Vec<(ClipId, (Tick, Tick, Option<TrackId>))> = if copying { previews.drain().collect() } else { Vec::new() };
    if copying {
        ctx.set_cursor_icon(CursorIcon::Copy);
    }
    let duplicates = if app.session.state.show_duplicate_frame_markers { duplicate_markers(app, ctx, seq_id, seq) } else { Arc::default() };
    for r in rows {
        let Some(tr) = seq.track(r.track) else { continue };
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip_rect);
        if row.height() <= 0.0 {
            continue;
        }
        let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, row.min.y), pos2(content.max.x, row.max.y)));
        let volume_line = r.kind == TrackKind::Audio && !r.lane;
        // the selected gap (#648, #668): a light block across the empty time, as in Premiere Pro
        if let Some(g) = app.session.state.gap_selection.filter(|g| g.track == r.track) {
            let x0 = layout.x_of(g.range.start).max(content.min.x - 1.0);
            let x1 = layout.x_of(g.range.end()).min(content.max.x + 1.0);
            if x1 > x0 {
                let gap = Rect::from_min_max(pos2(x0, r.rect.min.y + 1.0), pos2(x1, r.rect.max.y - 1.0));
                p.rect_filled(gap, 0.0, GAP_FILL);
                app.auto.add("timeline.gap", gap.intersect(content), "Selected gap");
            }
        }
        for it in &tr.items {
            let it = edited.get(&it.id).unwrap_or(it);
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
            draw_clip(app, ctx, &p, body, it, r.kind, selection.contains(&it.id), &t, rate);
            if volume_line {
                super::timeline_volume::paint(&p, body, it);
            }
            // Show Duplicate Frame Markers: a stripe along the bottom under each repeated frame,
            // in the color of the clip's master clip
            if let Some(d) = duplicates.get(&it.id) {
                let col = DUPLICATE_FRAME_COLORS.get(d.color).or(DUPLICATE_FRAME_COLORS.last()).copied().unwrap_or(Color32::WHITE);
                // kept inside the clip while a trim drag shortens it
                let inside = p.with_clip_rect(body.intersect(p.clip_rect()));
                paint_duplicate_stripes(app, &inside, layout, &d.ranges, start - it.start, (body.max.y - 3.0, body.max.y), visible, col, it.id);
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
                draw_clip(app, ctx, &p, body, it, r.kind, true, &t, rate);
                if volume_line {
                    super::timeline_volume::paint(&p, body, it);
                }
                app.auto.add(&format!("timeline.clip.{}", it.id.0), body.intersect(content), &it.name);
            }
        }
        for (cid, (start, dur, mt)) in &copies {
            if let Some((home, it)) = seq.find_item(*cid)
                && mt.unwrap_or(home) == r.track
            {
                let body = Rect::from_min_max(pos2(layout.x_of(*start), r.rect.min.y + 1.0), pos2(layout.x_of(*start + *dur), r.rect.max.y - 1.0));
                draw_clip(app, ctx, &p, body, it, r.kind, true, &t, rate);
                app.auto.add(&format!("timeline.clip.{}.copy", cid.0), body.intersect(content), &it.name);
            }
        }
        paint_track_edits(app, painter, seq, rows, layout, content, vclip, aclip, t, copying);
    }
}

/// Per-track details over the clips: through-edit bow-ties, the selected edit points, the
/// transitions (including the ones a drag carries) and the locked-track hatch.
#[allow(clippy::too_many_arguments)]
fn paint_track_edits(
    app: &mut FilmcraftApp,
    painter: &egui::Painter,
    seq: &Sequence,
    rows: &[Row],
    layout: &Layout,
    content: Rect,
    vclip: Rect,
    aclip: Rect,
    t: Tokens,
    copying: bool,
) {
    // transitions a clip drag carries, keyed by the row they land on: a carried transition draws
    // where the drop leaves it — also when that is another track, so a fade dragged from V1 to V2
    // previews on V2 instead of vanishing until the release. Each row keeps its own carried ones
    // at home (a copy's originals) and adds the ones landing on it.
    let mut carried_transitions: HashMap<TrackId, Vec<(&filmcraft_project::Transition, Tick, Tick)>> = HashMap::new();
    for t in seq.all_tracks() {
        for trn in &t.transitions {
            if let Some((s, e, dest)) = move_transition_preview(app, seq, trn) {
                carried_transitions.entry(dest).or_default().push((trn, s, e));
            }
        }
    }
    for r in rows {
        let Some(tr) = seq.track(r.track) else { continue };
        let clip = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip);
        if row.height() <= 0.0 {
            continue;
        }
        let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, row.min.y), pos2(content.max.x, row.max.y)));
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
        super::trim_monitor::paint_edit_points(app, &p, seq, tr, r, layout);
        // transitions (one being dragged where the drag puts it; the selected ones outlined). A
        // clip drag also shows where the drop will carry its transitions (those whose clips all
        // move to one track by one offset): a carried one draws where it lands — its own row or
        // another — right away, a copy adds a ghost beside the copies while the originals stay
        // at home.
        let mut draws: Vec<(&filmcraft_project::Transition, Tick, Tick, bool)> = Vec::new();
        for trn in &tr.transitions {
            let carried = move_transition_preview(app, seq, trn);
            // the transition's home span (a transition dragged by an end still takes over)
            let (start, end) = match &app.tl.drag {
                Some(Drag::Transition { id, start, duration, .. }) if *id == trn.id => (*start, *start + *duration),
                _ => (trn.start, trn.end()),
            };
            // (start, end, is the copy's ghost): a plain move's carried transition draws where
            // the drop leaves it (appended below), a copy keeps the original at home, and one
            // that is not carried stays on its cut
            if carried.is_some() && !copying {
                continue;
            }
            draws.push((trn, start, end, false));
        }
        for (trn, s, e) in carried_transitions.get(&r.track).into_iter().flatten() {
            draws.push((trn, *s, *e, copying));
        }
        for (trn, start, end, ghost) in draws {
            let x0 = layout.x_of(start);
            let x1 = layout.x_of(end);
            let tr_rect = Rect::from_min_max(pos2(x0, super::timeline_hit::transition_top(r.rect)), pos2(x1, r.rect.max.y - 1.0));
            let selected = app.session.state.transition_selection.contains(&trn.id);
            draw_transition(&p, tr_rect, trn, ghost || selected, &t);
            let suffix = if ghost { ".copy" } else { "" };
            app.auto.add(&format!("timeline.transition.{}{}", trn.id.0, suffix), tr_rect, &trn.effect.effect);
            // its ends, for dragging the duration (the same zones as the hit test)
            let zone = super::timeline_hit::EDGE_PX.min(tr_rect.width() / 3.0);
            if !ghost {
                app.auto.add(&format!("timeline.transition.{}.in", trn.id.0), Rect::from_min_max(tr_rect.min, pos2(x0 + zone, tr_rect.max.y)), "start");
                app.auto.add(&format!("timeline.transition.{}.out", trn.id.0), Rect::from_min_max(pos2(x1 - zone, tr_rect.min.y), tr_rect.max), "end");
            }
        }
        if tr.locked {
            let color = Color32::from_rgba_unmultiplied(t.text.r(), t.text.g(), t.text.b(), 64);
            let mut x = content.min.x - row.height();
            while x < content.max.x {
                p.line_segment([pos2(x, row.max.y), pos2(x + row.height(), row.min.y)], Stroke::new(1.0, color));
                x += 8.0;
            }
        }
    }
}

/// Shown when no sequence is open: offers the demo project and accepts project or OS-file drops to create a sequence.
fn empty_state(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, tl!("Drop media here to create sequence."), Tokens::ui(13.0), t.text_dim);
    if app.session.project.items.values().any(|i| matches!(i.kind, filmcraft_project::ItemKind::Sequence(_))) {
        let hint = tl!("Double-click a sequence in the Project panel to open it here.");
        ui.painter().text(rect.center() + vec2(0.0, 50.0), Align2::CENTER_CENTER, hint, Tokens::ui(12.0), t.text_dim);
    }
    let b = Rect::from_center_size(rect.center() + vec2(0.0, 20.0), vec2(170.0, 26.0));
    let resp = ui.interact(b, egui::Id::unique("tl-open-demo"), Sense::click());
    app.auto.add("timeline.openDemo", b, "Open Demo Project");
    ui.painter().rect_filled(b, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(b.center(), Align2::CENTER_CENTER, tl!("Open Demo Project"), Tokens::semibold(12.0), Color32::WHITE);
    if resp.clicked() {
        let _ = app.session.execute("file.openDemoProject", json!({}));
    }
    // accept drops from the project panel
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && ui.rect_contains_pointer(rect)
        && ui.input(|i| i.pointer.hover_pos()).is_some_and(|pos| app.timeline_drop_target(ui.ctx(), pos))
        && ui.input(|i| i.pointer.any_released())
    {
        // a multi-selection makes the sequence from its first item and follows it, back to back, in one undo step
        let items: Vec<u64> = crate::panels::dragged_selection(app, item).iter().map(|i| i.0).collect();
        if let Err(e) = app.session.execute("file.newSequence", json!({"fromItems": items})) {
            app.ui.status = e.to_string();
        }
    }
    // OS files dropped on an empty Timeline: import already happened; create a sequence from the first item
    if let Some((items, p, viewport)) = app.pending_timeline_drop.clone()
        && viewport == ui.ctx().viewport_id()
        && rect.contains(p)
        && let Some((first, rest)) = items.split_first()
    {
        app.pending_timeline_drop = None;
        if let Err(e) = app.session.execute("file.newSequence", json!({"fromItem": first.0})) {
            app.ui.status = e.to_string();
        } else {
            let mut t = app.session.project.item(*first).map(|i| i.duration()).unwrap_or(Tick::ZERO);
            for item in rest {
                let params = json!({"item": item.0, "time": t.0, "insert": false});
                if let Err(e) = app.session.execute("timeline.place", params) {
                    app.ui.status = e.to_string();
                } else {
                    t = Tick(t.0.saturating_add(app.session.project.item(*item).map(|i| i.duration().0.max(0)).unwrap_or(0)));
                }
            }
        }
    }
}

/// #374: trimming an audio clip must cut its waveform off at the moving edge, not squeeze the whole
/// waveform into the shorter clip: the preview draws from the item the trim will leave, which shows
/// the same media at every timeline time it still covers.
#[cfg(test)]
mod trim_preview_tests {
    use super::*;

    fn demo() -> (FilmcraftApp, Sequence) {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let seq = s.active_sequence().unwrap().clone();
        (FilmcraftApp::new(s), seq)
    }

    fn assert_same_media(before: &TrackItem, after: &TrackItem) {
        let mut t = after.start.max(before.start);
        while t < after.end().min(before.end()) {
            assert!((before.source_time_at(t) - after.source_time_at(t)).0.abs() <= 1, "{}: media moved at {t:?}", before.name);
            t += Tick(filmcraft_time::TICKS_PER_SECOND / 10);
        }
    }

    /// A source whose audio streams are different constants (0.1 for stream 0, 0.5 for stream 1).
    struct TwoStreams(filmcraft_media::MediaInfo);

    impl filmcraft_media::MediaSource for TwoStreams {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.0
        }
        fn video_frame(&self, _req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            Err(filmcraft_media::MediaError::NoStream("video"))
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            Ok(filmcraft_frame::AudioBuffer { sample_rate, channels: vec![vec![0.1; frames]] })
        }
        fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            match stream {
                0 => self.audio(start, frames, sample_rate),
                1 => Ok(filmcraft_frame::AudioBuffer { sample_rate, channels: vec![vec![0.5; frames]] }),
                _ => Err(filmcraft_media::MediaError::NoStream("audio")),
            }
        }
    }

    #[test]
    fn each_audio_stream_gets_its_own_waveform_peaks() {
        let info = filmcraft_media::MediaInfo {
            name: "two.mkv".into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: Tick(TICKS_PER_SECOND),
            video: None,
            audio_streams: Vec::new(),
            container: "mkv".into(),
            start_timecode: None,
            file_size: None,
        };
        let src = TwoStreams(info);
        let first = source_peaks(&src, 0, Tick(TICKS_PER_SECOND));
        let second = source_peaks(&src, 1, Tick(TICKS_PER_SECOND));
        assert!(!first.is_empty() && first.len() == second.len());
        assert!(first.iter().all(|&(_, hi)| (hi - 0.1).abs() < 1e-6), "stream 0 is the quiet one");
        assert!(second.iter().all(|&(_, hi)| (hi - 0.5).abs() < 1e-6), "stream 1 is its own audio, not stream 0 again");
        assert!(source_peaks(&src, 2, Tick(TICKS_PER_SECOND)).is_empty(), "a missing stream has no peaks");
    }

    #[test]
    fn trimming_the_music_keeps_its_waveform_in_place() {
        let (mut app, seq) = demo();
        let music = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.name.contains("Ambient")).unwrap().clone();
        for (edge, delta) in [(filmcraft_edit::Edge::In, 5), (filmcraft_edit::Edge::Out, -5), (filmcraft_edit::Edge::In, -1)] {
            let delta = Tick(delta * filmcraft_time::TICKS_PER_SECOND);
            app.tl.drag = Some(Drag::Trim { clip: music.id, edge, mode: filmcraft_edit::TrimMode::Regular, delta, from: Tick::ZERO, linked: true });
            let (mut out, mut edited) = (HashMap::new(), HashMap::new());
            preview_drag(&app, &seq, &mut out, &mut edited);
            let shown = edited.get(&music.id).expect("the trimmed clip is drawn from its edited item");
            // where the preview places it is unchanged
            let want = match edge {
                filmcraft_edit::Edge::In => (music.start + delta, music.duration - delta, None),
                filmcraft_edit::Edge::Out => (music.start, music.duration + delta, None),
            };
            assert_eq!(out.get(&music.id).copied(), Some(want));
            assert_eq!((shown.start, shown.duration), (want.0, want.1));
            // the music under every remaining timeline time is the same: the waveform stays put
            assert_same_media(&music, shown);
            if edge == filmcraft_edit::Edge::In {
                assert_eq!(shown.source_in, music.source_in + delta, "5 s into the music, not the music squeezed");
            }
        }
    }

    #[test]
    fn rolling_an_edit_keeps_both_sides_in_place_but_a_stretch_still_squeezes() {
        let (mut app, seq) = demo();
        let (left, right) = seq.audio_tracks[0].items.windows(2).find(|w| w[0].end() == w[1].start).map(|w| (w[0].clone(), w[1].clone())).unwrap();
        let delta = Tick(filmcraft_time::TICKS_PER_SECOND / 2);
        app.tl.drag = Some(Drag::Roll { left: left.id, right: right.id, delta, from: Tick::ZERO });
        let (mut out, mut edited) = (HashMap::new(), HashMap::new());
        preview_drag(&app, &seq, &mut out, &mut edited);
        assert_same_media(&left, &edited[&left.id]);
        assert_same_media(&right, &edited[&right.id]);
        assert_eq!(edited[&right.id].start, right.start + delta);
        // Rate Stretch keeps the same media in a new length: drawn from the original item
        app.tl.drag = Some(Drag::Stretch { clip: left.id, edge: filmcraft_edit::Edge::Out, delta, from: Tick::ZERO, linked: true });
        let (mut out, mut edited) = (HashMap::new(), HashMap::new());
        preview_drag(&app, &seq, &mut out, &mut edited);
        assert!(edited.is_empty() && out.contains_key(&left.id));
    }
}

/// #1424: dragging a clip edge must not squeeze the waveform into the new clip width. The render
/// loop builds each clip's `body` rect from the drag preview and maps waveform pixels to media
/// time through the item the preview chose (the edited item when there is one, the original
/// otherwise). A drag whose preview changes only the geometry while the waveform still maps
/// through the original item squeezes the whole waveform into the shorter body during the drag;
/// it only corrects itself when the drag commits.
#[cfg(test)]
mod waveform_drag_anchor_tests {
    use super::*;

    /// The waveform's pixel-to-media scale (see `draw_waveform`): pixel `x` of `body` maps to
    /// `it.start + it.duration * f`, so the waveform shows `it.duration / body.width()` media
    /// ticks per pixel. Compression during a drag is exactly this scale changing: the same media
    /// gets squeezed into (or stretched across) the previewed width.
    fn media_ticks_per_pixel(body: &Rect, it: &TrackItem) -> f64 {
        it.duration.0 as f64 / body.width().max(1.0) as f64
    }

    fn layout() -> Layout {
        Layout {
            content: Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 400.0)),
            ruler: Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 20.0)),
            rows: Vec::new(),
            pps: 100.0,
            scroll: 0.0,
            split_y: 0.0,
            v_scroll: 0.0,
            a_scroll: 0.0,
        }
    }

    /// The (body, item) pair the render loop would use for `id` with the app's current drag.
    fn shown(app: &FilmcraftApp, seq: &Sequence, id: ClipId) -> (Rect, TrackItem) {
        let (mut out, mut edited) = (HashMap::new(), HashMap::new());
        preview_drag(app, seq, &mut out, &mut edited);
        let original = seq.find_item(id).unwrap().1.clone();
        let (start, dur, _) = out.get(&id).copied().unwrap_or((original.start, original.duration, None));
        let it = edited.get(&id).cloned().unwrap_or(original);
        let l = layout();
        let body = Rect::from_min_max(pos2(l.x_of(start), 0.0), pos2(l.x_of(start + dur), 40.0));
        (body, it)
    }

    /// The waveform's media-per-pixel scale must not change while a drag is previewed: the clip
    /// body may move or resize, but the waveform keeps its pixels-per-second anchoring.
    fn assert_waveform_anchored(what: &str, before: &(Rect, TrackItem), after: &(Rect, TrackItem)) {
        let (a, b) = (media_ticks_per_pixel(&before.0, &before.1), media_ticks_per_pixel(&after.0, &after.1));
        let drift = ((b / a) - 1.0).abs();
        assert!(drift < 0.01, "{what}: waveform compressed during drag: {a} -> {b} media ticks per pixel");
    }

    fn demo_music() -> (FilmcraftApp, Sequence, TrackItem) {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let seq = s.active_sequence().unwrap().clone();
        let music = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.name.contains("Ambient")).unwrap().clone();
        (FilmcraftApp::new(s), seq, music)
    }

    #[test]
    fn remix_drag_keeps_the_waveform_anchored() {
        // #1424: dragging a music clip's Out edge with the Remix tool must show the same media at
        // the same pixels while dragging as after the remix commits, not squeeze the whole
        // waveform into the new, shorter width.
        let (mut app, seq, music) = demo_music();
        let before = shown(&app, &seq, music.id);
        app.tl.drag = Some(Drag::Remix { clip: music.id, delta: Tick(-8 * filmcraft_time::TICKS_PER_SECOND), from: Tick::ZERO });
        let after = shown(&app, &seq, music.id);
        assert!(after.0.width() < before.0.width(), "the preview shortens the clip");
        assert_waveform_anchored("remix", &before, &after);
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::scroll_to_reveal;

    #[test]
    fn home_scrolls_back_to_the_start() {
        assert_eq!(scroll_to_reveal(0.0, 120.0, 40.0, 800.0), Some(0.0));
    }

    #[test]
    fn end_scrolls_forward_to_the_playhead() {
        let s = scroll_to_reveal(300.0, 0.0, 40.0, 800.0).unwrap();
        assert!((0.0..=300.0).contains(&s) && (300.0 - s) * 40.0 <= 800.0);
    }

    #[test]
    fn a_visible_or_hostile_playhead_leaves_the_view() {
        assert_eq!(scroll_to_reveal(5.0, 0.0, 40.0, 800.0), None);
        assert_eq!(scroll_to_reveal(f64::NAN, 0.0, 40.0, 800.0), None);
        assert_eq!(scroll_to_reveal(5.0, 0.0, 0.0, 800.0), None);
    }
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
mod thumbnail_tiles_tests {
    use super::*;

    const S: Tick = Tick(TICKS_PER_SECOND);
    const FRAME: Tick = Tick(TICKS_PER_SECOND / 25);

    #[test]
    fn zooming_keeps_the_frames_shown_before_the_zoom() {
        // a 10 s clip 1000 pt wide at 20 pt a second, the panel showing x 0..800
        let dur = Tick(10 * S.0);
        let before = thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 1000.0, 80.0, (0.0, 800.0), dur, FRAME);
        // halfway through zooming in to 40 pt a second: the clip is 2000 pt wide
        let zoomed = zoomed_thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 2000.0, 80.0, (0.0, 800.0), dur, FRAME, 40.0, Some(20.0));
        let offsets: Vec<Tick> = zoomed.iter().map(|t| t.2).collect();
        let kept: Vec<Tick> = before.iter().take(5).map(|t| t.1).collect();
        assert_eq!(offsets, kept);
        // each frame stretched over the clip time it showed
        let placed: Vec<(f32, f32)> = zoomed.iter().map(|t| (t.0, t.1)).collect();
        assert_eq!(placed, [(0.0, 160.0), (160.0, 160.0), (320.0, 160.0), (480.0, 160.0), (640.0, 160.0)]);
        // once the zoom settles the tiles are laid out at the new zoom
        let settled = thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 2000.0, 80.0, (0.0, 800.0), dur, FRAME);
        let expect: Vec<(f32, f32, Tick)> = settled.iter().map(|&(x, t)| (x, 80.0, t)).collect();
        assert_eq!(zoomed_thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 2000.0, 80.0, (0.0, 800.0), dur, FRAME, 40.0, None), expect);
        // Head and tail frames do not depend on the zoom
        let head = zoomed_thumbnail_tiles(ThumbnailMode::Head, 0.0, 2000.0, 80.0, (0.0, 800.0), dur, FRAME, 40.0, Some(20.0));
        assert_eq!(head, [(0.0, 80.0, Tick::ZERO)]);
    }

    #[test]
    fn continuous_thumbnails_tile_the_visible_part_of_the_clip() {
        // a 10 s clip 1000 pt wide, 80 pt tiles, the panel showing x 250..650
        let tiles = thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 1000.0, 80.0, (250.0, 650.0), Tick(10 * S.0), FRAME);
        let xs: Vec<f32> = tiles.iter().map(|t| t.0).collect();
        assert_eq!(xs, [240.0, 320.0, 400.0, 480.0, 560.0, 640.0]);
        // each tile shows the clip time under its left edge (100 pt per second)
        assert_eq!(tiles[0].1, Tick(24 * S.0 / 10));
        assert_eq!(tiles[5].1, Tick(64 * S.0 / 10));
        // scrolling 30 pt moves the tiles with the clip: the ones still shown keep their frames
        let scrolled = thumbnail_tiles(ThumbnailMode::Continuous, -30.0, 970.0, 80.0, (250.0, 650.0), Tick(10 * S.0), FRAME);
        assert_eq!(scrolled.first(), Some(&(210.0, tiles[0].1)));
        assert!(scrolled.iter().all(|(x, t)| tiles.iter().any(|(x0, t0)| *x0 - 30.0 == *x && t0 == t)));
        // the last tile never shows a time past the clip's last frame
        let end = thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 1000.0, 80.0, (0.0, 2000.0), Tick(10 * S.0), FRAME);
        assert_eq!(end.len(), 13);
        assert!(end.iter().all(|(_, t)| *t <= Tick(10 * S.0) - FRAME));
        // a clip far zoomed in asks for a bounded number of frames
        let huge = thumbnail_tiles(ThumbnailMode::Continuous, -1.0e9, 1.0e9, 1.0, (-2.0e9, 2.0e9), Tick(10 * S.0), FRAME);
        assert_eq!(huge.len(), MAX_THUMBNAIL_TILES);
    }

    #[test]
    fn head_and_tail_thumbnails_need_room_for_two_frames() {
        let dur = Tick(4 * S.0);
        assert_eq!(thumbnail_tiles(ThumbnailMode::Head, 100.0, 500.0, 80.0, (0.0, 800.0), dur, FRAME), [(100.0, Tick::ZERO)]);
        assert_eq!(thumbnail_tiles(ThumbnailMode::HeadAndTail, 100.0, 500.0, 80.0, (0.0, 800.0), dur, FRAME), [(100.0, Tick::ZERO), (420.0, dur - FRAME)]);
        // narrower than two tiles: the head only, as in Continuous narrower than one tile
        assert_eq!(thumbnail_tiles(ThumbnailMode::HeadAndTail, 100.0, 250.0, 80.0, (0.0, 800.0), dur, FRAME), [(100.0, Tick::ZERO)]);
        assert_eq!(thumbnail_tiles(ThumbnailMode::Continuous, 100.0, 150.0, 80.0, (0.0, 800.0), dur, FRAME), [(100.0, Tick::ZERO)]);
        // off screen or degenerate: nothing
        assert!(thumbnail_tiles(ThumbnailMode::Head, 900.0, 1300.0, 80.0, (0.0, 800.0), dur, FRAME).is_empty());
        assert!(thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 400.0, f32::NAN, (0.0, 800.0), dur, FRAME).is_empty());
        assert!(thumbnail_tiles(ThumbnailMode::Continuous, 0.0, 0.0, 80.0, (0.0, 800.0), dur, FRAME).is_empty());
    }
}

#[cfg(test)]
mod new_track_tests {
    use super::*;
    use filmcraft_project::{Project, SequenceSettings};

    /// #483: a drop above the top video track adds a video track, one below the last audio track an
    /// audio track; over a track, between the areas or on the Mix row it adds none.
    #[test]
    fn empty_space_past_the_tracks_adds_a_track() {
        let mut p = Project::new("t");
        let id = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
        let seq = p.sequence(id).cloned().expect("sequence");
        // video area 0..200 (V1 at 150..200), divider, audio area 205..466 (A1 at 205..255), Mix 466..500
        let row = |kind, track: &filmcraft_project::Track, y0: f32| Row {
            track: track.id,
            kind,
            index: 0,
            rect: Rect::from_min_max(pos2(0.0, y0), pos2(800.0, y0 + 50.0)),
            lane: false,
        };
        let layout = Layout {
            content: Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 500.0)),
            ruler: Rect::from_min_max(pos2(0.0, -20.0), pos2(800.0, 0.0)),
            rows: vec![row(TrackKind::Video, &seq.video_tracks[0], 150.0), row(TrackKind::Audio, &seq.audio_tracks[0], 205.0)],
            pps: 100.0,
            scroll: 0.0,
            split_y: 200.0,
            v_scroll: 0.0,
            a_scroll: 0.0,
        };
        let at = |y| new_track_at(&seq, &layout, y, 50.0, 50.0);
        assert_eq!(at(40.0), Some((TrackKind::Video, Rect::from_min_max(pos2(0.0, 100.0), pos2(800.0, 150.0)))));
        assert_eq!(at(300.0), Some((TrackKind::Audio, Rect::from_min_max(pos2(0.0, 255.0), pos2(800.0, 305.0)))));
        for y in [160.0, 202.0, 230.0, 480.0] {
            assert_eq!(at(y), None, "y = {y}");
        }
    }

    #[test]
    fn rows_only_hit_inside_their_own_pane_when_scrolled() {
        // video pane 100…300, divider 300…305, audio pane 305…466, Mix row 466…500
        let content = Rect::from_min_max(pos2(0.0, 100.0), pos2(1000.0, 500.0));
        let split_y = 300.0;
        let row = |track: u64, kind: TrackKind, y0: f32, y1: f32| Row {
            track: TrackId(track),
            kind,
            index: 0,
            rect: Rect::from_min_max(pos2(0.0, y0), pos2(1000.0, y1)),
            lane: false,
        };
        let layout = Layout {
            content,
            ruler: Rect::from_min_max(pos2(0.0, 60.0), pos2(1000.0, 100.0)),
            rows: vec![
                // V1 scrolled 40 px: it reaches 40 px below the divider
                row(1, TrackKind::Video, 240.0, 340.0),
                // A1 scrolled: its top is hidden above the audio pane, over the divider
                row(2, TrackKind::Audio, 290.0, 380.0),
                // the last audio track runs under the Mix row
                row(3, TrackKind::Audio, 380.0, 480.0),
            ],
            pps: 100.0,
            scroll: 0.0,
            split_y,
            v_scroll: 0.0,
            a_scroll: 0.0,
        };
        let at = |y: f32| layout.row_at(y).map(|r| r.track.0);
        assert_eq!(at(260.0), Some(1), "V1 above the divider");
        assert_eq!(at(320.0), Some(2), "below the divider the audio track wins, not the hidden V1");
        assert_eq!(at(302.0), None, "the divider is no track");
        assert_eq!(at(295.0), Some(1), "A1's hidden top over the video pane");
        assert_eq!(at(400.0), Some(3));
        assert_eq!(at(470.0), None, "the Mix row is no track");
    }

    /// #1522: when the video tracks fill the video area, scrolling all the way up still leaves a
    /// track's height of empty space above the top one, so a drop there can add a fourth track.
    #[test]
    fn a_full_video_area_scrolls_to_space_for_a_new_track() {
        let mut p = Project::new("t");
        let id = p.new_sequence("s", SequenceSettings::default(), 3, 1, None);
        let seq = p.sequence(id).cloned().expect("sequence");
        // video area 0..150 holds V1..V3 exactly; scrolled up as far as it goes
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 150.0));
        let mut v_scroll = f32::MAX;
        let (tops, _) = video_row_tops(area, 3, 50.0, &mut v_scroll);
        let rows = seq
            .video_tracks
            .iter()
            .zip(&tops)
            .enumerate()
            .map(|(index, (t, &y0))| Row {
                track: t.id,
                kind: TrackKind::Video,
                index,
                rect: Rect::from_min_max(pos2(0.0, y0), pos2(800.0, y0 + 50.0)),
                lane: false,
            })
            .collect();
        let layout = Layout {
            content: Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 500.0)),
            ruler: Rect::from_min_max(pos2(0.0, -20.0), pos2(800.0, 0.0)),
            rows,
            pps: 100.0,
            scroll: 0.0,
            split_y: 150.0,
            v_scroll: 0.0,
            a_scroll: 0.0,
        };
        assert_eq!(new_track_at(&seq, &layout, 10.0, 50.0, 50.0), Some((TrackKind::Video, Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 50.0)))));
        // scrolled to the bottom, V1..V3 fill the area as before
        let mut v_scroll = 0.0;
        assert_eq!(video_row_tops(area, 3, 50.0, &mut v_scroll).0, vec![100.0, 50.0, 0.0]);
    }
}

#[cfg(test)]
mod ruler_tests {
    use super::ruler_steps;

    /// Every combination keeps both minimums and has the tick stride divide the label stride.
    #[test]
    fn labels_sit_on_evenly_spaced_ticks() {
        for base in [24, 25, 30, 50, 60, 120] {
            for frame_px in [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 8.0, 20.0, 400.0] {
                let (minor, label) = ruler_steps(frame_px, base);
                let what = format!("{base} fps at {frame_px} pt a frame: tick {minor}, label {label}");
                assert!(minor >= 1, "{what}");
                assert_eq!(label % minor, 0, "every label must fall on a tick: {what}");
                assert!(minor as f64 * frame_px >= 12.0 || minor == label, "ticks at least 12 pt apart: {what}");
                assert!(label as f64 * frame_px >= 110.0, "labels at least 110 pt apart: {what}");
            }
        }
    }

    /// At 24 fps and 100 pt a second the ruler labels every 2 s, with evenly spaced ticks between.
    #[test]
    fn a_24_fps_view_at_100_pt_a_second_labels_every_two_seconds() {
        let (minor, label) = ruler_steps(100.0 / 24.0, 24);
        assert_eq!(label, 48, "2 s at 24 fps");
        assert_eq!(label % minor, 0);
        assert!(minor as f64 * (100.0 / 24.0) >= 12.0);
    }

    /// Degenerate inputs still give non-zero strides that divide.
    #[test]
    fn damaged_rates_give_usable_strides() {
        for frame_px in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let (minor, label) = ruler_steps(frame_px, 24);
            assert!(minor >= 1 && label >= 1 && label % minor == 0, "{frame_px} pt a frame: tick {minor}, label {label}");
        }
        let (minor, label) = ruler_steps(1.0, i64::MAX);
        assert!(minor >= 1 && label >= 1 && label % minor == 0, "an unrepresentable rate: tick {minor}, label {label}");
    }
}

#[cfg(test)]
mod waveform_zone_tests {
    use super::waveform_zone_height;

    #[test]
    fn a_compact_clip_has_its_whole_height_for_the_waveform_and_a_tall_one_the_lower_part() {
        // tall clips are as before
        assert_eq!(waveform_zone_height(100.0), 42.0);
        assert!((waveform_zone_height(60.0) - 25.2).abs() < 1e-4);
        // compact ones fill the clip (a point of margin each side), where 42 % was 12 points of 28
        assert_eq!(waveform_zone_height(28.0), 26.0);
        assert_eq!(waveform_zone_height(49.0), 47.0);
        // never smaller than the minimum
        assert_eq!(waveform_zone_height(4.0), 8.0);
    }
}
