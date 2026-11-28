//! The Prompter overlay on the Program monitor (`view.prompter`, off by default): a card at the
//! bottom of the picture with the section at the playhead (a range marker) under its marker's
//! flag, its elapsed time and length over a bar split at the talking points, the current talking
//! point (or the script's reading line) as the most prominent line, the points around it with
//! their durations, the next section near the end, the caption at the playhead as a caption block,
//! and warnings when the talking points need more time than the section has.
//!
//! What it shows comes from [`filmcraft_engine::prompter::prompt_at`] at the prompt time
//! ([`filmcraft_engine::prompter::prompt_time_at`]: the playhead, or where Next and Previous moved
//! the prompter); this module only draws it. When the prompt time is not the playhead, the header
//! shows the gap in a readout badge before the time, "ahead 0:08" or "behind 0:12". When the
//! prompter runs at another rate than the playhead (Faster and Slower, #5263) a second badge
//! before it says "rate 105%". While a voice-over take records with Follow Speech on (#5266), a
//! badge with a microphone before them says what it is doing in words ("following", "lost",
//! "listening", …), and a footer row above the caption shows the words heard: the last committed
//! ones, then the tentative ones in italics and dimmed. The reading line follows the narrator because
//! the engine holds the prompt time at the tracked place. A second accessibility node
//! (`program.prompter.follow`, a polite live region) carries only that state, so a change to "lost"
//! and back is announced and the words are not.
//! It is drawn with the monitor's painter, so it never reaches a render or an export, and it is
//! still drawn in full-screen Program. Automation id `program.prompter`; its label and its
//! accessibility node carry the text it shows.
//!
//! **Layout.** The card is centred at the bottom of the picture, inset from its edges by the
//! action-safe band, at most [`MAX_WIDTH_EMS`] wide and [`MAX_HEIGHT`] of the picture tall. Its
//! type and spacing scale with the picture ([`Metrics`]); when the picture is too small for
//! everything, [`plan`] keeps what the narrator needs most and drops the rest.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId, Galley, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::follow::{CardState, FollowState};
use filmcraft_engine::prompter::{Direction, NORMAL_RATE_PERCENT, Point, PointState, Prompt, Script, Section, direction, prompt_at, prompt_time_at, rate};
use filmcraft_project::{MarkerId, Sequence};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::panels::monitor::Which;
use crate::state::MonitorView;
use crate::theme::Tokens;
use crate::widgets::{MarkerPart, marker_glyph, paint_tabular, tabular_text};

/// The card's largest share of the picture's height, so most of the picture stays clear.
const MAX_HEIGHT: f32 = 0.45;
/// The card's inset from the picture's edges, as a share of the picture's shorter side: the
/// action-safe band the monitor's Safe Margins draw.
const INSET: f32 = 0.05;
/// The card's widest measure, in base type sizes: wider lines are hard to take in at a glance.
const MAX_WIDTH_EMS: f32 = 60.0;
/// Upcoming talking points listed at most.
const UPCOMING_ROWS: usize = 3;
/// Rows the current talking point wraps to before it is cut with an ellipsis.
const CURRENT_ROWS: usize = 2;
/// Script lines shown at most; with three the reading line is the second, else the first.
const SCRIPT_ROWS: usize = 3;
/// The narrowest the current point's text may be beside the script, in its own type size: two or
/// three words a row. The points column widens to give it that.
const CURRENT_MIN_EMS: f32 = 10.0;
/// The narrowest the script may be beside the points, in its type size; narrower, it goes under
/// the points instead.
const SCRIPT_MIN_EMS: f32 = 10.0;
/// The least contrast of the marker accent (never behind text) against the card and the bars' groove.
const ACCENT_CONTRAST: f64 = 3.0;
/// The least contrast of text against what it is drawn on.
const TEXT_CONTRAST: f64 = 4.5;
/// The least contrast of the current point's highlight against the card, so it shows as a surface
/// (it is never the only sign of the current point).
const SURFACE_CONTRAST: f64 = 1.2;

/// The overlay's colours: theme tokens, and the Timeline's caption block colours for the caption.
/// The section's marker colour comes on top of these through [`accent`].
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The card (`panel_bg`, opaque: no picture, burned-in captions included, shows through its text).
    pub card: Color32,
    /// The card's edge and the duration badges' outline (`field_border`, as a window's edge).
    pub edge: Color32,
    pub text: Color32,
    /// The section's time, finished and upcoming points, script already read and badges (`text_dim`).
    pub dim: Color32,
    /// Behind the current point and the script's reading line: `row_selected`, or the first of
    /// `pill_active_bg` and `hover` that stands apart from the card and on which `text_dim` still
    /// reads (in Dark the selection grey is the card's own, and in Light the blue selection
    /// gives `text_dim` less than 4.5:1).
    pub current: Color32,
    /// The bars' groove (`control_well`, as the app's slider tracks).
    pub track: Color32,
    /// Behind a warning (`pill_active_bg`).
    pub callout: Color32,
    /// The caption line: the Timeline's caption block fill, with white text as on the Timeline.
    pub caption: Color32,
    pub caption_text: Color32,
}

/// WCAG contrast ratio of two opaque colours.
fn contrast(a: Color32, b: Color32) -> f64 {
    craft_theme::contrast_ratio(a.into(), b.into())
}

pub fn palette(t: &Tokens) -> Palette {
    let fits = |bg: Color32| contrast(t.text, bg) >= TEXT_CONTRAST && contrast(t.text_dim, bg) >= TEXT_CONTRAST && contrast(bg, t.panel_bg) >= SURFACE_CONTRAST;
    let current = [t.row_selected, t.pill_active_bg, t.hover].into_iter().find(|c| fits(*c)).unwrap_or(t.row_selected);
    Palette {
        card: t.panel_bg,
        edge: t.field_border,
        text: t.text,
        dim: t.text_dim,
        current,
        track: t.control_well,
        callout: t.pill_active_bg,
        caption: crate::panels::timeline_captions::FILL,
        caption_text: Color32::WHITE,
    }
}

/// A marker's colour as the overlay's accent (its flag and the bars' fill). A marker can have any
/// colour, so its lightness is moved, hue and chroma kept, just enough for 3:1 against the card
/// and the groove (`craft_theme::ensure_contrast`). No text is ever drawn on it, and it never
/// says anything alone: the section's name is beside the flag, the bar's length is the progress.
pub fn accent(marker: Color32, c: &Palette) -> Color32 {
    let a = craft_theme::ensure_contrast(marker.into(), c.card.into(), ACCENT_CONTRAST);
    let a = craft_theme::ensure_contrast(a, c.track.into(), ACCENT_CONTRAST);
    let [r, g, b, _] = a.0;
    Color32::from_rgb(r, g, b)
}

/// The colour a marker is drawn in on the rulers: its custom colour, else its label's.
fn marker_color(app: &FilmcraftApp, seq: &Sequence, id: MarkerId) -> Option<Color32> {
    seq.markers.iter().find(|m| m.id == id).map(|m| {
        let [r, g, b] = m.custom_color.map_or_else(|| app.session.prefs.labels.rgb(m.color), |c| c.0);
        Color32::from_rgb(r, g, b)
    })
}

/// `m:ss`, or `h:mm:ss` from an hour on, in whole seconds rounded down (time elapsed).
fn clock(t: Tick) -> String {
    hms(t.0.max(0) / TICKS_PER_SECOND)
}

/// [`clock`] rounded up, for lengths and countdowns: a section a few frames short of 8 s reads
/// 0:08, and a countdown reaches 0:00 only when it is over.
fn clock_up(t: Tick) -> String {
    let t = t.0.max(0);
    hms(t / TICKS_PER_SECOND + i64::from(t % TICKS_PER_SECOND != 0))
}

