//! What a press on the Timeline grabs (#259).
//!
//! Pure geometry and rules, shared by the cursor, the hover bracket, the press handler and the
//! control channel's `ui.timeline.hit`, so what the pointer shows is what a press does.

use std::cmp::Ordering;

use egui::{Modifiers, Pos2};
use filmcraft_edit::Edge;
use filmcraft_project::{ClipId, Sequence, Track, TrackId, TransitionId};

use super::timeline::Layout;
use crate::state::Tool;

/// How close (px) to a cut the Selection tool rolls instead of rippling when Settings ▸ Trim ▸
/// "Allow Selection tool to choose Roll and Ripple trims without modifier key" is on.
pub const ROLL_PX: f32 = 2.5;

/// Widest an edge zone gets (px), on each side of a clip edge.
pub const EDGE_PX: f32 = 7.0;

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
pub(crate) fn edge_geometry(seq: &Sequence, layout: &Layout, track: TrackId, clip: ClipId, edge: Edge, x: f32) -> (f32, bool) {
    let Some(tr) = seq.track(track) else { return (f32::MAX, false) };
    let Some(it) = tr.item(clip) else { return (f32::MAX, false) };
    let (cut, neighbour) = match edge {
        Edge::Out => (it.end(), tr.items.iter().any(|x| x.start == it.end())),
        Edge::In => (it.start, tr.items.iter().any(|x| x.end() == it.start)),
    };
    ((layout.x_of(cut) - x).abs(), neighbour)
}

/// What is at a screen point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Ruler,
    Clip {
        track: TrackId,
        clip: ClipId,
        edge: Option<Edge>,
    },
    /// A transition; `edge` is its start (`In`) or end (`Out`) when the press is on one of its
    /// ends (dragging it changes the duration), `None` on its middle (dragging slides it).
    Transition {
        track: TrackId,
        id: TransitionId,
        edge: Option<Edge>,
    },
    Empty {
        track: TrackId,
    },
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
    let Some(tr) = seq.track(row.track) else { return Hit::None };
    for trn in &tr.transitions {
        let x0 = layout.x_of(trn.start);
        let x1 = layout.x_of(trn.end());
        if pos.x >= x0 && pos.x <= x1 && pos.y > row.rect.min.y + 17.0 {
            // the ends are edge zones, narrower on a short transition so its middle stays grabbable
            let zone = EDGE_PX.min((x1 - x0) / 3.0);
            let edge = if pos.x - x0 <= zone {
                Some(Edge::In)
            } else if x1 - pos.x <= zone {
                Some(Edge::Out)
            } else {
                None
            };
            return Hit::Transition { track: row.track, id: trn.id, edge };
        }
    }
    if let Some((clip, edge)) = edge_at(tr, layout, pos.x) {
        return Hit::Clip { track: row.track, clip, edge: Some(edge) };
    }
    match tr.item_at(layout.tick_at(pos.x)) {
        Some(it) => Hit::Clip { track: row.track, clip: it.id, edge: None },
        None => Hit::Empty { track: row.track },
    }
}

/// A clip edge within reach of the pointer.
#[derive(Clone, Copy)]
struct Reach {
    clip: ClipId,
    edge: Edge,
    /// The pointer is inside the clip (or exactly on the edge).
    inside: bool,
    /// How far the pointer is from the edge (px).
    dist: f32,
}

impl Reach {
    /// Inside a clip beats outside one (so at a cut the side of the cut picks the clip), then the
    /// nearer edge wins; an exact tie (right on a cut, or mid-way across a narrow gap) goes to the
    /// later clip's In edge.
    fn beats(&self, other: &Reach) -> bool {
        if self.inside != other.inside {
            return self.inside;
        }
        match self.dist.partial_cmp(&other.dist) {
            Some(Ordering::Less) => true,
            Some(Ordering::Greater) => false,
            _ => self.edge == Edge::In && other.edge != Edge::In,
        }
    }
}

