use super::{DIVIDER_H, FilmcraftApp, MASTER_H, Rect, Sequence, Tick, TimelineView, TrackId, TrackKind, pos2};

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
    /// The vertical scrolls the rows were laid out at; a wheel turn changes the view's own values
    /// before the next frame lays the rows out again (and clamps them).
    pub v_scroll: f32,
    pub a_scroll: f32,
}

impl Layout {
    pub fn x_of(&self, t: Tick) -> f32 {
        self.content.min.x + ((t.seconds() - self.scroll) * self.pps) as f32
    }
    pub fn tick_at(&self, x: f32) -> Tick {
        Tick::from_seconds_f64(self.scroll + (x - self.content.min.x) as f64 / self.pps)
    }
    /// The track row under `y`. Rows are not clipped to their pane: a scrolled video track
    /// reaches below the divider and a scrolled audio track above it or over the Mix row, so a
    /// row only counts inside its own pane (video above the divider, audio between the divider
    /// and the Mix row). Without this a press on an audio track hit the hidden V1 row.
    pub fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.iter().find(|r| {
            let s = self.shown(r);
            s.min.y <= y && y < s.max.y
        })
    }

    /// The part of `row` its own pane shows (see `row_at`); empty when it is scrolled out.
    pub fn shown(&self, row: &Row) -> Rect {
        let mut r = row.rect;
        match row.kind {
            TrackKind::Video => r.max.y = r.max.y.min(self.split_y),
            TrackKind::Audio => {
                r.min.y = r.min.y.max(self.split_y + DIVIDER_H);
                r.max.y = r.max.y.min(self.content.max.y - MASTER_H);
            }
        }
        r
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

/// The least `pps` (points a second) the Timeline zooms out to: its whole extent fits a view
/// `width` points wide, and no farther, so the clips of a short sequence stay in sight. A zero or
/// invalid width gives a tiny positive floor, never zero.
pub fn min_pps(sequence_seconds: f64, width: f32) -> f64 {
    (f64::from(width.max(0.0)) / timeline_extent(sequence_seconds)).clamp(1e-6, 24_000.0)
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
    if !(ph.is_finite() && scroll.is_finite() && pps > 0.0 && pps.is_finite()) {
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

/// Zoom about a time, keeping it where it is drawn now (mid-animation included) and returning that
/// x from the view's left edge; a time off screen gets `None` and the view eases to centre it.
pub fn zoom_about(v: &mut TimelineView, factor: f64, anchor_secs: f64, width: f32, sequence_seconds: f64) -> Option<f32> {
    let new = (v.target_pps * factor).clamp(min_pps(sequence_seconds, width), 24_000.0);
    // the drawn view, not the targets: a wheel zoom or scroll still easing has not reached them
    let x = ((anchor_secs - v.scroll) * v.pps) as f32;
    let shown = (0.0..=width).contains(&x);
    let x = if shown { x } else { width / 2.0 };
    v.target_pps = new;
    v.target_scroll = (anchor_secs - x as f64 / new).max(0.0);
    shown.then_some(x)
}

/// View ▸ Zoom In and Zoom Out (`=` and `-`): zoom about the playhead and hold a visible one at
/// its screen x on every frame of the eased zoom, replacing any wheel anchor (#4383).
pub(crate) fn zoom_playhead(app: &mut FilmcraftApp, factor: f64) {
    let ph = app.session.playhead().seconds();
    let seconds = app.session.active_sequence().map_or(0.0, |s| s.duration().seconds());
    let v = &mut app.ui.timeline;
    // the x before the zoom, so a view held at 0 eases the playhead over instead of jumping it
    let x = zoom_about(v, factor, ph, app.last_timeline_width, seconds);
    app.tl.zoom_anchor = x.zip(app.tl.layout.as_ref()).map(|(x, l)| (ph, l.content.min.x + x));
}

#[cfg(test)]
mod zoom_about_tests {
    use super::{TimelineView, min_pps, zoom_about};

    /// A settled view at `pps` points a second with `scroll` seconds at its left edge.
    fn view(pps: f64, scroll: f64) -> TimelineView {
        TimelineView { pps, target_pps: pps, scroll, target_scroll: scroll, ..TimelineView::default() }
    }

    /// A time on screen keeps its x: the zoom returns it, and the target puts the time back there.
    #[test]
    fn a_time_on_screen_keeps_its_x() {
        let mut v = view(50.0, 4.0);
        assert_eq!(zoom_about(&mut v, 1.6, 10.0, 1000.0, 30.0), Some(300.0), "6 s from the left edge at 50 points a second");
        let pps = 50.0 * 1.6;
        assert_eq!((v.target_pps, v.target_scroll), (pps, 10.0 - 300.0 / pps));
        // the right edge itself is still on screen
        let mut v = view(50.0, 4.0);
        assert_eq!(zoom_about(&mut v, 1.6, 24.0, 1000.0, 30.0), Some(1000.0));
        assert_eq!(v.target_scroll, 24.0 - 1000.0 / pps);
    }

    /// A time off screen, either side, gets `None`, and the target centres it (not before 0).
    #[test]
    fn a_time_off_screen_is_centred() {
        let mut v = view(50.0, 4.0);
        assert_eq!(zoom_about(&mut v, 1.6, 60.0, 1000.0, 30.0), None, "56 s right of the left edge is 2800 points: past 1000");
        let pps = 50.0 * 1.6;
        assert_eq!((v.target_pps, v.target_scroll), (pps, 60.0 - 500.0 / pps));
        let mut v = view(50.0, 40.0);
        assert_eq!(zoom_about(&mut v, 1.6, 30.0, 1000.0, 30.0), None, "left of the view");
        assert_eq!(v.target_scroll, 30.0 - 500.0 / pps);
        // centring a time near the start would put the left edge before 0
        let mut v = view(50.0, 40.0);
        assert_eq!(zoom_about(&mut v, 1.6, 1.0, 1000.0, 30.0), None);
        assert_eq!(v.target_scroll, 0.0);
    }

    /// Mid-animation the x is where the time is drawn now (the current zoom and scroll), not where
    /// the targets will put it; the new zoom compounds the target one.
    #[test]
    fn the_x_comes_from_the_drawn_view_not_the_targets() {
        let mut v = TimelineView { pps: 50.0, target_pps: 75.0, scroll: 4.0, target_scroll: 7.0, ..TimelineView::default() };
        // the targets would draw 10 s at (10 - 7) * 75 = 225 points
        assert_eq!(zoom_about(&mut v, 1.6, 10.0, 1000.0, 30.0), Some(300.0));
        let pps = 75.0 * 1.6;
        assert_eq!((v.target_pps, v.target_scroll), (pps, 10.0 - 300.0 / pps));
        // drawn off screen while the targets would show it: centred
        let mut v = TimelineView { pps: 50.0, target_pps: 50.0, scroll: 40.0, target_scroll: 4.0, ..TimelineView::default() };
        assert_eq!(zoom_about(&mut v, 1.6, 10.0, 1000.0, 30.0), None);
        assert_eq!(v.target_scroll, 10.0 - 500.0 / (50.0 * 1.6), "centred, though the targets would show it at 300");
    }

    /// The zoom stops at the whole extent and at 24 000 points a second; the time still keeps its x.
    #[test]
    fn the_zoom_is_clamped_at_both_ends() {
        let floor = min_pps(30.0, 1000.0);
        let mut v = view(floor * 1.2, 0.0);
        let x = (10.0 * (floor * 1.2)) as f32;
        assert_eq!(zoom_about(&mut v, 1.0 / 1.6, 10.0, 1000.0, 30.0), Some(x));
        assert_eq!((v.target_pps, v.target_scroll), (floor, 0.0), "out no farther than the extent, and not before 0");
        let mut v = view(20_000.0, 10.0 - 200.0 / 20_000.0);
        let x = ((10.0 - v.scroll) * 20_000.0) as f32;
        assert_eq!(zoom_about(&mut v, 1.6, 10.0, 1000.0, 30.0), Some(x));
        assert_eq!((v.target_pps, v.target_scroll), (24_000.0, 10.0 - f64::from(x) / 24_000.0));
    }
}