fn hms(s: i64) -> String {
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The ahead/behind readout for a gap (prompt time minus playhead), rounded up to whole seconds so
/// any gap shows; None when the prompter is in step.
pub fn readout(gap: Tick) -> Option<String> {
    let time = clock_up(Tick(gap.0.saturating_abs()));
    direction(gap).map(|d| match d {
        Direction::Ahead => tlf!("ahead {time}", time = time),
        Direction::Behind => tlf!("behind {time}", time = time),
    })
}

/// "rate 105%" when the prompter does not run at the playhead's speed; None at the normal rate.
pub fn rate_text(percent: u32) -> Option<String> {
    (percent != NORMAL_RATE_PERCENT).then(|| tlf!("rate {percent}%", percent = percent))
}

/// The glyph beside the ahead/behind readout: a chevron pointing the way the prompter is from the
/// picture (forward when ahead). It only repeats what the words say.
fn readout_icon(gap: Tick) -> Option<Icon> {
    direction(gap).map(|d| match d {
        Direction::Ahead => Icon::ChevronRight,
        Direction::Behind => Icon::ChevronLeft,
    })
}

/// Follow Speech's state in the header badge, in words; None when it shows no badge.
pub fn follow_badge(state: FollowState) -> Option<&'static str> {
    Some(match state {
        FollowState::Following => tl!("following"),
        FollowState::Lost => tl!("lost"),
        FollowState::Listening => tl!("listening"),
        FollowState::Starting => tl!("starting"),
        FollowState::Unavailable => tl!("speech unavailable"),
        FollowState::Failed => tl!("speech failed"),
        FollowState::Off | FollowState::Ready => return None,
    })
}

/// Follow Speech's state as a sentence: the live region's text and part of the card's name.
pub fn follow_sentence(f: &CardState) -> String {
    match f.state {
        FollowState::Following => tl!("Following your voice").to_owned(),
        FollowState::Lost => tl!("Lost your place: the prompter holds until you return to the script").to_owned(),
        FollowState::Listening => tl!("Listening for the script").to_owned(),
        FollowState::Starting => tl!("Starting speech recognition").to_owned(),
        FollowState::Unavailable | FollowState::Failed => match &f.reason {
            Some(why) => tlf!("Follow Speech is not working: {why}", why = why),
            None => tl!("Follow Speech is not working").to_owned(),
        },
        FollowState::Off | FollowState::Ready => String::new(),
    }
}

/// The words heard, for the card's name: the committed ones, then the tentative ones said as such.
fn heard_text(f: &CardState) -> Option<String> {
    if f.committed.is_empty() && f.tentative.is_empty() {
        return None;
    }
    let mut text = tlf!("Heard: {words}", words = f.committed.join(" "));
    if !f.tentative.is_empty() {
        text = format!("{} {}", text.trim_end(), tlf!("(not final: {words})", words = f.tentative.join(" ")));
    }
    Some(text)
}

/// What the header says when the prompt time is in no section.
fn no_section(p: &Prompt, gap: Tick) -> &'static str {
    if p.sections == 0 {
        tl!("No sections: range markers on the sequence become prompter sections")
    } else if gap == Tick::ZERO {
        tl!("No section at the playhead")
    } else {
        tl!("No section at the prompter's position")
    }
}

fn point_label(p: &Point) -> String {
    if p.text.is_empty() { "—".to_owned() } else { p.text.clone() }
}

/// The text of the overlay, in reading order: its automation label and accessibility name. `gap`
/// is the prompt time minus the playhead and `rate` the prompter's rate in percent.
pub fn spoken(p: &Prompt, gap: Tick, rate: u32, follow: Option<&CardState>) -> String {
    let mut parts = Vec::new();
    let following = follow.map(follow_sentence).filter(|t| !t.is_empty());
    match &p.section {
        Some(s) => {
            parts.push(format!("{} {}", s.name, tlf!("{elapsed} of {length}", elapsed = clock(s.elapsed), length = clock_up(s.duration))));
            parts.extend(following.clone());
            parts.extend(readout(gap));
            parts.extend(rate_text(rate));
            // the points the card lists when it has room (the same rows), and the count of the rest
            let rows = PointRows::of(s);
            let name = |i: Option<usize>| i.and_then(|i| s.points.get(i)).map(point_label);
            parts.extend(name(rows.done).map(|point| tlf!("Done: {point}", point = point)));
            parts.extend(name(rows.current).map(|point| tlf!("Now: {point}", point = point)));
            parts.extend(rows.upcoming.iter().filter_map(|i| name(Some(*i))).map(|point| tlf!("Then: {point}", point = point)));
            let last = rows.upcoming.last().copied().or(rows.current).or(rows.done);
            let more = last.map_or(0, |i| s.points.len().saturating_sub(i + 1));
            if more > 0 {
                parts.push(tlf!("{n} more", n = more));
            }
            if let Some(line) = s.script.as_ref().and_then(|x| script_excerpt(&x.text, x.offset)) {
                parts.push(tlf!("Script: {text}", text = line));
            }
            parts.extend(follow.and_then(heard_text));
        }
        None => {
            parts.push(no_section(p, gap).to_owned());
            parts.extend(following);
            parts.extend(readout(gap));
            parts.extend(rate_text(rate));
        }
    }
    parts.extend(notices(p));
    // one sentence per part; a part that already ends a sentence gets no second period
    let mut out = String::new();
    for part in parts {
        if !out.is_empty() {
            out.push_str(if out.ends_with(['.', '!', '?', '…']) { " " } else { ". " });
        }
        out.push_str(&part);
    }
    out
}

/// The script from the start of the word at `offset` to the end of that sentence (or line).
fn script_excerpt(text: &str, offset: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut start = offset.min(chars.len());
    while start > 0 && chars.get(start - 1).is_some_and(|c| !c.is_whitespace()) {
        start -= 1;
    }
    let rest = chars.get(start..).unwrap_or_default();
    let end = rest.iter().position(|c| matches!(c, '.' | '!' | '?' | '\n')).map_or(rest.len(), |i| i + 1);
    let excerpt: String = rest.get(..end).unwrap_or_default().iter().collect();
    let excerpt = excerpt.trim();
    (!excerpt.is_empty()).then(|| excerpt.to_owned())
}

/// The lines under the body: the next section, the caption, and warnings.
fn notices(p: &Prompt) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = p.next.as_ref().filter(|n| n.soon) {
        out.push(tlf!("Next: {name} in {time}", name = n.name, time = clock_up(n.starts_in)));
    }
    if let Some(c) = &p.caption {
        out.push(tlf!("Caption: {text}", text = c.lines.join(" / ")));
    }
    out.extend(p.section.iter().flat_map(warnings));
    out
}

fn warnings(s: &Section) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(o) = s.overrun {
        if o.over > Tick::ZERO {
            out.push(tlf!("⚠ Talking points run {time} past the end of the section", time = clock_up(o.over)));
        }
        if o.untimed > 0 {
            out.push(tlf!("⚠ No time left for {n} talking points without a duration", n = o.untimed));
        }
    }
    if s.truncated {
        out.push(tl!("⚠ The marker's comment is too long: only its start is shown").to_owned());
    }
    out
}

/// Type sizes and spacing for one picture size. Everything is the theme's body size and spacing
/// (`Tokens::gap`, `Tokens::radius_sm`) times one factor that follows the picture.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    /// The base type size: the section name, the time and upcoming points.
    base: f32,
    /// The unit of space: between a mark and its text, inside badges, between rows.
    gap: f32,
    /// Inside the card's edge.
    pad: f32,
    /// Between the header, the body and the footer.
    section: f32,
    radius: f32,
}

