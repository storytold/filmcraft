//! Icons drawn from Tabler Icons SVG files (MIT, v3.49.0). Outline files live in
//! `assets/icons/tabler/`, filled files in `assets/icons/tabler/filled/`, and FilmCraft's own
//! drawings in the Tabler style in `assets/icons/filmcraft/`. Each file has a `.attribution`
//! sidecar and a row in `ATTRIBUTION.md`.
//!
//! The files are compiled in as text, and `TABLE` maps each icon to its file. The `d` attribute of
//! each drawn `<path>` of an outline file is parsed once, on first use, into stroked sub-paths on
//! the 24 × 24 grid. Drawing: the grid is centred in the rect and
//! scaled by `min(w, h) / 24`; the stroke is 2 grid units, rounded to whole device pixels (1 px at
//! 1x, 3 px at 2x); horizontal and vertical segments sit on whole pixels. Curves keep their exact
//! positions.
//!
//! A filled file (`fill="currentColor"` on the root, no stroke) is drawn differently: each sub-path
//! is one filled convex polygon, flattened with the same tolerance and drawn at its exact
//! positions. A filled sub-path that is not convex is a parse error.
//!
//! Round joins and caps. egui strokes a polyline with mitred joins and draws no caps, and a disc
//! cannot cut off a mitre tip. So each SVG command is stroked as its own polyline with butt ends,
//! and a disc as wide as the stroke goes on every command boundary and on both ends of an open
//! sub-path. The discs give the round joins and caps. Where two strokes meet inside a join they
//! overlap, so a translucent colour is slightly more opaque at those spots.
//!
//! Hostile input: parsing never panics. Sizes are checked before anything grows, every number must
//! be finite and within [`MAX_COORD`], and every failure is an [`SvgError`].

use std::f32::consts::TAU;
use std::fmt;
use std::sync::OnceLock;

use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, pos2};

use crate::icons::Icon;

/// The Tabler outline canvas: 24 × 24 grid units.
const GRID: f32 = 24.0;
const HALF_GRID: f32 = GRID / 2.0;
/// Stroke width in grid units (Tabler outline: 2 units, centred on the path).
const STROKE_UNITS: f32 = 2.0;
/// Largest distance, in grid units, between a curve and its polyline. At 128 pt and 2x one unit is
/// about 10.7 px, so 0.1 units (a pixel of chord error) showed visible facets; 0.01 keeps it under
/// a tenth of a pixel for a few more samples per curve.
const TOLERANCE: f32 = 0.01;
/// Steps per curve segment, at most.
const MAX_STEPS: usize = 256;
/// Coordinates beyond this are hostile input (the grid is 24 units wide).
const MAX_COORD: f32 = 1.0e6;
/// Adjacent points closer than this in x or y (grid units) make a level or plumb pair.
const FLAT_EPS: f32 = 1.0e-3;
/// Arc radii below this are treated as zero, which SVG draws as a straight line.
const MIN_RADIUS: f32 = 1.0e-6;
/// Smallest angle step between arc samples, in radians.
const MIN_ANGLE_STEP: f32 = 0.01;
/// A filled polygon's turn may go the wrong way by this much (radians) before it counts as concave.
const TURN_EPS: f32 = 1.0e-2;
/// The turns of a convex polygon add up to one full turn, within this (radians).
const TURN_TOTAL_EPS: f32 = 2.0e-2;
/// Input limits. Each is checked before the allocation it guards grows.
const MAX_SVG_BYTES: usize = 64 * 1024;
const MAX_D_BYTES: usize = 16 * 1024;
const MAX_PATHS: usize = 64;
const MAX_COMMANDS: usize = 1024;
const MAX_SUBPATHS: usize = 256;
const MAX_POINTS: usize = 8192;
const COMMANDS: &[u8] = b"MmLlHhVvCcSsQqTtAaZz";

