//! Multi-camera rendering: one angle of a multi-camera source sequence, and the Multi-Camera
//! view's grid of angles.
//!
//! The grid renders every shown angle at a reduced scale (sources are asked for small frames, so
//! YUV→RGB conversion and compositing run at the cell size) in parallel, and tiles them into one
//! image. A 2×2 grid of 1080p angles at ¼ scale costs about as much as one ½-resolution frame of
//! compositing plus the four decodes.

use filmcraft_project::{ItemId, Project, Sequence};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::{Image, RenderOptions, SourceProvider, output_size};

/// Grid layout for `n` angles: (columns, rows). 1 → 1×1, 2–4 → 2×2, 5–9 → 3×3, 10 and more → 4×4
/// (more than 16 angles are paged, see [`page_layout`]).
pub fn grid_dims(n: usize) -> (usize, usize) {
    let c = (1..=4).find(|c| c * c >= n).unwrap_or(4);
    let r = n.div_ceil(c).max(1);
    (c, r.min(c))
}

/// One page of the Multi-Camera view's angle grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageLayout {
    pub cols: usize,
    pub rows: usize,
    /// Cells per page (`cols × rows`).
    pub per_page: usize,
    /// Number of pages (≥ 1).
    pub pages: usize,
    /// The page shown (clamped to `pages - 1`).
    pub page: usize,
    /// Index (in shown order) of the first angle on the page.
    pub first: usize,
    /// Angles on this page.
    pub count: usize,
}

/// Layout of page `page` of a grid of `n` shown angles. `side` fixes the grid (2 = 2×2, 3 = 3×3,
/// 4 = 4×4); `None` is automatic: the smallest square grid that holds every angle, up to 4×4, and
/// pages of 16 beyond that (Premiere pages a multi-camera source with more than 16 angles).
pub fn page_layout(n: usize, side: Option<usize>, page: usize) -> PageLayout {
    let (cols, rows) = match side {
        Some(s) => (s.clamp(1, 4), s.clamp(1, 4)),
        None if n > 16 => (4, 4),
        None => grid_dims(n.max(1)),
    };
    let per_page = cols * rows;
    let pages = n.div_ceil(per_page).max(1);
    let page = page.min(pages - 1);
    let first = page * per_page;
    PageLayout { cols, rows, per_page, pages, page, first, count: n.saturating_sub(first).min(per_page) }
}

/// Decode / composite scale of the grid's cells: the cell's on-screen size relative to the
/// sequence frame, never more than the playback resolution. With Auto-Adjust Multi-Camera Playback
/// Quality on, playback renders the cells at half that (a quarter for grids of more than four
/// angles), so many angles keep up in real time; paused frames are always at the cell's size.
pub fn grid_cell_scale(seq_width: u32, cell_px: f32, playback_scale: f32, playing: bool, auto_adjust: bool, cells: usize) -> f32 {
    let base = playback_scale.min(cell_px / seq_width.max(1) as f32).clamp(1.0 / 32.0, 1.0);
    if playing && auto_adjust {
        let k = if cells > 4 { 0.25 } else { 0.5 };
        (base * k).max(1.0 / 32.0)
    } else {
        base
    }
}