impl Metrics {
    /// The base size follows the picture's height (3%), or its width for a tall narrow picture,
    /// from the theme's small text (11 pt, the normal layout's Program panel) to 19 pt (full screen
    /// on a large display); spacing scales with it.
    fn new(area: Rect, t: &Tokens, body: f32) -> Self {
        let base = (area.height() * 0.03).min(area.width() * 0.022).clamp(11.0, 19.0);
        let k = base / body.max(1.0);
        Metrics {
            base,
            gap: (t.gap * k).round(),
            pad: (t.gap * 3.0 * k).round(),
            section: (t.gap * 2.0 * k).round(),
            radius: (t.radius_sm * k).clamp(t.radius_sm, t.radius_sm * 2.0).round(),
        }
    }
    /// The current talking point: the line to say now.
    fn now(&self) -> FontId {
        Tokens::semibold((self.base * 1.4).round())
    }
    /// The script: long text, so regular weight, as large as there is room for.
    fn script(&self) -> FontId {
        Tokens::ui((self.base * 1.3).round())
    }
    fn item(&self) -> FontId {
        Tokens::ui(self.base)
    }
    fn done(&self) -> FontId {
        Tokens::ui((self.base * 0.9).round())
    }
    /// Badges and the count of points not listed.
    fn small(&self) -> FontId {
        Tokens::ui((self.base * 0.85).round())
    }
    /// The next section, the caption and the warnings.
    fn foot(&self, strong: bool) -> FontId {
        let size = (self.base * 0.95).round();
        if strong { Tokens::semibold(size) } else { Tokens::ui(size) }
    }
    /// The height of a line of `font`.
    fn line(&self, font: &FontId) -> f32 {
        (font.size * 1.3).round()
    }
    fn bar(&self) -> f32 {
        (self.base * 0.4).round().max(4.0)
    }
    /// The bar under the current point, showing how far through it the narrator is.
    fn underline(&self) -> f32 {
        (self.base * 0.2).round().max(2.0)
    }
    /// The column for the marks (▶, ✓, a dot) in front of the points, so their text lines up.
    fn mark_w(&self) -> f32 {
        (self.base * 1.4 * 0.9).round()
    }
    /// One row of the footer: a line of footer text in a block with `gap / 2` above and below.
    fn foot_row(&self) -> f32 {
        self.line(&self.foot(false)) + self.gap
    }
}

/// One line of `text`, cut with an ellipsis to `width`.
fn line_job(text: &str, font: FontId, color: Color32, width: f32) -> LayoutJob {
    wrap_job(text, font, color, width, 1)
}

/// `text` wrapped to `width` in at most `rows` rows, the last cut with an ellipsis.
fn wrap_job(text: &str, font: FontId, color: Color32, width: f32, rows: usize) -> LayoutJob {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, width.max(1.0));
    job.wrap = TextWrapping { max_width: width.max(1.0), max_rows: rows.max(1), break_anywhere: rows <= 1, overflow_character: Some('…') };
    job
}

/// The talking points to list: the last finished one, the current one, the next ones, and how
/// many follow those.
#[derive(Clone, Debug, Default)]
struct PointRows {
    done: Option<usize>,
    current: Option<usize>,
    upcoming: Vec<usize>,
}

impl PointRows {
    fn of(s: &Section) -> Self {
        // the point the list is about: the current one, else the first still to come
        let focus = s.current.or_else(|| s.points.iter().position(|p| p.state == PointState::Upcoming));
        let done = match focus {
            Some(f) => f.checked_sub(1),
            None => s.points.len().checked_sub(1),
        };
        let first_upcoming = match (s.current, focus) {
            (Some(c), _) => c + 1,
            (None, Some(f)) => f,
            (None, None) => s.points.len(),
        };
        PointRows { done, current: s.current, upcoming: (first_upcoming..s.points.len()).take(UPCOMING_ROWS).collect() }
    }
}

/// What the card holds, chosen in [`plan`] to fit its height.
#[derive(Clone, Debug, Default)]
struct Plan {
    rows: PointRows,
    /// Upcoming points listed (the first ones of `rows.upcoming`).
    upcoming: usize,
    show_done: bool,
    /// Points after the listed ones, said as "N more".
    more: usize,
    /// The current point's text, laid out to the room it has, and its height.
    current: Option<(Arc<Galley>, f32)>,
    script_rows: usize,
    /// The script is under the points, not beside them: a card too narrow for both side by side.
    stacked: bool,
    /// The points' height: the body's when they are beside the script.
    points_h: f32,
    /// The body's height (points and script side by side share it).
    body_h: f32,
    next: bool,
    warnings: usize,
    /// The words Follow Speech heard.
    heard: bool,
    caption: bool,
    /// The caption and the next section share one footer row.
    shared_foot: bool,
    /// The card's height.
    height: f32,
}

/// What the card holds when it may be at most `max_h` tall and `inner_w` wide inside, in order
/// of what the narrator needs most: the header and the current point (or two script lines)
/// always; then the next point, the next section, the warnings, the words Follow Speech heard, the
/// caption, a third script line, the other upcoming points, the last finished point and the count
/// of the rest. When the script is under the points, two lines of it come right after the current
/// point.
///
/// The current point (or the script) is cut to one row when two do not fit in `max_h`, and the
/// card grows past `max_h` to hold that one row, up to `limit` (the picture inside its inset);
/// beyond that the card holds the header alone.
fn plan(d: &Draw, prompt: &Prompt, inner_w: f32, max_h: f32, limit: f32, heard: bool) -> Plan {
    let m = &d.m;
    let mut p = Plan::default();
    let mut max_h = max_h;
    let header = m.line(&m.item()) + if prompt.section.is_some() { m.gap + m.bar() } else { 0.0 };
    let mut left = max_h - 2.0 * m.pad - header;
    let take = |left: &mut f32, cost: f32| {
        let fits = cost <= *left + 0.5;
        if fits {
            *left -= cost;
        }
        fits
    };
    let row_gap = (m.gap / 2.0).round();
    let item_h = m.line(&m.item());
    let script_line = m.line(&m.script());
    let mut body_points = 0.0;
    let mut script_only = false;
    let mut header_only = false;
    if let Some(s) = &prompt.section {
        let (points_w, _, stacked) = body_columns(d, s, inner_w);
        p.stacked = stacked;
        p.rows = PointRows::of(s);
        // required: the current point, else the first listed one, else two script lines; in one
        // row when two do not fit
        let room = left - m.section + 0.5;
        let mut required = 0.0;
        if let Some(c) = p.rows.current.and_then(|i| s.points.get(i)) {
            let (g, h) = current_row(d, c, points_w, room);
            p.current = Some((g, h));
            required += h;
        } else if !p.rows.upcoming.is_empty() {
            p.upcoming = 1;
            required += item_h;
        } else if p.rows.done.is_some() {
            p.show_done = true;
            required += m.line(&m.done());
        }
        if s.points.is_empty() && s.script.is_some() {
            script_only = true;
            p.script_rows = if 2.0 * script_line > room { 1 } else { 2 };
            required = p.script_rows as f32 * script_line;
        }
        if required > 0.0 {
            let need = m.section + required;
            if need > left + 0.5 {
                if 2.0 * m.pad + header + need <= limit + 0.5 {
                    // the card grows past its usual height rather than cut the line to say now
                    max_h = 2.0 * m.pad + header + need;
                    left = need;
                } else {
                    // no room for even one row of it on this picture: the header alone
                    (p.current, p.upcoming, p.show_done, p.script_rows) = (None, 0, false, 0);
                    script_only = false;
                    header_only = true;
                    required = 0.0;
                }
            }
            if required > 0.0 {
                left -= need;
            }
        }
        body_points = required;
        p.points_h = if script_only { 0.0 } else { required };
        if stacked && required > 0.0 && take(&mut left, m.section + script_line) {
            p.script_rows = 1;
            if take(&mut left, script_line) {
                p.script_rows = 2;
            }
        }
        if !script_only && required > 0.0 && p.upcoming == 0 && !p.rows.upcoming.is_empty() && take(&mut left, row_gap + item_h) {
            p.upcoming = 1;
            body_points += row_gap + item_h;
        }
        if header_only {
            let used = max_h - left;
            p.height = used.min(max_h).max(2.0 * m.pad + m.line(&m.item()));
            return p;
        }
    }
    // the footer
    let notices = prompt.next.as_ref().is_some_and(|n| n.soon);
    let warnings = prompt.section.as_ref().map_or(0, |s| warnings(s).len());
    let mut footer_started = false;
    let foot = |left: &mut f32, started: &mut bool| {
        let cost = m.foot_row() + if *started { m.gap } else { m.section };
        let ok = take(left, cost);
        *started |= ok;
        ok
    };
    p.next = notices && foot(&mut left, &mut footer_started);
    for _ in 0..warnings {
        if !foot(&mut left, &mut footer_started) {
            break;
        }
        p.warnings += 1;
    }
    p.heard = heard && foot(&mut left, &mut footer_started);
    if prompt.caption.is_some() {
        p.shared_foot = p.next && inner_w >= m.base * 36.0;
        p.caption = p.shared_foot || foot(&mut left, &mut footer_started);
    }
    if let Some(s) = &prompt.section {
        if (script_only || (p.stacked && p.script_rows == 2)) && take(&mut left, script_line) {
            p.script_rows += 1;
        }
        if !script_only {
            while p.upcoming < p.rows.upcoming.len() && take(&mut left, row_gap + item_h) {
                p.upcoming += 1;
                body_points += row_gap + item_h;
            }
            if !p.show_done && p.rows.done.is_some() && take(&mut left, row_gap + m.line(&m.done())) {
                p.show_done = true;
                body_points += row_gap + m.line(&m.done());
            }
            let last = p.rows.upcoming.get(p.upcoming.wrapping_sub(1)).copied().or(p.rows.current).or(if p.show_done { p.rows.done } else { None });
            let after = last.map_or(s.points.len(), |i| s.points.len().saturating_sub(i + 1));
            if after > 0 && take(&mut left, row_gap + m.line(&m.small())) {
                p.more = after;
                body_points += row_gap + m.line(&m.small());
            }
            if s.script.is_some() && !p.stacked {
                // beside the points, the script has the points' height: as many lines as fit
                p.script_rows = ((body_points / script_line).floor() as usize).clamp(1, SCRIPT_ROWS);
            }
            p.points_h = body_points;
        }
        p.body_h = if script_only {
            p.script_rows as f32 * script_line
        } else if p.stacked && p.script_rows > 0 {
            body_points + m.section + p.script_rows as f32 * script_line
        } else {
            body_points
        };
    }
    let used = max_h - left;
    p.height = used.min(max_h).max(2.0 * m.pad + m.line(&m.item()));
    p
}