/// Every icon drawn from an SVG file: the icon, the file's name (for log messages) and the file.
const TABLE: &[(Icon, &str, &str)] = &[
    (Icon::Selection, "pointer", include_str!("../../../assets/icons/tabler/pointer.svg")),
    (Icon::TrackSelectFwd, "chevrons-right", include_str!("../../../assets/icons/tabler/chevrons-right.svg")),
    (Icon::TrackSelectBack, "chevrons-left", include_str!("../../../assets/icons/tabler/chevrons-left.svg")),
    (Icon::Ripple, "arrow-bar-right", include_str!("../../../assets/icons/tabler/arrow-bar-right.svg")),
    (Icon::Rolling, "arrow-bar-both", include_str!("../../../assets/icons/tabler/arrow-bar-both.svg")),
    (Icon::RateStretch, "arrow-autofit-width", include_str!("../../../assets/icons/tabler/arrow-autofit-width.svg")),
    (Icon::Remix, "arrows-shuffle", include_str!("../../../assets/icons/tabler/arrows-shuffle.svg")),
    (Icon::Razor, "blade", include_str!("../../../assets/icons/tabler/blade.svg")),
    (Icon::Slip, "arrows-horizontal", include_str!("../../../assets/icons/tabler/arrows-horizontal.svg")),
    (Icon::Slide, "arrows-move-horizontal", include_str!("../../../assets/icons/tabler/arrows-move-horizontal.svg")),
    (Icon::Pen, "pencil", include_str!("../../../assets/icons/tabler/pencil.svg")),
    (Icon::Rectangle, "rectangle", include_str!("../../../assets/icons/tabler/rectangle.svg")),
    (Icon::Ellipse, "circle", include_str!("../../../assets/icons/tabler/circle.svg")),
    (Icon::Hand, "hand-stop", include_str!("../../../assets/icons/tabler/hand-stop.svg")),
    (Icon::Zoom, "zoom-in", include_str!("../../../assets/icons/tabler/zoom-in.svg")),
    (Icon::Type, "typography", include_str!("../../../assets/icons/tabler/typography.svg")),
    (Icon::Play, "player-play", include_str!("../../../assets/icons/tabler/filled/player-play.svg")),
    (Icon::Pause, "player-pause", include_str!("../../../assets/icons/tabler/filled/player-pause.svg")),
    (Icon::StepBack, "player-skip-back", include_str!("../../../assets/icons/tabler/player-skip-back.svg")),
    (Icon::StepFwd, "player-skip-forward", include_str!("../../../assets/icons/tabler/player-skip-forward.svg")),
    (Icon::GoToIn, "arrow-bar-to-left", include_str!("../../../assets/icons/tabler/arrow-bar-to-left.svg")),
    (Icon::GoToOut, "arrow-bar-to-right", include_str!("../../../assets/icons/tabler/arrow-bar-to-right.svg")),
    (Icon::MarkIn, "brackets-contain-start", include_str!("../../../assets/icons/tabler/brackets-contain-start.svg")),
    (Icon::MarkOut, "brackets-contain-end", include_str!("../../../assets/icons/tabler/brackets-contain-end.svg")),
    (Icon::Marker, "bookmark", include_str!("../../../assets/icons/tabler/bookmark.svg")),
    (Icon::Insert, "inbox", include_str!("../../../assets/icons/tabler/inbox.svg")),
    (Icon::Overwrite, "replace", include_str!("../../../assets/icons/tabler/replace.svg")),
    (Icon::Lift, "upload", include_str!("../../../assets/icons/tabler/upload.svg")),
    (Icon::Extract, "package-export", include_str!("../../../assets/icons/tabler/package-export.svg")),
    (Icon::Camera, "camera", include_str!("../../../assets/icons/tabler/camera.svg")),
    (Icon::Loop, "repeat", include_str!("../../../assets/icons/tabler/repeat.svg")),
    (Icon::Wrench, "tool", include_str!("../../../assets/icons/tabler/tool.svg")),
    (Icon::Plus, "plus", include_str!("../../../assets/icons/tabler/plus.svg")),
    (Icon::Eye, "eye", include_str!("../../../assets/icons/tabler/eye.svg")),
    (Icon::EyeOff, "eye-off", include_str!("../../../assets/icons/tabler/eye-off.svg")),
    (Icon::Speaker, "volume", include_str!("../../../assets/icons/tabler/volume.svg")),
    (Icon::Mute, "volume-off", include_str!("../../../assets/icons/tabler/volume-off.svg")),
    (Icon::Lock, "lock", include_str!("../../../assets/icons/tabler/lock.svg")),
    (Icon::Unlock, "lock-open", include_str!("../../../assets/icons/tabler/lock-open.svg")),
    (Icon::SyncLock, "sync-lock", include_str!("../../../assets/icons/filmcraft/sync-lock.svg")),
    (Icon::Mic, "microphone", include_str!("../../../assets/icons/tabler/microphone.svg")),
    (Icon::Folder, "folder", include_str!("../../../assets/icons/tabler/folder.svg")),
    (Icon::Film, "movie", include_str!("../../../assets/icons/tabler/movie.svg")),
    (Icon::Sequence, "timeline", include_str!("../../../assets/icons/tabler/timeline.svg")),
    (Icon::Audio, "wave-sine", include_str!("../../../assets/icons/tabler/wave-sine.svg")),
    (Icon::Image, "photo", include_str!("../../../assets/icons/tabler/photo.svg")),
    (Icon::Search, "search", include_str!("../../../assets/icons/tabler/search.svg")),
    (Icon::ListView, "list", include_str!("../../../assets/icons/tabler/list.svg")),
    (Icon::IconView, "layout-grid", include_str!("../../../assets/icons/tabler/layout-grid.svg")),
    (Icon::Freeform, "layout-dashboard", include_str!("../../../assets/icons/tabler/layout-dashboard.svg")),
    (Icon::NewItem, "file-plus", include_str!("../../../assets/icons/tabler/file-plus.svg")),
    (Icon::Trash, "trash", include_str!("../../../assets/icons/tabler/trash.svg")),
    (Icon::Home, "home", include_str!("../../../assets/icons/tabler/home.svg")),
    (Icon::Workspaces, "layout-sidebar", include_str!("../../../assets/icons/tabler/layout-sidebar.svg")),
    (Icon::Hamburger, "menu-2", include_str!("../../../assets/icons/tabler/menu-2.svg")),
    (Icon::ChevronDown, "chevron-down", include_str!("../../../assets/icons/tabler/chevron-down.svg")),
    (Icon::ChevronRight, "chevron-right", include_str!("../../../assets/icons/tabler/chevron-right.svg")),
    (Icon::Magnet, "magnet", include_str!("../../../assets/icons/tabler/magnet.svg")),
    (Icon::Link, "link", include_str!("../../../assets/icons/tabler/link.svg")),
    (Icon::Keyframe, "diamond", include_str!("../../../assets/icons/tabler/diamond.svg")),
    (Icon::Stopwatch, "stopwatch", include_str!("../../../assets/icons/tabler/stopwatch.svg")),
    (Icon::Fx, "function", include_str!("../../../assets/icons/tabler/function.svg")),
    (Icon::Reset, "rotate-2", include_str!("../../../assets/icons/tabler/rotate-2.svg")),
    (Icon::Close, "x", include_str!("../../../assets/icons/tabler/x.svg")),
    (Icon::Fullscreen, "arrows-maximize", include_str!("../../../assets/icons/tabler/arrows-maximize.svg")),
    (Icon::Export, "file-export", include_str!("../../../assets/icons/tabler/file-export.svg")),
    (Icon::Import, "file-import", include_str!("../../../assets/icons/tabler/file-import.svg")),
    (Icon::Gear, "settings", include_str!("../../../assets/icons/tabler/settings.svg")),
    (Icon::Info, "info-circle", include_str!("../../../assets/icons/tabler/info-circle.svg")),
    (Icon::Captions, "subtitles", include_str!("../../../assets/icons/tabler/subtitles.svg")),
    (Icon::Adjust, "adjustments-horizontal", include_str!("../../../assets/icons/tabler/adjustments-horizontal.svg")),
    (Icon::Nest, "box-multiple", include_str!("../../../assets/icons/tabler/box-multiple.svg")),
    (Icon::Undo, "arrow-back-up", include_str!("../../../assets/icons/tabler/arrow-back-up.svg")),
    (Icon::Redo, "arrow-forward-up", include_str!("../../../assets/icons/tabler/arrow-forward-up.svg")),
    (Icon::Bell, "bell", include_str!("../../../assets/icons/tabler/bell.svg")),
    (Icon::Chat, "message-dots", include_str!("../../../assets/icons/tabler/message-dots.svg")),
    (Icon::Globe, "world", include_str!("../../../assets/icons/tabler/world.svg")),
    (Icon::Code, "code", include_str!("../../../assets/icons/tabler/code.svg")),
    (Icon::Sparkle, "sparkles", include_str!("../../../assets/icons/tabler/sparkles.svg")),
    (Icon::Grid, "grid-3x3", include_str!("../../../assets/icons/tabler/grid-3x3.svg")),
    (Icon::Square, "square", include_str!("../../../assets/icons/tabler/square.svg")),
    (Icon::Proxy, "picture-in-picture", include_str!("../../../assets/icons/tabler/picture-in-picture.svg")),
    (Icon::Offline, "unlink", include_str!("../../../assets/icons/tabler/unlink.svg")),
    (Icon::TrackMaskBack, "player-track-prev", include_str!("../../../assets/icons/tabler/player-track-prev.svg")),
    (Icon::TrackMaskBackFrame, "track-mask-back-frame", include_str!("../../../assets/icons/filmcraft/track-mask-back-frame.svg")),
    (Icon::TrackMaskFwdFrame, "track-mask-fwd-frame", include_str!("../../../assets/icons/filmcraft/track-mask-fwd-frame.svg")),
    (Icon::TrackMaskFwd, "player-track-next", include_str!("../../../assets/icons/tabler/player-track-next.svg")),
    (Icon::SortIcons, "sort-descending", include_str!("../../../assets/icons/tabler/sort-descending.svg")),
    (Icon::Automate, "wand", include_str!("../../../assets/icons/tabler/wand.svg")),
    (Icon::Star, "star", include_str!("../../../assets/icons/tabler/star.svg")),
    (Icon::ChevronLeft, "chevron-left", include_str!("../../../assets/icons/tabler/chevron-left.svg")),
    (Icon::ArrowUp, "arrow-up", include_str!("../../../assets/icons/tabler/arrow-up.svg")),
    (Icon::Drive, "device-floppy", include_str!("../../../assets/icons/tabler/device-floppy.svg")),
    (Icon::Network, "network", include_str!("../../../assets/icons/tabler/network.svg")),
    (Icon::Clock, "clock", include_str!("../../../assets/icons/tabler/clock.svg")),
    (Icon::Monitor, "device-desktop", include_str!("../../../assets/icons/tabler/device-desktop.svg")),
    (Icon::Sun, "sun", include_str!("../../../assets/icons/tabler/sun.svg")),
    (Icon::Moon, "moon", include_str!("../../../assets/icons/tabler/moon.svg")),
    (Icon::Eyedropper, "color-picker", include_str!("../../../assets/icons/tabler/color-picker.svg")),
];

/// The table, parsed on first use: icons are painted every frame. A file that fails to parse (the
/// tests catch that) logs a warning and draws nothing.
static PARSED: OnceLock<Vec<SvgIcon>> = OnceLock::new();

/// The SVG icon drawn for `icon`, or `None` when the table has no entry: `icons::paint` then draws
/// it in code.
pub fn svg_for(icon: Icon) -> Option<&'static SvgIcon> {
    let index = TABLE.iter().position(|(which, _, _)| *which == icon)?;
    let parsed = PARSED.get_or_init(|| TABLE.iter().map(|(_, name, svg)| load(name, svg)).collect());
    parsed.get(index)
}