/// The edge zone (px, on each side of an edge) of a clip `w` px wide on screen: a third of the
/// clip, at most [`EDGE_PX`], so the middle third of a narrow clip still moves it. `None` when the
/// clip has no usable width (zoomed far out, or a broken zoom): it has no edges then.
fn zone(w: f32) -> Option<f32> {
    if w.is_finite() && w > 0.0 { Some(EDGE_PX.min(w / 3.0)) } else { None }
}

/// The clip edge on `tr` a pointer at `x` grabs (#259), whatever the order of `tr.items`.
fn edge_at(tr: &Track, layout: &Layout, x: f32) -> Option<(ClipId, Edge)> {
    let mut best: Option<Reach> = None;
    for it in &tr.items {
        let (x0, x1) = (layout.x_of(it.start), layout.x_of(it.end()));
        let Some(e) = zone(x1 - x0) else { continue };
        // the pointer's distance into the clip from each of its edges
        for (edge, d) in [(Edge::In, x - x0), (Edge::Out, x1 - x)] {
            if !(-e..=e).contains(&d) {
                continue;
            }
            let r = Reach { clip: it.id, edge, inside: d >= 0.0, dist: d.abs() };
            if best.is_none_or(|b| r.beats(&b)) {
                best = Some(r);
            }
        }
    }
    best.map(|r| (r.clip, r.edge))
}

/// The trim a press on a clip edge starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EdgeKind {
    /// Regular trim: the edge moves, opening or covering a gap.
    Trim,
    /// Ripple trim: the edge moves and later clips follow.
    Ripple,
    /// Roll the cut between `left` and `right`.
    Roll { left: ClipId, right: ClipId },
    /// Rate Stretch: the clip's speed changes to fill its new length.
    Stretch,
    /// Remix: a music clip is re-planned to its new length (Out edges only).
    Remix,
}

impl EdgeKind {
    /// Its name on the control channel (`ui.timeline.hit` → `kind`).
    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Trim => "trim",
            EdgeKind::Ripple => "ripple",
            EdgeKind::Roll { .. } => "roll",
            EdgeKind::Stretch => "rateStretch",
            EdgeKind::Remix => "remix",
        }
    }
}

/// What a press of a tool at a point grabs: a clip edge and the trim it starts, or whatever else
/// is there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grab {
    Edge {
        track: TrackId,
        clip: ClipId,
        edge: Edge,
        kind: EdgeKind,
    },
    /// An audio clip's Volume line (Selection and Pen tools): a drag changes the level, a Pen or
    /// Cmd/Ctrl click adds a keyframe.
    Volume {
        track: TrackId,
        clip: ClipId,
    },
    /// Not an edge this tool trims: a clip body, a transition, an empty track, the ruler or
    /// nothing. A clip's `edge` may still be set, for tools that do something else there (Razor,
    /// Slip…).
    Other(Hit),
}

impl Grab {
    /// What is under the pointer, whatever the tool does with it.
    pub fn hit(self) -> Hit {
        match self {
            Grab::Edge { track, clip, edge, .. } => Hit::Clip { track, clip, edge: Some(edge) },
            Grab::Volume { track, clip } => Hit::Clip { track, clip, edge: None },
            Grab::Other(h) => h,
        }
    }
}

/// What a press of `tool` with `mods` at `pos` grabs (#259): the one place that decides which
/// tools trim at a clip edge and which trim they start. `roll_ripple` is Settings ▸ Trim ▸ "Allow
/// Selection tool to choose Roll and Ripple trims without modifier key".
pub fn grab_at(seq: &Sequence, layout: &Layout, pos: Pos2, tool: Tool, mods: Modifiers, roll_ripple: bool) -> Grab {
    let h = hit(seq, layout, pos);
    if let Hit::Clip { track, clip, edge: None } = h
        && matches!(tool, Tool::Selection | Tool::Pen)
        && super::timeline_volume::on_line(seq, layout, clip, pos)
    {
        return Grab::Volume { track, clip };
    }
    let Hit::Clip { track, clip, edge: Some(edge) } = h else { return Grab::Other(h) };
    let kind = match tool {
        Tool::Selection => {
            let (dist, neighbour) = edge_geometry(seq, layout, track, clip, edge, pos.x);
            match selection_trim_kind(roll_ripple, mods.command, mods.shift, dist, neighbour) {
                "roll" => roll(seq, track, clip, edge),
                "ripple" => EdgeKind::Ripple,
                _ => EdgeKind::Trim,
            }
        }
        Tool::Ripple => EdgeKind::Ripple,
        Tool::Rolling => roll(seq, track, clip, edge),
        Tool::RateStretch => EdgeKind::Stretch,
        Tool::Remix if edge == Edge::Out => EdgeKind::Remix,
        _ => return Grab::Other(h),
    };
    Grab::Edge { track, clip, edge, kind }
}