/// The current point `c` laid out in a points column `w` wide, and its row's height: in
/// [`CURRENT_ROWS`] rows, or one when those are taller than `room`.
fn current_row(d: &Draw, c: &Point, w: f32, room: f32) -> (Arc<Galley>, f32) {
    let m = &d.m;
    let text_w = current_text_w(d, c, w);
    let layout = |rows| {
        let g = d.p.layout_job(wrap_job(&point_label(c), m.now(), d.c.text, text_w, rows));
        let h = m.gap + g.size().y.max(m.line(&m.now())) + m.gap + m.underline() + m.gap;
        (g, h)
    };
    let (g, h) = layout(CURRENT_ROWS);
    if h > room { layout(1) } else { (g, h) }
}

/// The widths of the points column and the script column (0 for the one the section lacks), and
/// whether the script goes under the points. Beside the script, the points take 42% of the width,
/// more when the current point would have less than [`CURRENT_MIN_EMS`]; when that leaves the
/// script less than [`SCRIPT_MIN_EMS`], both take the whole width, the script under the points.
fn body_columns(d: &Draw, s: &Section, inner_w: f32) -> (f32, f32, bool) {
    match (s.points.is_empty(), s.script.is_none()) {
        (false, false) => {
            let widest_badge = s.points.iter().map(|p| badge_w(d, &point_length(p))).fold(0.0, f32::max);
            let chrome = -current_text_w_for(d, widest_badge, 0.0);
            let points = (inner_w * 0.42).max(d.m.now().size * CURRENT_MIN_EMS + chrome).round();
            let script = inner_w - points - d.m.section;
            if script < d.m.script().size * SCRIPT_MIN_EMS { (inner_w, inner_w, true) } else { (points, script, false) }
        }
        (false, true) => (inner_w, 0.0, false),
        (true, false) => (0.0, inner_w, false),
        (true, true) => (0.0, 0.0, false),
    }
}

/// A point's length: its written duration or its share of the section.
fn point_length(p: &Point) -> String {
    clock_up(p.end.saturating_sub(p.start))
}

/// The room for the current point's text in a points column `w` wide: the row's inset, the mark
/// and a gap on the left; a gap, the badge and the inset on the right (as [`points`] draws them).
fn current_text_w(d: &Draw, p: &Point, w: f32) -> f32 {
    current_text_w_for(d, badge_w(d, &point_length(p)), w)
}

/// [`current_text_w`] with a badge `badge` wide.
fn current_text_w_for(d: &Draw, badge: f32, w: f32) -> f32 {
    let inset = (d.m.gap * 1.5).round();
    w - 2.0 * inset - d.m.mark_w() - 2.0 * d.m.gap - badge
}

/// The painter, colours and sizes the drawing functions share.
struct Draw {
    p: egui::Painter,
    c: Palette,
    m: Metrics,
    /// The section's accent ([`accent`]); `text` when its marker is not found.
    accent: Color32,
    /// The theme's animation time (0 when animation is turned off).
    anim: f32,
}

/// The Program monitor's Prompter, when it is turned on in `mv` (the settings as drawn).
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, which: Which, mv: &MonitorView, area: Rect, time: Tick) {
    if which == Which::Program && mv.prompter {
        overlay(app, ui, area, time);
    }
}