fn load(name: &str, svg: &str) -> SvgIcon {
    parse_svg(svg).unwrap_or_else(|err| {
        log::warn!("tabler icon {name} not drawn: {err}");
        SvgIcon::default()
    })
}

/// Why an SVG could not be read. Parsing never panics: each rejected input ends in one of these.
#[derive(Clone, Debug, PartialEq)]
pub enum SvgError {
    /// The input is longer than `limit` bytes.
    TooLarge { limit: usize },
    /// The document has no `<path>` that is stroked.
    NoPath,
    /// A `<path>` tag that is not closed, or has no `d` attribute.
    Malformed { at: usize },
    /// The `d` attribute is empty or only white space.
    EmptyData,
    /// Path data that does not start with a moveto.
    NoMoveTo { at: usize },
    /// A letter that is not an SVG path command.
    UnknownCommand { at: usize, found: char },
    /// A character that cannot start a command or a number.
    UnexpectedChar { at: usize, found: char },
    /// A command that ran out of arguments.
    MissingArgument { at: usize },
    /// A malformed number, such as a lone sign or `1e`.
    BadNumber { at: usize },
    /// A number that is not finite or is beyond `MAX_COORD`.
    OutOfRange { at: usize },
    /// An arc flag that is neither `0` nor `1`.
    BadFlag { at: usize },
    /// An arc whose geometry is not finite.
    BadArc,
    /// A filled sub-path whose outline is not convex: each filled sub-path is one convex polygon.
    NotConvex,
    /// More commands, paths, sub-paths or points than the limit allows.
    TooMany { what: &'static str, limit: usize },
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "larger than {limit} bytes"),
            Self::NoPath => write!(f, "no stroked path"),
            Self::Malformed { at } => write!(f, "malformed <path> at byte {at}"),
            Self::EmptyData => write!(f, "empty path data"),
            Self::NoMoveTo { at } => write!(f, "path data must start with M or m (byte {at})"),
            Self::UnknownCommand { at, found } => write!(f, "unknown command {found:?} at byte {at}"),
            Self::UnexpectedChar { at, found } => write!(f, "unexpected {found:?} at byte {at}"),
            Self::MissingArgument { at } => write!(f, "missing argument at byte {at}"),
            Self::BadNumber { at } => write!(f, "bad number at byte {at}"),
            Self::OutOfRange { at } => write!(f, "number out of range at byte {at}"),
            Self::BadFlag { at } => write!(f, "arc flag must be 0 or 1 (byte {at})"),
            Self::BadArc => write!(f, "arc geometry is not finite"),
            Self::NotConvex => write!(f, "filled sub-path is not convex"),
            Self::TooMany { what, limit } => write!(f, "more than {limit} {what}"),
        }
    }
}

impl std::error::Error for SvgError {}

/// One sub-path on the 24-unit grid: stroked (outline files), or the corners of a filled convex polygon
/// (filled files).
#[derive(Clone, Debug, Default, PartialEq)]
struct Subpath {
    /// Vertices in drawing order. Each consecutive pair is one segment.
    pts: Vec<[f32; 2]>,
    /// Indices into `pts` where an SVG command ends: the joins, and both ends of an open sub-path.
    joints: Vec<usize>,
    /// `straight[i]`: the segment `pts[i]` → `pts[i + 1]` is a line command (L, H, V, or the closing
    /// line of Z), not a sample of a curve.
    straight: Vec<bool>,
    /// Closed by `z`; the last point repeats the first.
    closed: bool,
}

impl Subpath {
    fn starting_at(p: [f32; 2]) -> Self {
        Self { pts: vec![p], joints: vec![0], straight: Vec::new(), closed: false }
    }

    /// A closed filled polygon: no joints and no straight flags, the points are the outline.
    fn polygon(pts: Vec<[f32; 2]>) -> Self {
        Self { pts, joints: Vec::new(), straight: Vec::new(), closed: true }
    }
}

/// A parsed icon, in grid coordinates. An outline icon holds its stroked sub-paths; a filled icon
/// holds one convex polygon per sub-path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SvgIcon {
    subpaths: Vec<Subpath>,
    /// The file is filled: `fill="currentColor"` on the root and no stroke (see `is_filled`).
    filled: bool,
}

/// Parse the stroked paths of a Tabler SVG. Paths with `stroke="none"` (the 24 × 24 background
/// square) are skipped. A filled file gives one convex polygon per sub-path instead.
pub fn parse_svg(svg: &str) -> Result<SvgIcon, SvgError> {
    if svg.len() > MAX_SVG_BYTES {
        return Err(SvgError::TooLarge { limit: MAX_SVG_BYTES });
    }
    let mut builder = Builder::default();
    let mut rest = svg;
    let mut paths = 0usize;
    while let Some(found) = rest.find("<path") {
        paths += 1;
        if paths > MAX_PATHS {
            return Err(SvgError::TooMany { what: "paths", limit: MAX_PATHS });
        }
        let at = svg.len() - rest.len() + found;
        let tail = rest.get(found..).unwrap_or_default();
        let Some(end) = tail.find('>') else {
            return Err(SvgError::Malformed { at });
        };
        let tag = tail.get(..end).unwrap_or_default();
        if !tag.contains("stroke=\"none\"") {
            let Some(d) = attribute(tag, "d") else {
                return Err(SvgError::Malformed { at });
            };
            builder.parse_d(d)?;
        }
        rest = tail.get(end + 1..).unwrap_or_default();
    }
    builder.finish()?;
    let filled = is_filled(svg);
    let mut subpaths = Vec::with_capacity(builder.done.len());
    for sub in builder.done {
        if !filled {
            subpaths.push(sub);
            continue;
        }
        let pts = filled_polygon(&sub.pts)?;
        if pts.len() >= 3 {
            subpaths.push(Subpath::polygon(pts));
        }
    }
    if subpaths.is_empty() {
        return Err(SvgError::NoPath);
    }
    Ok(SvgIcon { subpaths, filled })
}

/// The value of attribute `name="…"` in `tag`, where the name starts a word.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = tag.get(from..).and_then(|t| t.find(&needle)).map(|i| i + from) {
        let starts_word = tag.get(..i).and_then(|t| t.chars().next_back()).is_none_or(char::is_whitespace);
        let value_start = i + needle.len();
        if starts_word {
            let value = tag.get(value_start..)?;
            return value.get(..value.find('"')?);
        }
        from = value_start;
    }
    None
}

/// Filled files have `fill="currentColor"` on the root `<svg>` and no stroke there.
fn is_filled(svg: &str) -> bool {
    let root = svg.find("<svg").and_then(|at| svg.get(at..)).unwrap_or_default();
    let tag = root.find('>').and_then(|end| root.get(..end)).unwrap_or_default();
    attribute(tag, "fill") == Some("currentColor") && attribute(tag, "stroke").is_none()
}

/// The turn at `b` on the way from `a` to `c`, in radians; its sign says which side it bends to.
fn turn(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
    (u[0] * v[1] - u[1] * v[0]).atan2(u[0] * v[0] + u[1] * v[1])
}

/// Whether all `points` lie on one horizontal or one vertical line (within `FLAT_EPS`). A curve like
/// that is a straight line written as a curve, so its samples snap as a straight command's do.
fn level(points: &[[f32; 2]]) -> bool {
    let Some(first) = points.first() else {
        return false;
    };
    let same_y = points.iter().all(|p| (p[1] - first[1]).abs() < FLAT_EPS);
    let same_x = points.iter().all(|p| (p[0] - first[0]).abs() < FLAT_EPS);
    same_y || same_x
}

/// Two points within `FLAT_EPS` of each other in both coordinates are one point.
fn same_point(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() < FLAT_EPS && (a[1] - b[1]).abs() < FLAT_EPS
}