/// A roll of the cut at `clip`'s `edge`, or a regular trim when no clip touches that cut on the
/// other side (there is nothing to roll against).
fn roll(seq: &Sequence, track: TrackId, clip: ClipId, edge: Edge) -> EdgeKind {
    let Some(tr) = seq.track(track) else { return EdgeKind::Trim };
    let Some(it) = tr.item(clip) else { return EdgeKind::Trim };
    let pair = match edge {
        Edge::Out => tr.items.iter().find(|x| x.start == it.end()).map(|r| (clip, r.id)),
        Edge::In => tr.items.iter().find(|x| x.end() == it.start).map(|l| (l.id, clip)),
    };
    match pair {
        Some((left, right)) => EdgeKind::Roll { left, right },
        None => EdgeKind::Trim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2};
    use filmcraft_project::{ItemId, Label, Project, SequenceSettings, TrackItem, TrackKind};
    use filmcraft_time::{FrameRate, Tick};

    use crate::panels::timeline::Row;

    /// The row's top. Points are hit-tested 10 px below it, above where transitions are hit.
    const TOP: f32 = 100.0;

    /// A clip on screen from `x0` to `x1` px (the layout is 100 px per second).
    fn clip(id: u64, x0: f64, x1: f64) -> TrackItem {
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: format!("c{id}"),
            label: Label::Iris,
            start: Tick::from_seconds_f64(x0 / 100.0),
            duration: Tick::from_seconds_f64((x1 - x0) / 100.0),
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: Vec::new(),
            markers: Vec::new(),
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    /// A: 100–300, B: 300–500 (sharing a cut with A), C: 504–700 (after a 4 px gap), D: 800–809
    /// (9 px wide, so its zones are 3 px).
    fn clips() -> Vec<TrackItem> {
        vec![clip(1, 100.0, 300.0), clip(2, 300.0, 500.0), clip(3, 504.0, 700.0), clip(4, 800.0, 809.0)]
    }

    /// One video track holding `items`, laid out at 100 px/s from x = 0.
    fn fixture(items: Vec<TrackItem>) -> (Sequence, Layout, TrackId) {
        let mut p = Project::new("t");
        let s = p.new_sequence("s", SequenceSettings { frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 0, None);
        let mut seq = p.sequence(s).cloned().expect("sequence");
        let track = seq.video_tracks[0].id;
        seq.video_tracks[0].items = items;
        let row = Rect::from_min_max(pos2(0.0, TOP), pos2(2000.0, TOP + 60.0));
        let layout = Layout {
            content: row,
            ruler: Rect::from_min_max(pos2(0.0, 0.0), pos2(2000.0, 40.0)),
            rows: vec![Row { track, kind: TrackKind::Video, index: 0, rect: row, lane: false }],
            pps: 100.0,
            scroll: 0.0,
            split_y: TOP + 60.0,
        };
        (seq, layout, track)
    }

    fn at(x: f32) -> Pos2 {
        pos2(x, TOP + 10.0)
    }

    fn edge_hit(track: TrackId, clip: u64, edge: Edge) -> Hit {
        Hit::Clip { track, clip: ClipId(clip), edge: Some(edge) }
    }

    #[test]
    fn the_side_of_a_cut_picks_the_clip() {
        let (seq, l, tr) = fixture(clips());
        assert_eq!(hit(&seq, &l, at(297.0)), edge_hit(tr, 1, Edge::Out));
        assert_eq!(hit(&seq, &l, at(303.0)), edge_hit(tr, 2, Edge::In));
        // right on the cut: the later clip
        assert_eq!(hit(&seq, &l, at(300.0)), edge_hit(tr, 2, Edge::In));
    }

    #[test]
    fn a_narrow_gap_splits_at_its_middle() {
        let (seq, l, tr) = fixture(clips());
        assert_eq!(hit(&seq, &l, at(501.0)), edge_hit(tr, 2, Edge::Out));
        assert_eq!(hit(&seq, &l, at(503.0)), edge_hit(tr, 3, Edge::In));
        // exactly mid-way: the later clip
        assert_eq!(hit(&seq, &l, at(502.0)), edge_hit(tr, 3, Edge::In));
    }

    #[test]
    fn free_edges_reach_into_the_gap_beside_them() {
        let (seq, l, tr) = fixture(clips());
        assert_eq!(hit(&seq, &l, at(95.0)), edge_hit(tr, 1, Edge::In));
        assert_eq!(hit(&seq, &l, at(705.0)), edge_hit(tr, 3, Edge::Out));
        assert_eq!(hit(&seq, &l, at(92.0)), Hit::Empty { track: tr });
        assert_eq!(hit(&seq, &l, at(200.0)), Hit::Clip { track: tr, clip: ClipId(1), edge: None });
    }

    #[test]
    fn edges_do_not_depend_on_the_order_of_the_items() {
        let mut reversed = clips();
        reversed.reverse();
        let (a, la, _) = fixture(clips());
        let (b, lb, _) = fixture(reversed);
        // edge points only: finding the clip under a body point relies on the items being sorted
        for x in [297.0, 300.0, 303.0, 501.0, 502.0, 503.0, 95.0, 705.0, 802.0, 807.0] {
            assert_eq!(hit(&a, &la, at(x)), hit(&b, &lb, at(x)), "x = {x}");
        }
    }

    #[test]
    fn a_narrow_clip_keeps_its_middle_third() {
        let (seq, l, tr) = fixture(clips());
        assert_eq!(hit(&seq, &l, at(802.0)), edge_hit(tr, 4, Edge::In));
        assert_eq!(hit(&seq, &l, at(807.0)), edge_hit(tr, 4, Edge::Out));
        assert_eq!(hit(&seq, &l, at(804.5)), Hit::Clip { track: tr, clip: ClipId(4), edge: None });
        // 4 px outside it is beyond its 3 px zone
        assert_eq!(hit(&seq, &l, at(796.0)), Hit::Empty { track: tr });
    }

    #[test]
    fn hostile_points_and_zooms_never_panic_or_grab_an_edge() {
        let (seq, l, tr) = fixture(vec![clip(5, 1000.0, 1000.0)]);
        // a zero-length clip has no edges, even right on it
        assert_eq!(hit(&seq, &l, at(1000.0)), Hit::Empty { track: tr });
        for p in [pos2(f32::NAN, TOP + 10.0), pos2(f32::INFINITY, TOP + 10.0), pos2(f32::NEG_INFINITY, TOP + 10.0), pos2(300.0, f32::NAN), pos2(300.0, 5000.0)]
        {
            assert_eq!(hit(&seq, &l, p), Hit::None, "{p:?}");
        }
        let (seq, l, _) = fixture(clips());
        assert_eq!(hit(&seq, &l, pos2(300.0, 20.0)), Hit::Ruler);
        for pps in [0.0, f64::NAN, f64::INFINITY] {
            let broken = Layout { pps, ..l.clone() };
            assert!(!matches!(hit(&seq, &broken, at(300.0)), Hit::Clip { edge: Some(_), .. }), "pps {pps}");
        }
    }

    use egui::Modifiers;

    use crate::state::Tool;

    const ROLL_AB: EdgeKind = EdgeKind::Roll { left: ClipId(1), right: ClipId(2) };

    fn grab(seq: &Sequence, l: &Layout, tool: Tool, x: f32, mods: Modifiers, roll_ripple: bool) -> Grab {
        grab_at(seq, l, at(x), tool, mods, roll_ripple)
    }

    fn edge_grab(track: TrackId, clip: u64, edge: Edge, kind: EdgeKind) -> Grab {
        Grab::Edge { track, clip: ClipId(clip), edge, kind }
    }

    #[test]
    fn selection_tool_trims_by_modifier_and_setting() {
        let (seq, l, tr) = fixture(clips());
        let (none, cmd, cmd_shift) = (Modifiers::NONE, Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT);
        let sel = |x, mods, pref| grab(&seq, &l, Tool::Selection, x, mods, pref);
        assert_eq!(sel(297.0, none, false), edge_grab(tr, 1, Edge::Out, EdgeKind::Trim));
        assert_eq!(sel(303.0, cmd, false), edge_grab(tr, 2, Edge::In, EdgeKind::Ripple));
        assert_eq!(sel(303.0, cmd_shift, false), edge_grab(tr, 2, Edge::In, ROLL_AB));
        // Settings ▸ Trim: right on the cut rolls, elsewhere on the edge ripples
        assert_eq!(sel(298.0, none, true), edge_grab(tr, 1, Edge::Out, ROLL_AB));
        assert_eq!(sel(302.0, none, true), edge_grab(tr, 2, Edge::In, ROLL_AB));
        assert_eq!(sel(305.0, none, true), edge_grab(tr, 2, Edge::In, EdgeKind::Ripple));
        // a roll with no clip on the other side of the cut is a regular trim
        assert_eq!(sel(102.0, cmd_shift, false), edge_grab(tr, 1, Edge::In, EdgeKind::Trim));
        // mid-clip: the clip itself
        assert_eq!(sel(200.0, none, false), Grab::Other(Hit::Clip { track: tr, clip: ClipId(1), edge: None }));
    }

    #[test]
    fn which_tools_trim_at_an_edge() {
        let (seq, l, tr) = fixture(clips());
        let g = |tool, x| grab(&seq, &l, tool, x, Modifiers::NONE, false);
        assert_eq!(g(Tool::Ripple, 297.0), edge_grab(tr, 1, Edge::Out, EdgeKind::Ripple));
        assert_eq!(g(Tool::Rolling, 303.0), edge_grab(tr, 2, Edge::In, ROLL_AB));
        assert_eq!(g(Tool::Rolling, 102.0), edge_grab(tr, 1, Edge::In, EdgeKind::Trim));
        assert_eq!(g(Tool::RateStretch, 297.0), edge_grab(tr, 1, Edge::Out, EdgeKind::Stretch));
        assert_eq!(g(Tool::Remix, 297.0), edge_grab(tr, 1, Edge::Out, EdgeKind::Remix));
        // Remix has Out edges only
        assert_eq!(g(Tool::Remix, 303.0), Grab::Other(edge_hit(tr, 2, Edge::In)));
        for tool in [Tool::Razor, Tool::Slip, Tool::Slide, Tool::Hand, Tool::Zoom, Tool::TrackSelectForward] {
            assert_eq!(g(tool, 297.0), Grab::Other(edge_hit(tr, 1, Edge::Out)), "{tool:?}");
        }
    }

    #[test]
    fn edge_kind_names_for_the_control_channel() {
        assert_eq!(
            [EdgeKind::Trim, EdgeKind::Ripple, ROLL_AB, EdgeKind::Stretch, EdgeKind::Remix].map(EdgeKind::name),
            ["trim", "ripple", "roll", "rateStretch", "remix"]
        );
    }

    #[test]
    fn a_grab_reports_what_is_under_it() {
        let (seq, l, tr) = fixture(clips());
        assert_eq!(grab(&seq, &l, Tool::Selection, 303.0, Modifiers::NONE, false).hit(), edge_hit(tr, 2, Edge::In));
        assert_eq!(grab(&seq, &l, Tool::Razor, 303.0, Modifiers::NONE, false).hit(), edge_hit(tr, 2, Edge::In));
    }
}