/// Draw the prompter for the playhead `time` at the bottom of the visible picture `area`: what is
/// at the prompt time, and the gap to the playhead.
fn overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect, time: Tick) {
    let Some(seq) = app.session.active_sequence() else { return };
    if area.width() < 60.0 || area.height() < 40.0 {
        return;
    }
    let at = prompt_time_at(&app.session, time);
    let gap = at.saturating_sub(time);
    let percent = rate(&app.session);
    let follow = filmcraft_engine::follow::card(&app.session);
    let prompt = prompt_at(seq, at);
    let t = app.tokens;
    let c = palette(&t);
    let body = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(12.0, |f| f.size);
    let m = Metrics::new(area, &t, body);
    let accent_of = |id: MarkerId| marker_color(app, seq, id).map(|mc| accent(mc, &c));
    let section_accent = prompt.section.as_ref().and_then(|s| accent_of(s.marker)).unwrap_or(c.text);
    let next_accent = prompt.next.as_ref().and_then(|n| accent_of(n.marker)).unwrap_or(c.dim);
    let caption_track = prompt.caption.as_ref().and_then(|cap| seq.caption_tracks.iter().position(|tr| tr.id == cap.track)).map(|i| format!("C{}", i + 1));
    let anim = ui.style().animation_time;
    let d = Draw { p: ui.painter().with_clip_rect(ui.clip_rect().intersect(area)), c, m, accent: section_accent, anim };

    // ---- the card: centred at the bottom, inside the action-safe band
    let inset = (area.width().min(area.height()) * INSET).clamp(2.0 * t.gap, 10.0 * t.gap);
    // the header's readouts before the time: Follow Speech's state, the rate, then the gap; the
    // state is dropped last, then the gap
    let readouts: Vec<Trailing> = follow
        .as_ref()
        .and_then(|f| follow_badge(f.state))
        .map(|text| Trailing::readout(&d, text, Some(Icon::Mic), 3))
        .into_iter()
        .chain(rate_text(percent).map(|text| Trailing::readout(&d, &text, None, 1)))
        .chain(readout(gap).map(|text| Trailing::readout(&d, &text, readout_icon(gap), 2)))
        .collect();
    let heard = follow.as_ref().is_some_and(|f| !f.committed.is_empty() || !f.tentative.is_empty());
    let empty = prompt.section.is_none().then(|| no_section(&prompt, gap));
    // between sections the card holds a line or two, so it is only as wide as its message needs
    let widest = match empty {
        None => m.base * MAX_WIDTH_EMS,
        Some(text) => {
            let message = d.p.layout_no_wrap(text.to_owned(), m.item(), c.dim).size().x;
            let badges: f32 = readouts.iter().map(|r| r.outer_w(&m) + m.section).sum();
            (message + badges + 2.0 * m.pad).max(m.base * MAX_WIDTH_EMS * 0.6)
        }
    };
    let width = (area.width() - 2.0 * inset).min(widest).max(1.0);
    let inner_w = width - 2.0 * m.pad;
    let plan = plan(&d, &prompt, inner_w, area.height() * MAX_HEIGHT, area.height() - 2.0 * inset, heard);
    let card = Rect::from_min_size(
        pos2((area.center().x - width / 2.0).round(), (area.max.y - inset - plan.height).round()),
        vec2(width.round(), plan.height.round()),
    );
    d.p.add(ui.visuals().popup_shadow.as_shape(card, m.radius));
    d.p.rect(card, m.radius, c.card, Stroke::new(1.0, c.edge), StrokeKind::Inside);
    let content = card.shrink(m.pad);
    let d = Draw { p: d.p.with_clip_rect(d.p.clip_rect().intersect(card.shrink(1.0))), ..d };

    // ---- header: flag, section name, readouts, time; the bar
    let mut y = match &prompt.section {
        Some(s) => {
            let time = Trailing::tabular(&d, &format!("{} / {}", clock(s.elapsed), clock_up(s.duration)), d.c.dim);
            let trailing: Vec<&Trailing> = readouts.iter().chain([&time]).collect();
            let below = header(&d, content, Title::Section(&s.name), &trailing) + m.gap;
            let bar = Rect::from_min_size(pos2(content.min.x, below), vec2(inner_w, m.bar()));
            section_bar(&d, bar, s);
            bar.max.y
        }
        None => {
            let trailing: Vec<&Trailing> = readouts.iter().collect();
            header(&d, content, Title::Message(empty.unwrap_or_default()), &trailing)
        }
    };

    // ---- body: the talking points beside the script
    if let Some(s) = &prompt.section
        && plan.body_h > 0.0
    {
        y += m.section;
        let (points_w, script_w, _) = body_columns(&d, s, inner_w);
        if points_w > 0.0 {
            points(&d, Rect::from_min_size(pos2(content.min.x, y), vec2(points_w, plan.points_h)), s, &plan, prompt.time);
        }
        if let (true, true, Some(script)) = (script_w > 0.0, plan.script_rows > 0, &s.script) {
            // beside the points, or under them with a section gap
            let top = if plan.stacked { y + plan.points_h + m.section } else { y };
            let r = Rect::from_min_max(pos2(content.max.x - script_w, top), pos2(content.max.x, y + plan.body_h));
            script_lines(&d, r, script, plan.script_rows, egui::Id::unique(("program-prompter-script", s.marker)));
        }
        y += plan.body_h;
    }

    // ---- footer: the next section, the warnings, the caption
    let mut started = false;
    let mut next_row = |y: &mut f32| {
        *y += if started { m.gap } else { m.section };
        started = true;
        let r = Rect::from_min_size(pos2(content.min.x, *y), vec2(inner_w, m.foot_row()));
        *y = r.max.y;
        r
    };
    let next_w = if plan.shared_foot { (inner_w * 0.45).round() } else { inner_w };
    let next_text = prompt.next.as_ref().map(|n| tlf!("Next: {name} in {time}", name = n.name, time = clock_up(n.starts_in)));
    // what the next section leaves of its row, for the caption when they share it
    let mut caption_room = None;
    if let (true, Some(text)) = (plan.next, &next_text) {
        let r = next_row(&mut y);
        let used = next_notice(&d, Rect::from_min_max(pos2(r.max.x - next_w, r.min.y), r.max), text, next_accent, plan.shared_foot);
        caption_room = Some(Rect::from_min_max(r.min, pos2(r.max.x - used - m.section, r.max.y)));
    }
    for w in prompt.section.iter().flat_map(warnings).take(plan.warnings) {
        warning(&d, next_row(&mut y), &w);
    }
    if let (true, Some(f)) = (plan.heard, &follow) {
        heard_line(&d, next_row(&mut y), f);
    }
    if let (true, Some(cap)) = (plan.caption, &prompt.caption) {
        let r = match caption_room.filter(|_| plan.shared_foot) {
            Some(r) => r,
            None => next_row(&mut y),
        };
        caption_line(&d, r, caption_track.as_deref(), &cap.lines.join(" / "));
    }

    let label = spoken(&prompt, gap, percent, follow.as_ref());
    let response = ui.interact(card, egui::Id::unique("program-prompter"), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    app.auto.add("program.prompter", card, &label);
    // Follow Speech's state alone, as a polite live region: a change to "lost" and back is
    // announced; the words heard and the clock are not
    if let Some(f) = &follow {
        let text = follow_sentence(f);
        let id = egui::Id::unique("program-prompter-follow");
        let response = ui.interact(card, id, Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Status, true, &text));
        ui.ctx().accesskit_node_builder(id, |node| node.set_live(egui::accesskit::Live::Polite));
        app.auto.add("program.prompter.follow", card, &text);
    }
}

/// The words Follow Speech heard in `r`: "Heard:", the last committed words, then the tentative
/// ones in italics and dimmed (they may still change). The oldest words give way when the row is
/// full, so the newest are always shown.
fn heard_line(d: &Draw, r: Rect, f: &CardState) {
    let m = &d.m;
    let font = m.foot(false);
    let job = |skip: usize| {
        let mut job = LayoutJob::default();
        let format = |color: Color32, italics: bool| TextFormat { font_id: font.clone(), color, italics, ..Default::default() };
        job.append(tl!("Heard:"), 0.0, format(d.c.dim, false));
        if skip > 0 {
            job.append(" …", 0.0, format(d.c.dim, false));
        }
        for w in f.committed.iter().skip(skip) {
            job.append(&format!(" {w}"), 0.0, format(d.c.text, false));
        }
        for w in &f.tentative {
            job.append(&format!(" {w}"), 0.0, format(d.c.dim, true));
        }
        job.wrap = TextWrapping { max_width: r.width(), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        job
    };
    let mut skip = 0;
    let mut g = d.p.layout_job(job(0));
    while skip < f.committed.len() && g.elided {
        skip += 1;
        g = d.p.layout_job(job(skip));
    }
    d.p.galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, d.c.text);
}

/// A piece of the header's right end: the time (plain) or a readout (in a badge).
struct Trailing {
    pieces: Vec<(f32, Arc<Galley>)>,
    width: f32,
    color: Color32,
    /// Drawn in a readout badge, with this glyph before the text.
    badge: Option<Option<Icon>>,
    /// The order pieces go in when the header is short of room: the lowest first.
    keep: u8,
}

impl Trailing {
    /// `text` in tabular figures, so its digits do not shift as they change; the first piece to go.
    fn tabular(d: &Draw, text: &str, color: Color32) -> Self {
        let (pieces, width) = tabular_text(&d.p, text, &d.m.item(), color);
        Trailing { pieces, width, color, badge: None, keep: 0 }
    }

    /// A readout badge: the duration badges' pill, but outlined in `text` with a heavier line and
    /// semibold text, since it says the prompter is not where the picture is. Its text is a little
    /// smaller than the header's so it sits clear of the outline in a pill as tall as the row. The
    /// glyph only repeats what the words say.
    fn readout(d: &Draw, text: &str, icon: Option<Icon>, keep: u8) -> Self {
        let (pieces, width) = tabular_text(&d.p, text, &Tokens::semibold((d.m.base * 0.9).round()), d.c.text);
        Trailing { pieces, width, color: d.c.text, badge: Some(icon), keep }
    }

    /// The width it takes in the header.
    fn outer_w(&self, m: &Metrics) -> f32 {
        match self.badge {
            None => self.width,
            Some(icon) => self.width + 2.0 * badge_pad(m) + icon.map_or(0.0, |_| readout_icon_w(m) + m.gap / 2.0),
        }
    }