/// The polygon of a filled sub-path: the point that closes it is dropped, and repeated points are
/// merged. With fewer than three points there is no area and the result is short (the caller skips
/// it). Otherwise the polygon must be convex: its turns all go one way and add up to one full turn,
/// or the sub-path is [`SvgError::NotConvex`].
fn filled_polygon(raw: &[[f32; 2]]) -> Result<Vec<[f32; 2]>, SvgError> {
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(raw.len());
    for &p in raw {
        if pts.last().is_none_or(|q| !same_point(*q, p)) {
            pts.push(p);
        }
    }
    if pts.len() > 1 && pts.first().zip(pts.last()).is_some_and(|(a, b)| same_point(*a, *b)) {
        pts.pop();
    }
    let n = pts.len();
    if n < 3 {
        return Ok(pts);
    }
    let before = pts.iter().cycle().skip(n - 1);
    let after = pts.iter().cycle().skip(1);
    let turns: Vec<f32> = before.zip(pts.iter()).zip(after).take(n).map(|((a, b), c)| turn(*a, *b, *c)).collect();
    let total: f32 = turns.iter().sum();
    let sign = if total < 0.0 { -1.0 } else { 1.0 };
    let one_turn = (total.abs() - TAU).abs() < TURN_TOTAL_EPS;
    if !one_turn || turns.iter().any(|t| t * sign < -TURN_EPS) {
        return Err(SvgError::NotConvex);
    }
    Ok(pts)
}

/// Byte cursor over path data.
struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_separators(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r' | b',')) {
            self.pos += 1;
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    /// One number after any separators. Something that cannot start a number is a missing argument;
    /// a malformed number is a bad number.
    fn number(&mut self) -> Result<f32, SvgError> {
        self.skip_separators();
        let start = self.pos;
        if !matches!(self.peek(), Some(b'0'..=b'9' | b'+' | b'-' | b'.')) {
            return Err(SvgError::MissingArgument { at: start });
        }
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut digits = self.digits();
        if self.peek() == Some(b'.') {
            self.pos += 1;
            digits += self.digits();
        }
        if digits == 0 {
            return Err(SvgError::BadNumber { at: start });
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let exponent = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(SvgError::BadNumber { at: exponent });
            }
        }
        let text = self.bytes.get(start..self.pos).ok_or(SvgError::BadNumber { at: start })?;
        let text = std::str::from_utf8(text).map_err(|_| SvgError::BadNumber { at: start })?;
        let value: f32 = text.parse().map_err(|_| SvgError::BadNumber { at: start })?;
        if value.is_finite() && value.abs() <= MAX_COORD { Ok(value) } else { Err(SvgError::OutOfRange { at: start }) }
    }

    /// An arc flag: exactly one `0` or `1`, which may be followed directly by the next number.
    fn flag(&mut self) -> Result<bool, SvgError> {
        self.skip_separators();
        let at = self.pos;
        match self.peek() {
            None => Err(SvgError::MissingArgument { at }),
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(SvgError::BadFlag { at }),
        }
    }
}

/// Reads one coordinate and adds `offset` (the current point, for relative commands).
fn read_coord(sc: &mut Scanner<'_>, offset: f32) -> Result<f32, SvgError> {
    let at = sc.pos;
    let value = sc.number()? + offset;
    if value.is_finite() && value.abs() <= MAX_COORD { Ok(value) } else { Err(SvgError::OutOfRange { at }) }
}

fn read_point(sc: &mut Scanner<'_>, base: [f32; 2]) -> Result<[f32; 2], SvgError> {
    Ok([read_coord(sc, base[0])?, read_coord(sc, base[1])?])
}

/// Turns path data into sub-paths. One builder can read several paths in turn.
#[derive(Default)]
struct Builder {
    /// Finished sub-paths.
    done: Vec<Subpath>,
    /// The sub-path being built.
    cur: Option<Subpath>,
    /// The current point, and the start of the current sub-path (for `z`).
    pos: [f32; 2],
    start: [f32; 2],
    /// Control points of the last cubic (for `s`) and quadratic (for `t`), if the last command was one.
    cubic_ctrl: Option<[f32; 2]>,
    quad_ctrl: Option<[f32; 2]>,
    /// Points made so far, across all paths.
    points: usize,
}

impl Builder {
    fn parse_d(&mut self, d: &str) -> Result<(), SvgError> {
        if d.len() > MAX_D_BYTES {
            return Err(SvgError::TooLarge { limit: MAX_D_BYTES });
        }
        if d.trim().is_empty() {
            return Err(SvgError::EmptyData);
        }
        self.finish()?;
        self.pos = [0.0, 0.0];
        self.start = [0.0, 0.0];
        self.cubic_ctrl = None;
        self.quad_ctrl = None;
        let mut sc = Scanner { bytes: d.as_bytes(), pos: 0 };
        let mut cmd: Option<u8> = None;
        let mut commands = 0usize;
        loop {
            sc.skip_separators();
            let Some(c) = sc.peek() else { break };
            let at = sc.pos;
            if c.is_ascii_alphabetic() {
                sc.pos += 1;
                if !COMMANDS.contains(&c) {
                    return Err(SvgError::UnknownCommand { at, found: char::from(c) });
                }
                commands += 1;
                if commands > MAX_COMMANDS {
                    return Err(SvgError::TooMany { what: "commands", limit: MAX_COMMANDS });
                }
                if cmd.is_none() && !matches!(c, b'M' | b'm') {
                    return Err(SvgError::NoMoveTo { at });
                }
                cmd = Some(c);
                if matches!(c, b'Z' | b'z') {
                    self.close()?;
                } else {
                    self.apply(c, &mut sc)?;
                }
            } else if matches!(c, b'0'..=b'9' | b'+' | b'-' | b'.') {
                // A number where a command was expected repeats the last one (M becomes L).
                let Some(current) = cmd else {
                    return Err(SvgError::NoMoveTo { at });
                };
                if matches!(current, b'Z' | b'z') {
                    return Err(SvgError::UnexpectedChar { at, found: char::from(c) });
                }
                let implicit = match current {
                    b'M' => b'L',
                    b'm' => b'l',
                    other => other,
                };
                self.apply(implicit, &mut sc)?;
            } else {
                return Err(SvgError::UnexpectedChar { at, found: char::from(c) });
            }
        }
        self.finish()
    }

    /// Runs one command with its arguments (`c` is the letter, `sc` is after it).
    fn apply(&mut self, c: u8, sc: &mut Scanner<'_>) -> Result<(), SvgError> {
        let rel = c.is_ascii_lowercase();
        let base = if rel { self.pos } else { [0.0, 0.0] };
        let prev_cubic = self.cubic_ctrl.take();
        let prev_quad = self.quad_ctrl.take();
        match c.to_ascii_uppercase() {
            b'M' => {
                let p = read_point(sc, base)?;
                self.move_to(p)
            }
            b'L' => {
                let p = read_point(sc, base)?;
                self.line_to(p)
            }
            b'H' => {
                let x = read_coord(sc, base[0])?;
                let y = self.pos[1];
                self.line_to([x, y])
            }
            b'V' => {
                let y = read_coord(sc, base[1])?;
                let x = self.pos[0];
                self.line_to([x, y])
            }
            b'C' => {
                let c1 = read_point(sc, base)?;
                let c2 = read_point(sc, base)?;
                let end = read_point(sc, base)?;
                self.cubic_to(c1, c2, end)
            }
            b'S' => {
                let c2 = read_point(sc, base)?;
                let end = read_point(sc, base)?;
                let c1 = prev_cubic.map_or(self.pos, |p| reflect(p, self.pos));
                self.cubic_to(c1, c2, end)
            }
            b'Q' => {
                let ctrl = read_point(sc, base)?;
                let end = read_point(sc, base)?;
                self.quad_to(ctrl, end)
            }
            b'T' => {
                let end = read_point(sc, base)?;
                let ctrl = prev_quad.map_or(self.pos, |q| reflect(q, self.pos));
                self.quad_to(ctrl, end)
            }
            b'A' => {
                let rx = sc.number()?;
                let ry = sc.number()?;
                let angle = sc.number()?;
                let large = sc.flag()?;
                let sweep = sc.flag()?;
                let end = read_point(sc, base)?;
                self.arc_to(rx.abs(), ry.abs(), angle, large, sweep, end)
            }
            _ => Err(SvgError::UnknownCommand { at: sc.pos, found: char::from(c) }),
        }
    }