/// Render angle `angle` of the multi-camera source sequence `seq` at its time `t` (transparent
/// for an audio-only angle). The image is in the sequence's working space unless `opts.depth == 0`
/// and `!opts.working_output` (then display-ready like [`crate::render_sequence`]).
pub fn render_angle(project: &Project, seq: &Sequence, angle: usize, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> crate::Result<Image> {
    match seq.angle_video_track_index(angle) {
        Some(ti) => crate::render_seq_tracks(project, seq, t, opts, sources, Some(ti)),
        None => {
            let (w, h) = output_size(seq, opts.scale);
            Ok(Image::new(w, h))
        }
    }
}

/// The Multi-Camera view's angle grid of the sequence `item` used as a multi-camera source at its
/// time `t`: the shown angles (Edit Cameras), each at `cell_scale` of the sequence frame size,
/// tiled left to right, top to bottom over black. Returns the image and the angles in grid order.
/// Equivalent to [`render_grid_page`] with the automatic layout, first page.
pub fn render_grid(project: &Project, item: ItemId, t: Tick, cell_scale: f32, sources: &dyn SourceProvider) -> crate::Result<Option<(Image, Vec<usize>)>> {
    render_grid_page(project, item, t, cell_scale, None, 0, sources)
}

/// One page of the angle grid (see [`page_layout`]); only that page's angles are decoded, each at
/// `cell_scale` (sources are asked for frames of that scale).
pub fn render_grid_page(
    project: &Project,
    item: ItemId,
    t: Tick,
    cell_scale: f32,
    side: Option<usize>,
    page: usize,
    sources: &dyn SourceProvider,
) -> crate::Result<Option<(Image, Vec<usize>)>> {
    let Some(seq) = project.sequence(item) else { return Ok(None) };
    let shown = seq.cameras().shown_angles();
    let l = page_layout(shown.len(), side, page);
    let angles: Vec<usize> = shown.iter().copied().skip(l.first).take(l.count).collect();
    let (cols, rows) = (l.cols, l.rows);
    let (cw, ch) = output_size(seq, cell_scale);
    let opts = RenderOptions { scale: cell_scale, ..Default::default() };
    let cells: Vec<Image> = angles.par_iter().map(|&a| render_angle(project, seq, a, t, opts, sources)).collect::<crate::Result<Vec<_>>>()?;
    let mut out = Image::filled(cw * cols, ch * rows, [0.0, 0.0, 0.0, 1.0]);
    for (k, cell) in cells.iter().enumerate().take(cols * rows) {
        let (x0, y0) = ((k % cols) * cw, (k / cols) * ch);
        for y in 0..ch.min(cell.h) {
            for x in 0..cw.min(cell.w) {
                let s = (y * cell.w + x) * 4;
                let d = ((y0 + y) * out.w + x0 + x) * 4;
                // cells over black: premultiplied over an opaque background
                let a = cell.px[s + 3];
                for c in 0..3 {
                    out.px[d + c] = cell.px[s + c] + out.px[d + c] * (1.0 - a);
                }
            }
        }
    }
    Ok(Some((out, angles)))
}

/// A thumbnail of one angle (Edit Cameras dialog): the angle at `scale`, display-ready.
pub fn render_angle_thumbnail(
    project: &Project,
    item: ItemId,
    angle: usize,
    t: Tick,
    scale: f32,
    sources: &dyn SourceProvider,
) -> crate::Result<Option<Image>> {
    let Some(seq) = project.sequence(item) else { return Ok(None) };
    let opts = RenderOptions { scale, ..Default::default() };
    Ok(Some(render_angle(project, seq, angle, t, opts, sources)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_beyond_sixteen_angles() {
        // automatic layouts
        assert_eq!((page_layout(1, None, 0).cols, page_layout(1, None, 0).rows), (1, 1));
        assert_eq!(page_layout(4, None, 0).per_page, 4);
        assert_eq!(page_layout(7, None, 0).per_page, 9);
        assert_eq!(page_layout(16, None, 0).pages, 1);
        // 20 angles: two pages of 4×4, the second holding angles 17–20
        let l = page_layout(20, None, 1);
        assert_eq!((l.cols, l.rows, l.pages, l.first, l.count), (4, 4, 2, 16, 4));
        // fixed 2×2: five pages, the last holding one angle; out-of-range pages clamp
        let l = page_layout(17, Some(2), 9);
        assert_eq!((l.per_page, l.pages, l.page, l.first, l.count), (4, 5, 4, 16, 1));
        let l = page_layout(17, Some(3), 1);
        assert_eq!((l.per_page, l.pages, l.first, l.count), (9, 2, 9, 8));
        // no angles: one empty page
        assert_eq!(page_layout(0, None, 3).count, 0);
    }

    #[test]
    fn auto_adjust_lowers_playback_cell_scale() {
        // a 480-pixel cell of a 1920 sequence at full playback resolution: ¼
        assert_eq!(grid_cell_scale(1920, 480.0, 1.0, false, true, 16), 0.25);
        // playing: unchanged without Auto-Adjust, ½ / ¼ of that with it
        assert_eq!(grid_cell_scale(1920, 480.0, 1.0, true, false, 16), 0.25);
        assert_eq!(grid_cell_scale(1920, 480.0, 1.0, true, true, 4), 0.125);
        assert_eq!(grid_cell_scale(1920, 480.0, 1.0, true, true, 9), 0.0625);
        // never above the playback resolution, never below 1/32
        assert_eq!(grid_cell_scale(1920, 1920.0, 0.5, false, false, 4), 0.5);
        assert_eq!(grid_cell_scale(1920, 10.0, 1.0, true, true, 16), 1.0 / 32.0);
    }
}