    /// Draw it with its left end at `x`, centred on `cy`.
    fn paint(&self, d: &Draw, x: f32, cy: f32) {
        let m = &d.m;
        let Some(icon) = self.badge else {
            paint_tabular(&d.p, pos2(x, cy), self.pieces.clone(), self.color);
            return;
        };
        let h = m.line(&m.item());
        let r = Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(self.outer_w(m), h));
        d.p.rect(r, h / 2.0, d.c.card, Stroke::new(1.5, d.c.text), StrokeKind::Inside);
        let mut tx = r.min.x + badge_pad(m);
        if let Some(icon) = icon {
            let w = readout_icon_w(m);
            icons::paint(&d.p, Rect::from_center_size(pos2(tx + w / 2.0, cy), vec2(w, w)), icon, d.c.text);
            tx += w + m.gap / 2.0;
        }
        paint_tabular(&d.p, pos2(tx, cy), self.pieces.clone(), self.color);
    }
}

/// The room between a readout badge's outline and its content, at either end.
fn badge_pad(m: &Metrics) -> f32 {
    (m.gap * 1.5).round()
}

/// The size of a readout badge's glyph.
fn readout_icon_w(m: &Metrics) -> f32 {
    (m.base * 0.75).round()
}

/// What the header row starts with.
#[derive(Clone, Copy)]
enum Title<'a> {
    /// The section: its flag in its accent and its name.
    Section(&'a str),
    /// A message in place of a section ("No section at the playhead").
    Message(&'a str),
}

/// The header row at the top of `r`: the title, cut to the room the `trailing` pieces leave
/// (right-aligned, the last at the right edge). When the title would get less than three ems,
/// pieces are dropped, lowest `keep` first (the time, then the rate), but the last one stays:
/// while a readout shows it matters more than the section's name. Returns the row's bottom.
fn header(d: &Draw, r: Rect, title: Title, trailing: &[&Trailing]) -> f32 {
    let m = &d.m;
    let h = m.line(&m.item());
    let cy = r.min.y + h / 2.0;
    let mut shown: Vec<&Trailing> = trailing.to_vec();
    let needs = |shown: &[&Trailing]| shown.iter().map(|t| t.outer_w(m) + m.section).sum::<f32>();
    while shown.len() > 1 && needs(&shown) > r.width() - m.base * 3.0 {
        if let Some(i) = shown.iter().enumerate().min_by_key(|(_, t)| t.keep).map(|(i, _)| i) {
            shown.remove(i);
        }
    }
    let mut right = r.max.x;
    for piece in shown.iter().rev() {
        right -= piece.outer_w(m);
        piece.paint(d, right, cy);
        right -= m.section;
    }
    match title {
        Title::Section(name) => {
            let flag_h = (m.base * 0.85).round();
            let flag = Rect::from_min_size(pos2(r.min.x, cy - flag_h / 2.0), vec2((flag_h * 0.75).round(), flag_h));
            marker_glyph(&d.p, flag, (flag_h * 0.35).round(), MarkerPart::Whole, d.accent);
            let x = flag.max.x + m.gap * 1.5;
            if right - x > m.base {
                let g = d.p.layout_job(line_job(name, Tokens::semibold(m.base), d.c.text, right - x));
                d.p.galley(pos2(x, cy - g.size().y / 2.0), g, d.c.text);
            }
        }
        Title::Message(text) => {
            let g = d.p.layout_job(line_job(text, m.item(), d.c.dim, right - r.min.x));
            d.p.galley(pos2(r.min.x, cy - g.size().y / 2.0), g, d.c.dim);
        }
    }
    r.min.y + h
}

/// The section's progress over its groove, split where one talking point ends and the next
/// begins (a bar too short for its segments stays whole).
fn section_bar(d: &Draw, bar: Rect, s: &Section) {
    let span = s.duration.0.max(1) as f64;
    let frac = |t: Tick| ((t.0.saturating_sub(s.start.0)) as f64 / span).clamp(0.0, 1.0) as f32;
    let mut cuts: Vec<f32> = s.points.iter().map(|p| frac(p.end)).filter(|f| *f > 0.0 && *f < 1.0).collect();
    cuts.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);
    let split = d.m.gap.clamp(2.0, 4.0);
    if bar.width() / (cuts.len() + 1) as f32 <= split * 4.0 {
        cuts.clear();
    }
    let progress_x = bar.min.x + bar.width() * s.progress as f32;
    let mut from = 0.0;
    for to in cuts.into_iter().chain([1.0]) {
        let x0 = bar.min.x + bar.width() * from + if from > 0.0 { split / 2.0 } else { 0.0 };
        let x1 = bar.min.x + bar.width() * to - if to < 1.0 { split / 2.0 } else { 0.0 };
        from = to;
        if x1 <= x0 {
            continue;
        }
        let seg = Rect::from_min_max(pos2(x0, bar.min.y), pos2(x1, bar.max.y));
        meter(d, seg, ((progress_x - x0) / (x1 - x0)).clamp(0.0, 1.0));
    }
}

/// A groove filled `fraction` of the way in the accent.
fn meter(d: &Draw, r: Rect, fraction: f32) {
    let radius = r.height() / 2.0;
    d.p.rect_filled(r, radius, d.c.track);
    if fraction > 0.0 {
        let w = (r.width() * fraction).max(r.height().min(r.width()));
        d.p.rect_filled(Rect::from_min_size(r.min, vec2(w, r.height())), radius, d.accent);
    }
}

/// The width of a badge showing `text`.
fn badge_w(d: &Draw, text: &str) -> f32 {
    tabular_text(&d.p, text, &d.m.small(), d.c.dim).1 + 2.0 * d.m.gap
}

/// A duration badge (an outlined pill in tabular figures, on the card's colour) with its right
/// end at `right`, centred on `cy`.
fn badge(d: &Draw, right: f32, cy: f32, text: &str) {
    let (pieces, w) = tabular_text(&d.p, text, &d.m.small(), d.c.dim);
    let h = d.m.line(&d.m.small());
    let r = Rect::from_min_max(pos2(right - w - 2.0 * d.m.gap, cy - h / 2.0), pos2(right, cy + h / 2.0));
    d.p.rect(r, h / 2.0, d.c.card, Stroke::new(1.0, d.c.edge), StrokeKind::Inside);
    paint_tabular(&d.p, pos2(r.min.x + d.m.gap, cy), pieces, d.c.dim);
}

/// The talking points in `r`: the last finished one small with ✓, the current one large and bold
/// on a highlight with ▶, its duration and how far through it the narrator is, then the next ones
/// with their durations and how many more follow.
fn points(d: &Draw, r: Rect, s: &Section, plan: &Plan, time: Tick) {
    let m = &d.m;
    let row_gap = (m.gap / 2.0).round();
    let inset = (m.gap * 1.5).round();
    let mark_x = r.min.x + inset;
    let text_x = mark_x + m.mark_w() + m.gap;
    let mut y = r.min.y;
    if let Some(p) = plan.rows.done.filter(|_| plan.show_done).and_then(|i| s.points.get(i)) {
        let h = m.line(&m.done());
        let cy = y + h / 2.0;
        let mark = d.p.layout_no_wrap("✓".to_owned(), m.done(), d.c.dim);
        d.p.galley(pos2(mark_x + (m.mark_w() - mark.size().x) / 2.0, cy - mark.size().y / 2.0), mark, d.c.dim);
        let g = d.p.layout_job(line_job(&point_label(p), m.done(), d.c.dim, r.max.x - inset - text_x));
        d.p.galley(pos2(text_x, cy - g.size().y / 2.0), g, d.c.dim);
        y += h + row_gap;
    }
    if let (Some(p), Some((g, h))) = (plan.rows.current.and_then(|i| s.points.get(i)), &plan.current) {
        let row = Rect::from_min_size(pos2(r.min.x, y), vec2(r.width(), *h));
        d.p.rect_filled(row, m.radius, d.c.current);
        let first_cy = row.min.y + m.gap + m.line(&m.now()) / 2.0;
        let icon = (m.now().size * 0.75).round();
        icons::paint(&d.p, Rect::from_center_size(pos2(mark_x + m.mark_w() / 2.0, first_cy), vec2(icon, icon)), Icon::Play, d.c.text);
        let first_row_h = g.rows.first().map_or(g.size().y, |row| row.rect().height());
        d.p.galley(pos2(text_x, first_cy - first_row_h / 2.0), g.clone(), d.c.text);
        badge(d, row.max.x - inset, first_cy, &point_length(p));
        let span = p.end.0.saturating_sub(p.start.0).max(1) as f64;
        let through = ((time.0.saturating_sub(p.start.0)) as f64 / span).clamp(0.0, 1.0) as f32;
        let ul = Rect::from_min_max(pos2(text_x, row.max.y - m.gap - m.underline()), pos2(row.max.x - inset, row.max.y - m.gap));
        meter(d, ul, through);
        y = row.max.y + row_gap;
    }
    let item_h = m.line(&m.item());
    for p in plan.rows.upcoming.iter().take(plan.upcoming).filter_map(|i| s.points.get(*i)) {
        let cy = y + item_h / 2.0;
        d.p.circle_stroke(pos2(mark_x + m.mark_w() / 2.0, cy), (m.base * 0.2).round().max(2.5), Stroke::new(1.25, d.c.dim));
        let label = point_length(p);
        let bw = badge_w(d, &label);
        badge(d, r.max.x - inset, cy, &label);
        let g = d.p.layout_job(line_job(&point_label(p), m.item(), d.c.dim, r.max.x - inset - bw - m.gap - text_x));
        d.p.galley(pos2(text_x, cy - g.size().y / 2.0), g, d.c.dim);
        y += item_h + row_gap;
    }
    if plan.more > 0 {
        let h = m.line(&m.small());
        let g = d.p.layout_no_wrap(tlf!("{n} more", n = plan.more), m.small(), d.c.dim);
        d.p.galley(pos2(text_x, y + (h - g.size().y) / 2.0), g, d.c.dim);
    }
}