    fn count_point(&mut self) -> Result<(), SvgError> {
        self.points += 1;
        if self.points > MAX_POINTS {
            return Err(SvgError::TooMany { what: "points", limit: MAX_POINTS });
        }
        Ok(())
    }

    /// Adds one vertex to the current sub-path (starting one at the current point if needed).
    /// `straight` says whether the segment that ends here is a line command.
    fn push_point(&mut self, p: [f32; 2], straight: bool) -> Result<(), SvgError> {
        self.count_point()?;
        let from = self.pos;
        let sub = self.cur.get_or_insert_with(|| Subpath::starting_at(from));
        sub.straight.push(straight);
        sub.pts.push(p);
        self.pos = p;
        Ok(())
    }

    /// Marks the end of an SVG command: the last vertex becomes a joint.
    fn end_command(&mut self) {
        if let Some(sub) = self.cur.as_mut() {
            let last = sub.pts.len().saturating_sub(1);
            if sub.joints.last() != Some(&last) {
                sub.joints.push(last);
            }
        }
    }

    fn finish(&mut self) -> Result<(), SvgError> {
        if let Some(sub) = self.cur.take()
            && sub.pts.len() >= 2
        {
            if self.done.len() >= MAX_SUBPATHS {
                return Err(SvgError::TooMany { what: "sub-paths", limit: MAX_SUBPATHS });
            }
            self.done.push(sub);
        }
        Ok(())
    }

    fn move_to(&mut self, p: [f32; 2]) -> Result<(), SvgError> {
        self.finish()?;
        self.start = p;
        self.pos = p;
        self.count_point()?;
        self.cur = Some(Subpath::starting_at(p));
        Ok(())
    }

    fn line_to(&mut self, p: [f32; 2]) -> Result<(), SvgError> {
        self.push_point(p, true)?;
        self.end_command();
        Ok(())
    }

    fn close(&mut self) -> Result<(), SvgError> {
        if self.cur.is_none() {
            return Ok(());
        }
        if self.pos != self.start {
            let start = self.start;
            self.push_point(start, true)?;
            self.end_command();
        }
        if let Some(sub) = self.cur.as_mut() {
            sub.closed = true;
        }
        self.finish()?;
        self.pos = self.start;
        Ok(())
    }

    fn cubic_to(&mut self, c1: [f32; 2], c2: [f32; 2], end: [f32; 2]) -> Result<(), SvgError> {
        let p0 = self.pos;
        let d1 = norm([p0[0] - 2.0 * c1[0] + c2[0], p0[1] - 2.0 * c1[1] + c2[1]]);
        let d2 = norm([c1[0] - 2.0 * c2[0] + end[0], c1[1] - 2.0 * c2[1] + end[1]]);
        let n = steps((0.75 * d1.max(d2) / TOLERANCE).sqrt());
        let samples = sample_curve(p0, end, n, |t| bezier3(p0, c1, c2, end, t));
        let straight = level(&[p0, c1, c2, end]);
        for s in samples.into_iter().skip(1) {
            self.push_point(s, straight)?;
        }
        self.end_command();
        self.cubic_ctrl = Some(c2);
        Ok(())
    }

    fn quad_to(&mut self, ctrl: [f32; 2], end: [f32; 2]) -> Result<(), SvgError> {
        let p0 = self.pos;
        let d = norm([p0[0] - 2.0 * ctrl[0] + end[0], p0[1] - 2.0 * ctrl[1] + end[1]]);
        let n = steps((0.25 * d / TOLERANCE).sqrt());
        let samples = sample_curve(p0, end, n, |t| bezier2(p0, ctrl, end, t));
        let straight = level(&[p0, ctrl, end]);
        for s in samples.into_iter().skip(1) {
            self.push_point(s, straight)?;
        }
        self.end_command();
        self.quad_ctrl = Some(ctrl);
        Ok(())
    }

    /// An elliptical arc, converted to a centre parameterisation (SVG 1.1, F.6.5–F.6.6).
    fn arc_to(&mut self, rx: f32, ry: f32, angle_deg: f32, large: bool, sweep: bool, end: [f32; 2]) -> Result<(), SvgError> {
        let p0 = self.pos;
        if p0 == end {
            return Ok(());
        }
        if rx < MIN_RADIUS || ry < MIN_RADIUS {
            return self.line_to(end);
        }
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let half_dx = (p0[0] - end[0]) / 2.0;
        let half_dy = (p0[1] - end[1]) / 2.0;
        let x1 = cos * half_dx + sin * half_dy;
        let y1 = -sin * half_dx + cos * half_dy;
        let (mut rx, mut ry) = (rx, ry);
        let lambda = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
        if lambda > 1.0 {
            let grow = lambda.sqrt();
            rx *= grow;
            ry *= grow;
        }
        let (rx2, ry2) = (rx * rx, ry * ry);
        let num = rx2 * ry2 - rx2 * y1 * y1 - ry2 * x1 * x1;
        let den = rx2 * y1 * y1 + ry2 * x1 * x1;
        let mut root = if den > 0.0 { (num / den).max(0.0).sqrt() } else { 0.0 };
        if large == sweep {
            root = -root;
        }
        let cxp = root * rx * y1 / ry;
        let cyp = -root * ry * x1 / rx;
        let cx = cos * cxp - sin * cyp + (p0[0] + end[0]) / 2.0;
        let cy = sin * cxp + cos * cyp + (p0[1] + end[1]) / 2.0;
        let u = [(x1 - cxp) / rx, (y1 - cyp) / ry];
        let v = [(-x1 - cxp) / rx, (-y1 - cyp) / ry];
        let theta1 = angle_between([1.0, 0.0], u);
        let mut dtheta = angle_between(u, v);
        if !sweep && dtheta > 0.0 {
            dtheta -= TAU;
        } else if sweep && dtheta < 0.0 {
            dtheta += TAU;
        }
        if ![cx, cy, theta1, dtheta].iter().all(|v| v.is_finite()) {
            return Err(SvgError::BadArc);
        }
        let r = rx.max(ry);
        let step = (2.0 * (1.0 - TOLERANCE / r).clamp(-1.0, 1.0).acos()).max(MIN_ANGLE_STEP);
        let n = steps(dtheta.abs() / step);
        let samples = sample_curve(p0, end, n, |t| {
            let a = theta1 + dtheta * t;
            [cx + rx * cos * a.cos() - ry * sin * a.sin(), cy + rx * sin * a.cos() + ry * cos * a.sin()]
        });
        for s in samples.into_iter().skip(1) {
            self.push_point(s, false)?;
        }
        self.end_command();
        Ok(())
    }
}

fn reflect(p: [f32; 2], about: [f32; 2]) -> [f32; 2] {
    [2.0 * about[0] - p[0], 2.0 * about[1] - p[1]]
}

fn norm(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

/// Signed angle from `u` to `v`, in radians; zero if either vector is empty.
fn angle_between(u: [f32; 2], v: [f32; 2]) -> f32 {
    let len = norm(u) * norm(v);
    if !len.is_finite() || len <= 0.0 {
        return 0.0;
    }
    let dot = u[0] * v[0] + u[1] * v[1];
    let angle = (dot / len).clamp(-1.0, 1.0).acos();
    if u[0] * v[1] - u[1] * v[0] < 0.0 { -angle } else { angle }
}

/// Segments for a curve with the given Wang bound (`x` is the bound's square root). Never zero,
/// never more than `MAX_STEPS`.
fn steps(x: f32) -> usize {
    if x.is_finite() { x.ceil().clamp(1.0, MAX_STEPS as f32) as usize } else { 1 }
}

fn bezier3(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]]
}

