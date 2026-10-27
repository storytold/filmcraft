use super::*;

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

/// #483: where a drop at `y` adds a track: the empty space above the top video track (a video
/// track) or below the last audio track (an audio track), never over a track, the caption tracks
/// or the Mix row. Gives the kind and the band the new track would take (`vh` / `ah` high).
pub(super) fn new_track_at(seq: &Sequence, layout: &Layout, y: f32, vh: f32, ah: f32) -> Option<(TrackKind, Rect)> {
    if layout.row_at(y).is_some() {
        return None;
    }
    let cap_n = seq.caption_tracks.len();
    let tracks_top = layout.content.min.y + if cap_n > 0 { cap_n as f32 * crate::panels::timeline_captions::ROW_H + DIVIDER_H } else { 0.0 };
    let band = |y0: f32, y1: f32| Rect::from_min_max(pos2(layout.content.min.x, y0), pos2(layout.content.max.x, y1));
    if y < layout.split_y {
        let top = layout.rows.iter().filter(|r| r.kind == TrackKind::Video).map(|r| r.rect.min.y).fold(layout.split_y, f32::min);
        (y >= tracks_top && y < top).then(|| (TrackKind::Video, band((top - vh).max(tracks_top), top)))
    } else {
        let bottom = layout.rows.iter().filter(|r| r.kind == TrackKind::Audio).map(|r| r.rect.max.y).fold(layout.split_y + DIVIDER_H, f32::max);
        let mix = layout.content.max.y - MASTER_H;
        (y >= bottom && y < mix).then(|| (TrackKind::Audio, band(bottom, (bottom + ah).min(mix))))
    }
}

/// The top of each of `n` video rows `vh` high, V1 first: V1 sits at the bottom of `area` and the
/// tracks stack upward; `v_scroll` (clamped here) shifts them up. Also gives the largest `v_scroll`.
/// The scroll range includes one empty track's height above the top track: without it, tracks that
/// fill the area leave no space where a drop adds a new one (#1522).
pub(super) fn video_row_tops(area: Rect, n: usize, vh: f32, v_scroll: &mut f32) -> (Vec<f32>, f32) {
    let total = (n + 1) as f32 * vh;
    let v_off = (area.height() - total).max(0.0);
    let max_vs = (total - area.height()).max(0.0);
    *v_scroll = v_scroll.clamp(0.0, max_vs);
    let tops = (0..n).map(|i| area.min.y + v_off + (n - i) as f32 * vh - (max_vs - *v_scroll)).collect();
    (tops, max_vs)
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

/// Where the left edge goes so the playhead at `ph` seconds shows in a view `width` points wide
/// (the page-scroll margin from the left), or `None` when it is in view already.
pub fn scroll_to_reveal(ph: f64, scroll: f64, pps: f64, width: f64) -> Option<f64> {
    if !ph.is_finite() || !scroll.is_finite() || !(pps > 0.0 && pps.is_finite()) {
        return None;
    }
    let px = (ph - scroll) * pps;
    (px < 0.0 || px > width).then(|| (ph - width * 0.05 / pps).max(0.0))
}

/// Explicit navigation reveals the playhead even when its time has not changed (#1188).
pub(crate) fn reveal_playhead(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let ph = app.session.playhead().seconds();
    let v = &mut app.ui.timeline;
    if let Some(scroll) = scroll_to_reveal(ph, v.target_scroll, v.target_pps, f64::from(app.last_timeline_width)) {
        v.scroll = scroll;
        v.target_scroll = scroll;
        app.tl.zoom_anchor = None;
        ctx.request_repaint();
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