/// The script in `r`, `rows` lines of it, with the line holding the narrator's position on the
/// reading line: a band on the current-point highlight with a bar at its left, the second line of
/// three (the first while the first line is read) or else the first. What is already read is
/// dimmed. The script moves up a whole line at a time, eased over the theme's animation time (none
/// when Settings ▸ Appearance turns animation off), so no line is ever shown cut in half.
fn script_lines(d: &Draw, r: Rect, script: &Script, rows: usize, id: egui::Id) {
    let m = &d.m;
    let (text, offset) = (&script.text, script.offset);
    let row_h = m.line(&m.script());
    let bar_w = (m.gap * 0.75).round().max(3.0);
    // one layout per text and width: the reading position only changes which part is drawn dim,
    // so it never lays the script out again (the placeholder colour takes the colour drawn with)
    let mut job = LayoutJob::default();
    job.append(text, 0.0, TextFormat { font_id: m.script(), color: Color32::PLACEHOLDER, line_height: Some(row_h), ..Default::default() });
    let text_x = r.min.x + bar_w + m.gap * 1.5;
    job.wrap.max_width = (r.max.x - m.gap - text_x).max(1.0);
    let galley = d.p.layout_job(job);
    // the row holding `offset`
    let mut seen = 0usize;
    let mut current = (0usize, 0.0);
    let last = galley.rows.len().saturating_sub(1);
    for (i, row) in galley.rows.iter().enumerate() {
        let n = row.char_count_including_newline().0;
        if offset < seen + n || i == last {
            current = (i, row.rect().min.y);
            break;
        }
        seen += n;
    }
    let reading_row = if rows >= SCRIPT_ROWS { current.0.min(1) } else { 0 };
    let reading = r.min.y + row_h * reading_row as f32;
    let target = reading - current.1;
    let shift = d.p.ctx().animate_value_with_time(id, target, d.anim);
    let band = Rect::from_min_size(pos2(r.min.x, reading), vec2(r.width(), row_h));
    let p = d.p.with_clip_rect(d.p.clip_rect().intersect(r));
    p.rect_filled(band, m.radius, d.c.current);
    p.rect_filled(Rect::from_min_size(band.min, vec2(bar_w, row_h)).shrink2(vec2(0.0, (m.gap / 2.0).round())), bar_w / 2.0, d.c.text);
    // what is read is dim: the rows above the reading position's row and its start up to it
    let at = galley.pos_from_cursor(egui::text::CCursor::new(offset));
    let (row_top, row_bottom, split_x) = (shift + at.min.y, shift + at.max.y, text_x + at.min.x);
    for (area, color) in [
        (Rect::from_min_max(pos2(r.min.x, r.min.y), pos2(r.max.x, row_top)), d.c.dim),
        (Rect::from_min_max(pos2(r.min.x, row_top), pos2(split_x, row_bottom)), d.c.dim),
        (Rect::from_min_max(pos2(split_x, row_top), pos2(r.max.x, row_bottom)), d.c.text),
        (Rect::from_min_max(pos2(r.min.x, row_bottom), pos2(r.max.x, r.max.y)), d.c.text),
    ] {
        p.with_clip_rect(p.clip_rect().intersect(area)).galley(pos2(text_x, shift), galley.clone(), color);
    }
}

/// The next section at the right end of `r` (or its left end when it has the row to itself): its
/// marker's flag and "Next: … in m:ss". Returns the width used.
fn next_notice(d: &Draw, r: Rect, text: &str, flag_color: Color32, right_aligned: bool) -> f32 {
    let m = &d.m;
    let flag_h = (m.base * 0.8).round();
    let flag_w = (flag_h * 0.75).round();
    let g = d.p.layout_job(line_job(text, m.foot(true), d.c.text, r.width() - flag_w - m.gap * 1.5));
    let used = flag_w + m.gap * 1.5 + g.size().x;
    let x = if right_aligned { r.max.x - used } else { r.min.x };
    let cy = r.center().y;
    marker_glyph(&d.p, Rect::from_min_size(pos2(x, cy - flag_h / 2.0), vec2(flag_w, flag_h)), (flag_h * 0.35).round(), MarkerPart::Whole, flag_color);
    d.p.galley(pos2(x + flag_w + m.gap * 1.5, cy - g.size().y / 2.0), g, d.c.text);
    used
}

/// A warning in `r`: bold text with its ⚠ on a callout block.
fn warning(d: &Draw, r: Rect, text: &str) {
    let m = &d.m;
    d.p.rect_filled(r, m.radius, d.c.callout);
    let x = r.min.x + m.gap * 1.5;
    let g = d.p.layout_job(line_job(text, m.foot(true), d.c.text, r.max.x - m.gap * 1.5 - x));
    d.p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, d.c.text);
}