fn bezier2(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (a, b, c) = (u * u, 2.0 * u * t, t * t);
    [a * p0[0] + b * p1[0] + c * p2[0], a * p0[1] + b * p1[1] + c * p2[1]]
}

/// Points of a curve from `start` to `end`, `n` steps, including both ends. `at(t)` gives the
/// interior points. Adjacent points at the same height or the same x would read as a straight H or
/// V line, so the curve is sampled again with one more step (at most twice more). Symmetric arcs
/// with an odd step count are the usual case.
fn sample_curve(start: [f32; 2], end: [f32; 2], n: usize, at: impl Fn(f32) -> [f32; 2]) -> Vec<[f32; 2]> {
    let mut n = n;
    let mut out = build_samples(start, end, n, &at);
    for _ in 0..2 {
        if !has_flat_pair(&out) {
            break;
        }
        n += 1;
        out = build_samples(start, end, n, &at);
    }
    out
}

fn build_samples(start: [f32; 2], end: [f32; 2], n: usize, at: &impl Fn(f32) -> [f32; 2]) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(n + 1);
    out.push(start);
    for k in 1..n {
        out.push(at(k as f32 / n as f32));
    }
    out.push(end);
    out
}

fn has_flat_pair(points: &[[f32; 2]]) -> bool {
    points.windows(2).any(|w| match w {
        [[ax, ay], [bx, by]] => (ax - bx).abs() < FLAT_EPS || (ay - by).abs() < FLAT_EPS,
        _ => false,
    })
}

/// Maps grid units to points: the grid is centred in the rect, `scale` points per unit, and
/// pixels are `ppp` per point. `odd` says whether the stroke is an odd number of pixels wide.
struct Mapping {
    centre: Pos2,
    scale: f32,
    ppp: f32,
    odd: bool,
}

impl Mapping {
    fn to_screen(&self, p: [f32; 2]) -> Pos2 {
        pos2(self.centre.x + (p[0] - HALF_GRID) * self.scale, self.centre.y + (p[1] - HALF_GRID) * self.scale)
    }

    /// The snap rule: a coordinate goes to a whole pixel, the middle of a pixel when a
    /// stroke that is odd in pixels crosses it (`mid`), otherwise a pixel edge.
    fn snap(&self, v: f32, centre: f32, mid: bool) -> f32 {
        let half = if mid && self.odd { 0.5 } else { 0.0 };
        let base = (centre * self.ppp).round() + half;
        (base + ((v - centre) * self.ppp).round()) / self.ppp
    }

    fn snap_pos(&self, q: Pos2, mid_x: bool, mid_y: bool) -> Pos2 {
        pos2(self.snap(q.x, self.centre.x, mid_x), self.snap(q.y, self.centre.y, mid_y))
    }
}

/// Paint `icon` centred in `rect`, the 24-unit grid scaled to the smaller side.
pub fn paint_svg(painter: &Painter, rect: Rect, icon: &SvgIcon, color: Color32) {
    let ppp = painter.pixels_per_point();
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let scale = rect.width().min(rect.height()) / GRID;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    if icon.filled {
        // Filled polygons keep their exact positions, as curves do: their edges are not snapped.
        let map = Mapping { centre: rect.center(), scale, ppp, odd: false };
        for sub in &icon.subpaths {
            let pts: Vec<Pos2> = sub.pts.iter().map(|p| map.to_screen(*p)).collect();
            painter.add(PathShape::convex_polygon(pts, color, PathStroke::NONE));
        }
        return;
    }
    let width_px = (STROKE_UNITS * scale * ppp - 0.5).ceil().max(1.0);
    let map = Mapping { centre: rect.center(), scale, ppp, odd: (width_px * 0.5).fract() != 0.0 };
    for sub in &icon.subpaths {
        paint_subpath(painter, sub, &map, width_px / ppp, color);
    }
}

fn mark(flags: &mut [bool], i: usize) {
    if let Some(flag) = flags.get_mut(i) {
        *flag = true;
    }
}

/// On a closed sub-path the first and last vertex are one place, so they share their flags.
fn join_ends(flags: &mut [bool]) {
    let both = flags.first().copied().unwrap_or(false) || flags.last().copied().unwrap_or(false);
    if let Some(first) = flags.first_mut() {
        *first = both;
    }
    if let Some(last) = flags.last_mut() {
        *last = both;
    }
}