/// The caption line in `r` as the Timeline draws captions: the track's badge (C1) beside a
/// caption block with its text.
fn caption_line(d: &Draw, r: Rect, track: Option<&str>, text: &str) {
    let m = &d.m;
    let radius = (m.radius * 0.75).round();
    let mut x = r.min.x;
    if let Some(track) = track {
        let g = d.p.layout_no_wrap(track.to_owned(), Tokens::semibold(m.small().size), d.c.caption_text);
        let badge = Rect::from_min_size(pos2(x, r.min.y), vec2(g.size().x + 2.0 * m.gap, r.height()));
        d.p.rect_filled(badge, radius, d.c.caption);
        d.p.galley(badge.center() - g.size() / 2.0, g, d.c.caption_text);
        x = badge.max.x + m.gap;
    }
    let pad = m.gap * 1.5;
    let g = d.p.layout_job(line_job(text, m.foot(false), d.c.caption_text, r.max.x - x - 2.0 * pad));
    let block = Rect::from_min_size(pos2(x, r.min.y), vec2(g.size().x + 2.0 * pad, r.height()));
    d.p.rect_filled(block, radius, d.c.caption);
    d.p.galley(pos2(block.min.x + pad, block.center().y - g.size().y / 2.0), g, d.c.caption_text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The prompt at `seconds` in the demo with one twelve-second section from `comment`.
    fn prompt(comment: &str, seconds: f64) -> Prompt {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let fps = s.sequence_rate().frame_at(Tick(TICKS_PER_SECOND));
        s.execute("markers.add", serde_json::json!({"time": 0, "name": "Opening", "comment": comment, "durationFrames": 12 * fps})).unwrap();
        prompt_at(s.active_sequence().unwrap(), Tick::from_seconds_f64(seconds))
    }

    /// A frame of the app's fonts and theme, and the drawing state for a picture of each size.
    fn drawing(sizes: &[(f32, f32)], mut f: impl FnMut((f32, f32), &Draw)) {
        let ctx = egui::Context::default();
        let t = Tokens::for_kind(ThemeKind::Dark);
        crate::theme::install(&ctx, &t);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for &(w, h) in sizes {
                let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h));
                let body = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(12.0, |f| f.size);
                f((w, h), &Draw { p: ui.painter().clone(), c: palette(&t), m: Metrics::new(area, &t, body), accent: Color32::WHITE, anim: 0.0 });
            }
        })
        .textures_delta
        .clear();
    }

    /// Plans the card for `prompt` over a picture of each size as the overlay does (with a row of
    /// words Follow Speech `heard` to fit in too), and hands each plan to `check` with the card's
    /// room for content.
    fn plans(prompt: &Prompt, sizes: &[(f32, f32)], heard: bool, mut check: impl FnMut((f32, f32), &Draw, &Plan, f32)) {
        drawing(sizes, |(w, h), d| {
            let t = Tokens::for_kind(ThemeKind::Dark);
            let inset = (w.min(h) * INSET).clamp(2.0 * t.gap, 10.0 * t.gap);
            let inner_w = (w - 2.0 * inset).min(d.m.base * MAX_WIDTH_EMS) - 2.0 * d.m.pad;
            let plan = plan(d, prompt, inner_w, h * MAX_HEIGHT, h - 2.0 * inset, heard);
            check((w, h), d, &plan, h - 2.0 * inset);
        });
    }

    #[test]
    fn reading_on_through_the_script_lays_it_out_once() {
        let p = prompt(&format!("{}\n- A point", "Every region grew this year. ".repeat(200)), 1.0);
        let script = p.section.and_then(|s| s.script).unwrap();
        drawing(&[(1600.0, 900.0)], |_, d| {
            let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 120.0));
            let cached = || d.p.ctx().fonts(|f| f.num_galleys_in_cache());
            let before = cached();
            // the narrator's position moves on every few frames while the playhead runs
            for offset in (0..script.chars).step_by(37) {
                script_lines(d, r, &Script { offset, ..script.clone() }, SCRIPT_ROWS, egui::Id::unique("script"));
            }
            let added = cached() - before;
            assert!(added <= 1, "{} positions, {added} layouts of the script", script.chars.div_ceil(37));
        });
    }

    /// The characters the current point shows, an ellipsis included.
    fn shown(plan: &Plan) -> String {
        plan.current.as_ref().map(|(g, _)| g.rows.iter().flat_map(|r| r.glyphs.iter().map(|g| g.chr)).collect()).unwrap_or_default()
    }

    // pictures from a small Program monitor up: 16:9, and the 300x120 one as letterboxed
    const SIZES: [(f32, f32); 7] = [(213.0, 120.0), (300.0, 120.0), (300.0, 169.0), (350.0, 197.0), (420.0, 236.0), (640.0, 360.0), (1600.0, 900.0)];

    #[test]
    fn the_current_point_stays_inside_the_card_on_a_small_picture() {
        for comment in [
            "- Say hello to everyone in the room [0:04]\n- Introduce the topic",
            "Sales grew every quarter this year.\n- Say hello to everyone in the room [0:04]\n- Introduce the topic",
        ] {
            let p = prompt(comment, 1.0);
            // the current row's height without and then with a row of words Follow Speech heard
            let mut rows = Vec::new();
            for heard in [false, true] {
                plans(&p, &SIZES, heard, |size, d, plan, limit| {
                    let m = &d.m;
                    let (_, current_h) = plan.current.as_ref().unwrap_or_else(|| panic!("{size:?}: the current point is on the card"));
                    let header = m.line(&m.item()) + m.gap + m.bar();
                    let above_and_in = 2.0 * m.pad + header + m.section + current_h;
                    assert!(
                        above_and_in <= plan.height + 0.5,
                        "{size:?}, heard {heard}: the current row ends inside the card ({above_and_in} of {})",
                        plan.height
                    );
                    assert!(plan.height <= limit + 0.5, "{size:?}, heard {heard}: the card stays inside the picture ({} of {limit})", plan.height);
                    rows.push((size, *current_h));
                });
            }
            let (without, with) = rows.split_at(SIZES.len());
            for ((size, a), (_, b)) in without.iter().zip(with) {
                assert!((a - b).abs() < 0.5, "{size:?}: the heard words never take the current point's room ({a} without, {b} with)");
            }
        }
    }

    #[test]
    fn the_current_point_beside_a_script_shows_its_first_words() {
        let with_script = prompt(
            "Sales grew every quarter this year, and the second half carried most of it.\n- Say hello to everyone in the room [0:04]\n- Introduce the topic",
            1.0,
        );
        let alone = prompt("- Say hello to everyone in the room [0:04]\n- Introduce the topic", 1.0);
        let mut seen = Vec::new();
        plans(&alone, &SIZES, false, |_, _, plan, _| seen.push(shown(plan)));
        let mut alone_shown = seen.into_iter();
        plans(&with_script, &SIZES, true, |size, _, plan, _| {
            let text = shown(plan);
            assert!(text.starts_with("Say hello to"), "{size:?}: the narrator can read the start of the point: {text:?}");
            // a section without a script keeps its full-width points
            assert!(alone_shown.next().is_some_and(|a| a.starts_with("Say hello to")), "{size:?}: alone too");
        });
    }

    #[test]
    fn the_prompter_text_keeps_its_contrast_over_any_picture_in_every_theme() {
        // marker colours: a 6-step cube of the RGB space, which holds the label presets' extremes
        let steps = [0u8, 51, 102, 153, 204, 255];
        let markers: Vec<Color32> = (0..steps.len().pow(3)).map(|i| Color32::from_rgb(steps[i / 36], steps[i / 6 % 6], steps[i % 6])).collect();
        for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
            for accessible in [false, true] {
                let c = palette(&Tokens::for_kind(kind).with_appearance(None, accessible));
                // opaque, so no picture (a burned-in caption included) changes any pair below
                assert_eq!(c.card.a(), 255, "{kind:?}: the card hides the picture under it");
                for (what, fg, bg, min) in [
                    ("text", c.text, c.card, 4.5),
                    ("dimmed text: time, points, badges", c.dim, c.card, 4.5),
                    ("the current point", c.text, c.current, 4.5),
                    ("script already read on the reading line", c.dim, c.current, 4.5),
                    ("a warning", c.text, c.callout, 4.5),
                    ("the caption", c.caption_text, c.caption, 4.5),
                    ("the reading bar and the current point's mark", c.text, c.current, 3.0),
                    ("a bar without a marker colour", c.text, c.track, 3.0),
                    ("the current point's highlight shows on the card", c.current, c.card, 1.2),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(ratio >= min, "{kind:?} (accessible {accessible}): {what} has {ratio:.2}:1");
                }
                for m in &markers {
                    let a = accent(*m, &c);
                    for (on, bg) in [("card", c.card), ("groove", c.track)] {
                        let ratio = contrast(a, bg);
                        assert!(ratio >= 3.0, "{kind:?} (accessible {accessible}): marker {m:?} as {a:?} has {ratio:.2}:1 on the {on}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_marker_colour_that_already_reads_is_kept_as_it_is() {
        let c = palette(&Tokens::for_kind(ThemeKind::Dark));
        let yellow = Color32::from_rgb(0xe0, 0xd0, 0x40);
        assert_eq!(accent(yellow, &c), yellow);
        let navy = Color32::from_rgb(0x10, 0x18, 0x60);
        let lifted = accent(navy, &c);
        let hue = |c: Color32| egui::ecolor::Hsva::from(c).h;
        assert!(lifted != navy && (hue(lifted) - hue(navy)).abs() < 0.05, "a dark marker is lightened, keeping its hue: {lifted:?}");
    }
}