fn paint_subpath(painter: &Painter, sub: &Subpath, map: &Mapping, width: f32, color: Color32) {
    let n = sub.pts.len();
    if n < 2 {
        return;
    }
    // A segment that is an exact line command and runs level (or plumb) marks its ends for snapping.
    // Curve samples are never marked (they keep their exact positions), unless the curve is level or
    // plumb: then it is a straight line (see `level`).
    let mut vert = vec![false; n];
    let mut horiz = vec![false; n];
    for (i, straight) in sub.straight.iter().enumerate() {
        if !*straight {
            continue;
        }
        let (Some(a), Some(b)) = (sub.pts.get(i), sub.pts.get(i + 1)) else {
            continue;
        };
        if (a[1] - b[1]).abs() < FLAT_EPS {
            mark(&mut horiz, i);
            mark(&mut horiz, i + 1);
        }
        if (a[0] - b[0]).abs() < FLAT_EPS {
            mark(&mut vert, i);
            mark(&mut vert, i + 1);
        }
    }
    if sub.closed {
        join_ends(&mut vert);
        join_ends(&mut horiz);
    }
    let pts: Vec<Pos2> = sub
        .pts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let q = map.to_screen(*p);
            let v = vert.get(i).copied().unwrap_or(false);
            let h = horiz.get(i).copied().unwrap_or(false);
            if v || h { map.snap_pos(q, v, h) } else { q }
        })
        .collect();
    // One polyline per command, so no join is mitred. Duplicate points (a zero-length segment, or
    // two points that snapped to one pixel) are dropped: egui cannot normalise a zero-length segment.
    for w in sub.joints.windows(2) {
        let [a, b] = w else {
            continue;
        };
        let Some(run) = pts.get(*a..=*b) else {
            continue;
        };
        let mut run = run.to_vec();
        run.dedup();
        if run.len() >= 2 {
            painter.add(PathShape::line(run, PathStroke::new(width, color)));
        }
    }
    let radius = width / 2.0;
    for j in &sub.joints {
        if let Some(p) = pts.get(*j) {
            painter.circle_filled(*p, radius, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::Shape;
    use egui::{RawInput, vec2};

    /// The table's file names and texts.
    fn bundled() -> impl Iterator<Item = (&'static str, &'static str)> {
        TABLE.iter().map(|(_, name, svg)| (*name, *svg))
    }

    /// The sub-paths of one `d` attribute, parsed on their own.
    fn parse_d_alone(d: &str) -> Result<Vec<Subpath>, SvgError> {
        let mut b = Builder::default();
        b.parse_d(d)?;
        b.finish()?;
        Ok(b.done)
    }

    fn single(d: &str) -> Subpath {
        let mut subs = parse_d_alone(d).unwrap_or_else(|e| panic!("{d}: {e}"));
        assert_eq!(subs.len(), 1, "{d}");
        subs.remove(0)
    }

    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    /// Paints `icon` into a `size`-point rect at `ppp` and returns the shapes of the second frame
    /// (the first frame sets the context up).
    fn shapes_for(icon: &SvgIcon, ppp: f32, size: f32) -> Vec<Shape> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(RawInput::default(), |ui| {
                paint_svg(ui.painter(), Rect::from_min_size(pos2(10.3, 7.7), vec2(size, size)), icon, Color32::WHITE);
            });
            out.textures_delta.clear();
            shapes = out.shapes.into_iter().map(|c| c.shape).collect();
        }
        shapes
    }

    #[test]
    fn valid_path_gives_its_vertices() {
        let s = single("M7 4v16l13 -8l-13 -8");
        assert_eq!(s.pts, vec![[7.0, 4.0], [7.0, 20.0], [20.0, 12.0], [7.0, 4.0]]);
        assert_eq!(s.joints, vec![0, 1, 2, 3]);
        assert_eq!(s.straight, vec![true, true, true]);
        assert!(!s.closed);
    }

    #[test]
    fn empty_or_blank_data_is_an_error() {
        assert_eq!(parse_d_alone(""), Err(SvgError::EmptyData));
        assert_eq!(parse_d_alone(" \n\t"), Err(SvgError::EmptyData));
    }

    #[test]
    fn invalid_data_is_an_error() {
        assert!(matches!(parse_d_alone("L1 1"), Err(SvgError::NoMoveTo { .. })));
        assert!(matches!(parse_d_alone("M0 0 X1 1"), Err(SvgError::UnknownCommand { found: 'X', .. })));
        assert!(matches!(parse_d_alone("M0 0 1 1 ?"), Err(SvgError::UnexpectedChar { .. })));
        assert!(matches!(parse_d_alone("M1 1 z 2 2"), Err(SvgError::UnexpectedChar { .. })));
        assert!(matches!(parse_d_alone("M0 0 L- 1"), Err(SvgError::BadNumber { .. })));
        assert!(matches!(parse_d_alone("M0 0 a1 1 0 2 0 0"), Err(SvgError::BadFlag { .. })));
    }

    #[test]
    fn missing_numbers_are_an_error() {
        assert!(matches!(parse_d_alone("M7"), Err(SvgError::MissingArgument { .. })));
        assert!(matches!(parse_d_alone("M0 0 L1"), Err(SvgError::MissingArgument { .. })));
        assert!(matches!(parse_d_alone("M0 0 Q1 1 2"), Err(SvgError::MissingArgument { .. })));
        assert!(matches!(parse_d_alone("M0 0 C1 1 2 2 3"), Err(SvgError::MissingArgument { .. })));
        assert!(matches!(parse_d_alone("M0 0 A1 1 0 0"), Err(SvgError::MissingArgument { .. })));
    }

    #[test]
    fn unknown_commands_are_an_error() {
        assert!(matches!(parse_d_alone("M0 0 k1 1"), Err(SvgError::UnknownCommand { found: 'k', .. })));
        assert!(matches!(parse_d_alone("M0 0 N1 1"), Err(SvgError::UnknownCommand { found: 'N', .. })));
    }

    #[test]
    fn nan_and_infinity_are_refused() {
        for d in ["M NaN 2", "M0 0 L inf 2", "M0 0 L Infinity 0", "M1e999 2", "M0 0 L-1e999 0", "M0 0 L1e38 0"] {
            assert!(parse_d_alone(d).is_err(), "{d} must not parse");
        }
        assert!(matches!(parse_d_alone("M1e999 2"), Err(SvgError::OutOfRange { .. })));
    }

    #[test]
    fn truncated_data_never_panics() {
        let full = "M0 0C1 1 2 2 3 3";
        for end in 0..=full.len() {
            let prefix = full.get(..end).unwrap_or_default();
            assert!(std::panic::catch_unwind(|| parse_d_alone(prefix).is_ok()).is_ok(), "{prefix}");
        }
        assert!(parse_d_alone("M0 0C1 1 2 2 3").is_err());
    }

    #[test]
    fn a_megabyte_is_refused_before_parsing() {
        let big = "M0 0 ".repeat(220_000);
        assert!(big.len() >= 1 << 20);
        assert_eq!(parse_d_alone(&big).err(), Some(SvgError::TooLarge { limit: MAX_D_BYTES }));
        let svg = format!("<svg><path d=\"{big}\"/></svg>");
        assert_eq!(parse_svg(&svg).err(), Some(SvgError::TooLarge { limit: MAX_SVG_BYTES }));
    }

    #[test]
    fn too_many_points_are_refused() {
        let one_path = format!("<path d=\"M0 0{}\"/>", " 1 1".repeat(3000));
        let svg = one_path.repeat(3);
        assert!(matches!(parse_svg(&svg), Err(SvgError::TooMany { what: "points", .. })));
    }

    #[test]
    fn number_forms_follow_the_svg_grammar() {
        let s = single("M.5-.5L1e2 1E-1");
        assert!(near(s.pts[0], [0.5, -0.5]));
        assert!(near(s.pts[1], [100.0, 0.1]));
        assert_eq!(single("M0.5.5L1,2 3,4").pts, vec![[0.5, 0.5], [1.0, 2.0], [3.0, 4.0]]);
        assert_eq!(single("M10-5L0 0").pts, vec![[10.0, -5.0], [0.0, 0.0]]);
        assert_eq!(single("M1 1h-.5v+2e1").pts, vec![[1.0, 1.0], [0.5, 1.0], [0.5, 21.0]]);
    }

    #[test]
    fn implicit_commands_repeat() {
        assert_eq!(single("M0 0 1 1 2 2").pts, vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]);
        assert_eq!(single("m1 1 2 2").pts, vec![[1.0, 1.0], [3.0, 3.0]]);
    }

    #[test]
    fn relative_and_smooth_commands() {
        let s = single("m1 1 l2 2 h-1 v3 z");
        assert_eq!(s.pts, vec![[1.0, 1.0], [3.0, 3.0], [2.0, 3.0], [2.0, 6.0], [1.0, 1.0]]);
        assert!(s.closed);
        assert_eq!(single("M0 0 H5 V5").pts, vec![[0.0, 0.0], [5.0, 0.0], [5.0, 5.0]]);
        // S reflects the previous second control point (2, 2) about (2, 0), giving (2, -2).
        // The bulge then reaches y = -1.5; without the reflection it would stop near -0.89.
        let s = single("M0 0 C0 2 2 2 2 0 S4 -2 4 0");
        assert!(near(*s.pts.last().unwrap_or(&[0.0, 0.0]), [4.0, 0.0]));
        assert!(s.pts.iter().any(|p| p[1] < -1.2));
        let s = single("M0 0 Q1 1 2 0 T4 0");
        assert!(near(*s.pts.last().unwrap_or(&[0.0, 0.0]), [4.0, 0.0]));
    }

    #[test]
    fn arcs_stay_within_tolerance_of_the_circle() {
        // A semicircle of radius 1 centred on (1, 0).
        let s = single("M0 0a1 1 0 0 1 2 0");
        assert!(near(s.pts[0], [0.0, 0.0]));
        assert!(near(*s.pts.last().unwrap_or(&[0.0, 0.0]), [2.0, 0.0]));
        for p in &s.pts {
            assert!((norm([p[0] - 1.0, p[1]]) - 1.0).abs() < 1e-3, "{p:?}");
        }
        for w in s.pts.windows(2) {
            let mid = [(w[0][0] + w[1][0]) / 2.0, (w[0][1] + w[1][1]) / 2.0];
            let r = norm([mid[0] - 1.0, mid[1]]);
            assert!((1.0 - TOLERANCE - 1e-3..=1.0 + 1e-3).contains(&r), "chord midpoint at {r}");
        }
    }

    #[test]
    fn compact_arc_flags_parse() {
        // "1120" is the flags 1 and 1, then x = 20. The radii grow to 10, so this is a semicircle.
        let s = single("M0 0a2 2 0 1120 0");
        assert!(near(*s.pts.last().unwrap_or(&[0.0, 0.0]), [20.0, 0.0]));
        for p in &s.pts {
            assert!((norm([p[0] - 10.0, p[1]]) - 10.0).abs() < 1e-2, "{p:?}");
        }
    }

    #[test]
    fn bundled_icons_parse_inside_the_grid() {
        for (name, svg) in bundled() {
            let icon = parse_svg(svg).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!icon.subpaths.is_empty(), "{name}");
            for sub in &icon.subpaths {
                for p in &sub.pts {
                    assert!((-0.01..=24.01).contains(&p[0]) && (-0.01..=24.01).contains(&p[1]), "{name}: {p:?}");
                }
            }
        }
        assert_eq!(svg_for(Icon::Play).map(|i| i.subpaths.len()), Some(1));
        assert_eq!(svg_for(Icon::Pause).map(|i| i.subpaths.len()), Some(2));
    }

    #[test]
    fn background_paths_are_skipped_and_empty_documents_refused() {
        let svg = r#"<svg><path stroke="none" d="M0 0h24v24H0z" fill="none" /><path d="M1 1h2" /></svg>"#;
        assert_eq!(parse_svg(svg).map(|i| i.subpaths.len()), Ok(1));
        assert_eq!(parse_svg(r#"<svg><path stroke="none" d="M0 0h24v24H0z"/></svg>"#), Err(SvgError::NoPath));
        assert!(matches!(parse_svg(r#"<svg><path stroke="x"/></svg>"#), Err(SvgError::Malformed { .. })));
        assert!(matches!(parse_svg("<svg><path d=\"M1 1h2\""), Err(SvgError::Malformed { .. })));
    }

    #[test]
    fn hostile_svg_never_panics() {
        for (name, svg) in bundled() {
            for end in 0..=svg.len() {
                let prefix = svg.get(..end).unwrap_or_default();
                assert!(std::panic::catch_unwind(|| parse_svg(prefix).is_ok()).is_ok(), "{name}: cut at {end}");
            }
            let bytes = svg.as_bytes();
            for i in 0..bytes.len() {
                for bit in [0u8, 2, 5, 7] {
                    let mut flipped = bytes.to_vec();
                    if let Some(b) = flipped.get_mut(i) {
                        *b ^= 1 << bit;
                    }
                    let text = String::from_utf8_lossy(&flipped).into_owned();
                    assert!(std::panic::catch_unwind(|| parse_svg(&text).is_ok()).is_ok(), "{name}: bit {bit} at {i}");
                }
            }
        }
    }

    #[test]
    fn random_path_text_never_panics() {
        const ALPHABET: &[u8] = b"MmLlHhVvCcSsQqTtAaZz0123456789.,-+eE x";
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        for _ in 0..4000 {
            let mut text = String::from("M0 0");
            for _ in 0..next() % 40 {
                let idx = usize::try_from(next()).unwrap_or(0) % ALPHABET.len();
                if let Some(&b) = ALPHABET.get(idx) {
                    text.push(char::from(b));
                }
            }
            assert!(std::panic::catch_unwind(|| parse_d_alone(&text).is_ok()).is_ok(), "{text:?}");
        }
    }

    #[test]
    fn svg_icons_draw_inside_the_rect() {
        for (which, name, _) in TABLE {
            let Some(icon) = svg_for(*which) else { panic!("{name}: no svg") };
            for ppp in [1.0, 2.0] {
                let shapes = shapes_for(icon, ppp, 16.0);
                let drawn = shapes.iter().filter(|s| matches!(s, Shape::Path(_))).count();
                assert!(drawn > 0, "{name} at {ppp}x drew nothing");
                for s in &shapes {
                    if let Shape::Path(p) = s {
                        for q in &p.points {
                            assert!(q.x > 9.3 && q.x < 27.3 && q.y > 6.7 && q.y < 24.7, "{name} at {ppp}x: {q:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn stroke_is_one_pixel_at_1x_and_three_at_2x() {
        let Some(lock) = svg_for(Icon::Lock) else { panic!("lock has no svg") };
        for (ppp, want) in [(1.0, 1.0), (2.0, 3.0)] {
            for s in shapes_for(lock, ppp, 16.0) {
                if let Shape::Path(p) = s {
                    assert!((p.stroke.width * ppp - want).abs() < 1e-3, "{ppp}x: {} px", p.stroke.width * ppp);
                }
            }
        }
    }

    #[test]
    fn every_command_boundary_gets_a_disc() {
        // M7 4 v16 l13 -8 l-13 -8: three strokes, and four joints (the start, two corners and the
        // end). Each joint gets a disc.
        let Ok(icon) = parse_svg(r#"<svg fill="none" stroke="currentColor"><path d="M7 4v16l13 -8l-13 -8" /></svg>"#) else { panic!("the test path parses") };
        let shapes = shapes_for(&icon, 2.0, 16.0);
        let strokes = shapes.iter().filter(|s| matches!(s, Shape::Path(_))).count();
        let discs = shapes.iter().filter(|s| matches!(s, Shape::Circle(_))).count();
        assert_eq!((strokes, discs), (3, 4));
    }

    #[test]
    fn filled_icons_are_convex_and_draw_pixels() {
        for (icon, polygons) in [(Icon::Play, 1), (Icon::Pause, 2)] {
            let Some(svg) = svg_for(icon) else { panic!("{icon:?} has no svg") };
            assert!(svg.filled, "{icon:?} is a filled file");
            assert_eq!(svg.subpaths.len(), polygons, "{icon:?}");
            for sub in &svg.subpaths {
                assert!(sub.closed && sub.pts.len() >= 3, "{icon:?}");
                assert!(filled_polygon(&sub.pts).is_ok(), "{icon:?} is convex");
            }
            let paths: Vec<egui::epaint::PathShape> =
                shapes_for(svg, 2.0, 16.0).into_iter().filter_map(|s| if let Shape::Path(p) = s { Some(p) } else { None }).collect();
            assert_eq!(paths.len(), polygons, "{icon:?}");
            for p in &paths {
                assert!(p.closed && p.fill.a() > 0 && p.points.len() >= 3, "{icon:?}");
                assert!(area(&p.points) > 1.0, "{icon:?}: {} pt²", area(&p.points));
            }
        }
    }

    /// Shoelace area of a polygon, in points².
    fn area(points: &[Pos2]) -> f32 {
        let n = points.len();
        let twice: f32 = points.iter().zip(points.iter().cycle().skip(1)).take(n).map(|(a, b)| a.x * b.y - b.x * a.y).sum();
        twice.abs() / 2.0
    }

    #[test]
    fn filled_sub_paths_must_be_convex() {
        let fill = |d: &str| parse_svg(&format!("<svg fill=\"currentColor\"><path d=\"{d}\"/></svg>"));
        assert!(fill("M2 2h20v20h-20z").is_ok(), "a square is convex");
        assert!(fill("M2 2 L22 2 L12 20 Z").is_ok(), "a triangle is convex");
        // A dart: the corner at (12, 6) bends inwards.
        assert_eq!(fill("M2 2h20v10l-10 -6l-10 6z").err(), Some(SvgError::NotConvex));
        // A pentagram: every turn goes the same way, but they add up to two full turns.
        assert_eq!(fill("M12 2L19 20L4 8L20 8L5 20Z").err(), Some(SvgError::NotConvex));
        // A bow tie: the turns change direction.
        assert_eq!(fill("M2 2L22 22L22 2L2 22Z").err(), Some(SvgError::NotConvex));
    }

    #[test]
    fn fill_mode_comes_from_the_root() {
        let square = r#"<path d="M2 2h20v20h-20z"/>"#;
        let filled = parse_svg(&format!(r#"<svg fill="currentColor">{square}</svg>"#));
        let stroked = parse_svg(&format!(r#"<svg fill="none" stroke="currentColor">{square}</svg>"#));
        assert_eq!(filled.map(|i| i.filled), Ok(true));
        assert_eq!(stroked.map(|i| i.filled), Ok(false));
    }

    #[test]
    fn table_has_one_entry_per_icon() {
        for (which, name, _) in TABLE {
            assert_eq!(TABLE.iter().filter(|(other, _, _)| other == which).count(), 1, "{name}");
        }
    }
}
