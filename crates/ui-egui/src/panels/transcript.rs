//! Text panel ▸ Transcript: text-based editing of the active sequence as in Premiere Pro (see
//! `docs/transcripts.md` § Premiere parity). Every action is an engine command (`transcript.*`,
//! `sequence.transcribe`, `prefs.set`); the view state is [`TranscriptUi`] (serde, so
//! `ui.set {"transcript": {...}}` and `ui.inspect` reach it).
//!
//! Automation ids (`text.transcript.*`): `generate` (empty state), `banner.transcribe`, `progress`,
//! `cancel`; toolbar `search`, `filter` (menu `filter.text|fillers|pauses|searchSettings`),
//! `autoInOut`, `extract`, `lift`, `more` (menu `more.createCaptions|transcribe|viewOptions|autoScroll`);
//! results row `delete`, `count`, `prev`, `next`, delete row `deleteMode.extract|lift`, `deleteAll`,
//! `deleteOne`; text `segment.<n>`, `word.<i>`, `pause.<i>` (the pause after word i). The Transcript
//! View Options dialog: `transcriptViewOptions.fillerWords|pauses|minPauseLength|wholeWords|matchCase|save|cancel`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_edit::transcript::{self as tx, SeqWord};
use filmcraft_engine::settings::TranscriptPrefs;
use filmcraft_engine::transcript::{Filter, Match};
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// A pause of at least this long starts a new segment (as `transcript.inspect`'s paragraphs).
const SEGMENT_GAP_SECONDS: f64 = 1.5;
const WORD_SIZE: f32 = 13.5;
const LINE_H: f32 = 21.0;

/// View state of the Transcript tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptUi {
    /// The search filter: Text (the search field), Filler words or Pauses.
    pub filter: Filter,
    /// The current match (∧ ∨), an index into the matches.
    pub current: usize,
    /// The Delete row under the results row is open; `lift` = its Lift radio (else Extract).
    pub delete_open: bool,
    pub lift: bool,
    /// The selected pause: the index of the word it follows.
    pub pause: Option<usize>,
    /// The word the text cursor is at (a plain click, keyboard navigation).
    pub caret: Option<usize>,
    /// The open Transcript View Options dialog and the values being edited.
    pub view_options: Option<TranscriptPrefs>,
}

/// What a click or drag in the text lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Word(usize),
    /// The pause after word i.
    Pause(usize),
}

impl Tok {
    /// The word a selection reaches when it ends on this token.
    fn word(self) -> usize {
        match self {
            Tok::Word(i) | Tok::Pause(i) => i,
        }
    }
}

struct Placed {
    tok: Tok,
    rect: Rect,
    galley: Arc<egui::Galley>,
}

/// Premiere's search highlights: every match orange, the current one salmon.
const MATCH_BG: Color32 = Color32::from_rgba_premultiplied(104, 62, 16, 110);
const CURRENT_MATCH_BG: Color32 = Color32::from_rgba_premultiplied(176, 78, 62, 190);

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    if app.session.active_sequence().is_none() {
        crate::dock::placeholder(ui, rect, &t, tl!("Open a sequence to see its transcript"));
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let words = filmcraft_engine::transcript::sequence_words(&app.session);
    let job = filmcraft_engine::transcript::running_job(&app.session).cloned();
    view_options_dialog(app, ui.ctx());
    if words.is_empty() {
        empty_state(app, ui, rect, job.as_ref(), &mut actions);
        run(app, ui, actions);
        return;
    }
    let n = words.len();
    // a selection left over from a longer transcript (an edit over the control channel) is none
    if app.ui.transcript_sel.is_some_and(|(a, b)| a >= n || b >= n) {
        app.ui.transcript_sel = None;
    }
    if app.ui.transcript.pause.is_some_and(|p| tx::pause_after(&words, p).is_none()) {
        app.ui.transcript.pause = None;
    }
    let prefs = app.session.prefs.transcript.clone();
    let mut y = rect.min.y + 2.0;

    // ---- toolbar: search, filter, { }, Extract, Lift, •••
    let bar = Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(rect.width() - 20.0, 24.0));
    toolbar(app, ui, bar, &mut actions);
    y = bar.max.y + 6.0;

    // ---- search results: matches, counter, ∧ ∨, Delete
    let filter = app.ui.transcript.filter;
    let query = app.ui.transcript_search.clone();
    let searching = filter != Filter::Text || !query.trim().is_empty();
    let found: Vec<Match> = if searching { filmcraft_engine::transcript::matches(&app.session, &words, filter, &query, &json!({})) } else { Vec::new() };
    let changed_key = egui::Id::new("transcript-search-last");
    let key = format!("{}\u{1}{query}", filter.name());
    if ui.ctx().data(|d| d.get_temp::<String>(changed_key)).as_deref() != Some(key.as_str()) {
        ui.ctx().data_mut(|d| d.insert_temp(changed_key, key));
        app.ui.transcript.current = 0;
    }
    if !found.is_empty() {
        app.ui.transcript.current = app.ui.transcript.current.min(found.len() - 1);
    }
    let mut scroll_to: Option<Tok> = None;
    if searching {
        let row = Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(rect.width() - 20.0, 24.0));
        if let Some(tok) = results_row(app, ui, row, &words, &found, &mut actions) {
            scroll_to = Some(tok);
        }
        y = row.max.y + 4.0;
        if app.ui.transcript.delete_open {
            let row = Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(rect.width() - 20.0, 24.0));
            delete_row(app, ui, row, &words, &found, &query, &mut actions);
            y = row.max.y + 4.0;
        }
    }

    // ---- transcription in progress, or clips still without a transcript
    if let Some(j) = &job {
        let row = Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(rect.width() - 20.0, 24.0));
        progress_row(app, ui, row, j, &mut actions);
        y = row.max.y + 4.0;
    } else if let Some(text) = untranscribed(app) {
        let row = Rect::from_min_size(pos2(rect.min.x + 10.0, y), vec2(rect.width() - 20.0, 24.0));
        banner(app, ui, row, &text, &mut actions);
        y = row.max.y + 4.0;
    }

    // ---- the text
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, y + 2.0), pos2(rect.max.x - 6.0, rect.max.y - 4.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let hits: BTreeSet<usize> = found.iter().filter_map(|m| if let Match::Words { from, to } = m { Some(*from..=*to) } else { None }).flatten().collect();
    let pause_hits: BTreeSet<usize> = found.iter().filter_map(|m| if let Match::Pause(p) = m { Some(p.after) } else { None }).collect();
    let current_match = found.get(app.ui.transcript.current).copied();
    let fillers: BTreeSet<usize> = if prefs.filler_words || filter == Filter::Fillers {
        filmcraft_engine::transcript::filler_hits(&app.session, &words, &json!({})).into_iter().flatten().collect()
    } else {
        BTreeSet::new()
    };
    let min_pause = filmcraft_engine::transcript::min_pause(&app.session, &json!({}));
    let pauses: BTreeMap<usize, tx::Pause> =
        if prefs.pauses || filter == Filter::Pauses { tx::pauses(&words, min_pause).into_iter().map(|p| (p.after, p)).collect() } else { BTreeMap::new() };
    let ph = app.session.playhead();
    let rate = app.session.sequence_rate();
    let playing_pause = pauses.values().find(|p| p.start <= ph && ph < p.end).map(|p| p.after);
    let playing_word = playing_word(&words, ph, rate.frame_duration(), playing_pause.is_some());
    let df = app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
    let tc = |x: Tick| format_time(x, rate, df, TimeDisplay::Timecode, 48_000);
    let paras = tx::paragraphs(&words, Tick::from_seconds_f64(SEGMENT_GAP_SECONDS));
    let sel = app.ui.transcript_sel.map(|(a, b)| (a.min(b), a.max(b)));
    let sel_pause = app.ui.transcript.pause;
    let mut placed: Vec<Placed> = Vec::new();
    let mut segment_rects: Vec<(usize, Rect)> = Vec::new();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(6.0)).id_salt("transcript-list"));
    let auto_scroll_key = egui::Id::new("transcript-last-playing");
    let last_playing = child.ctx().data(|d| d.get_temp::<Option<Tok>>(auto_scroll_key)).flatten();
    let playing = playing_word.map(Tok::Word).or(playing_pause.map(Tok::Pause));
    child.ctx().data_mut(|d| d.insert_temp(auto_scroll_key, playing));
    let follow = prefs.auto_scroll && playing.is_some() && playing != last_playing;
    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("transcript-scroll").show(&mut child, |ui| {
        let width = (list.width() - 22.0).max(40.0);
        ui.set_width(width);
        let font = Tokens::ui(WORD_SIZE);
        let space = ui.painter().layout_no_wrap(" ".into(), font.clone(), Color32::PLACEHOLDER).size().x.max(3.0);
        for (pi, para) in paras.iter().enumerate() {
            let (Some(first), Some(last)) = (words.get(para.start), para.end.checked_sub(1).and_then(|e| words.get(e))) else { continue };
            let head = ui.label(egui::RichText::new(format!("{} - {}", tc(first.start), tc(last.end))).font(Tokens::mono(10.5)).color(t.text_faint));
            segment_rects.push((pi, head.rect));
            let origin = ui.cursor().min;
            let mut toks: Vec<(Tok, String)> = Vec::new();
            for i in para.clone() {
                let Some(w) = words.get(i) else { continue };
                toks.push((Tok::Word(i), w.text.clone()));
                if pauses.contains_key(&i) {
                    toks.push((Tok::Pause(i), "[...]".into()));
                }
            }
            let (mut x, mut row) = (0.0f32, 0.0f32);
            let start = placed.len();
            for (tok, text) in toks {
                let galley = ui.painter().layout_no_wrap(text, font.clone(), Color32::PLACEHOLDER);
                let w = galley.size().x;
                if x > 0.0 && x + w > width {
                    x = 0.0;
                    row += 1.0;
                }
                let r = Rect::from_min_size(origin + vec2(x, row * LINE_H), vec2(w, LINE_H));
                placed.push(Placed { tok, rect: r, galley });
                x += w + space;
            }
            let (para_rect, _) = ui.allocate_exact_size(vec2(width, (row + 1.0) * LINE_H), Sense::hover());
            if !ui.is_rect_visible(para_rect) {
                ui.add_space(10.0);
                continue;
            }
            let painter = ui.painter();
            let selected = |tok: Tok| match tok {
                Tok::Word(i) => sel.is_some_and(|(a, b)| a <= i && i <= b),
                Tok::Pause(i) => sel_pause == Some(i) || sel.is_some_and(|(a, b)| a <= i && i < b),
            };
            for (k, p) in placed[start..].iter().enumerate() {
                let mut r = p.rect.expand2(vec2(2.0, -1.5));
                let in_sel = selected(p.tok);
                let filler = matches!(p.tok, Tok::Word(i) if fillers.contains(&i));
                // a selection is one band: up to the next selected token on the same line
                if in_sel
                    && let Some(next) = placed.get(start + k + 1)
                    && selected(next.tok)
                    && next.rect.min.y == p.rect.min.y
                {
                    r.max.x = next.rect.min.x - 2.0;
                }
                let (hit, current) = match p.tok {
                    Tok::Word(i) => (hits.contains(&i), matches!(current_match, Some(Match::Words { from, to }) if from <= i && i <= to)),
                    Tok::Pause(i) => (pause_hits.contains(&i), matches!(current_match, Some(Match::Pause(q)) if q.after == i)),
                };
                if in_sel {
                    painter.rect_filled(r, 0.0, t.accent.gamma_multiply(0.55));
                } else if current {
                    painter.rect_filled(r, 2.0, CURRENT_MATCH_BG);
                } else if hit {
                    painter.rect_filled(r, 2.0, MATCH_BG);
                } else if filler {
                    painter.rect_filled(r, 2.0, t.hover);
                }
                let is_playing = Some(p.tok) == playing;
                let color = match p.tok {
                    Tok::Pause(_) => t.text_faint,
                    _ if in_sel => t.text,
                    _ if is_playing => t.hot_text,
                    Tok::Word(_) if filler => t.text_dim,
                    Tok::Word(_) => t.text,
                };
                let ty = r.center().y - p.galley.size().y / 2.0;
                painter.galley(pos2(p.rect.min.x, ty), p.galley.clone(), color);
                if filler {
                    // a dotted underline marks a filler word
                    let yb = ty + p.galley.size().y - 1.0;
                    let mut xd = p.rect.min.x;
                    while xd < p.rect.max.x {
                        painter.line_segment([pos2(xd, yb), pos2((xd + 1.5).min(p.rect.max.x), yb)], Stroke::new(1.0, t.text_dim));
                        xd += 3.5;
                    }
                }
                if is_playing {
                    painter.line_segment([pos2(p.rect.min.x, r.max.y), pos2(p.rect.max.x, r.max.y)], Stroke::new(2.0, t.accent));
                }
            }
            ui.add_space(10.0);
        }
        // the playhead's word into view while it moves (Enable auto-scrolling), a match after ∧ ∨
        let target = scroll_to.or(if follow { playing } else { None });
        if let Some(target) = target
            && let Some(p) = placed.iter().find(|p| p.tok == target)
            && (scroll_to.is_some() || !ui.clip_rect().contains_rect(p.rect))
        {
            ui.scroll_to_rect(p.rect, Some(egui::Align::Center));
        }
    });
    let clip = scroll.inner_rect;
    // ---- clicks and drags in the text
    // (not over the scroll bar at the right edge)
    let content = Rect::from_min_max(scroll.inner_rect.min, pos2(scroll.inner_rect.max.x - 12.0, scroll.inner_rect.max.y));
    let resp = ui.interact(content, egui::Id::new("transcript-text"), Sense::click_and_drag());
    let hit = |pos: Pos2| -> Option<Tok> {
        if let Some(p) = placed.iter().find(|p| p.rect.expand2(vec2(2.0, 0.0)).contains(pos)) {
            return Some(p.tok);
        }
        // between words: the nearest one on that line
        placed
            .iter()
            .filter(|p| p.rect.y_range().contains(pos.y))
            .min_by(|a, b| a.rect.center().x.sub_abs(pos.x).total_cmp(&b.rect.center().x.sub_abs(pos.x)))
            .map(|p| p.tok)
    };
    let drag_key = egui::Id::new("transcript-drag-anchor");
    if resp.drag_started()
        && let Some(tok) = resp.interact_pointer_pos().and_then(hit)
    {
        ui.ctx().data_mut(|d| d.insert_temp(drag_key, tok.word()));
    }
    if resp.dragged()
        && let (Some(anchor), Some(tok)) = (ui.ctx().data(|d| d.get_temp::<usize>(drag_key)), resp.interact_pointer_pos().and_then(hit))
    {
        app.ui.transcript_sel = Some((anchor, tok.word()));
        app.ui.transcript.pause = None;
    }
    if resp.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove::<usize>(drag_key));
        if let Some((a, b)) = app.ui.transcript_sel {
            select_words(app, &words, a, b, &mut actions);
        }
    } else if resp.triple_clicked() {
        // the whole segment
        if let Some(tok) = resp.interact_pointer_pos().and_then(hit)
            && let Some(p) = paras.iter().find(|p| p.contains(&tok.word()))
        {
            select_words(app, &words, p.start, p.end.saturating_sub(1), &mut actions);
        }
    } else if resp.double_clicked() {
        if let Some(Tok::Word(i)) = resp.interact_pointer_pos().and_then(hit) {
            select_words(app, &words, i, i, &mut actions);
        }
    } else if resp.clicked()
        && let Some(tok) = resp.interact_pointer_pos().and_then(hit)
    {
        let shift = crate::panels::project_views::click_modifiers(ui).shift;
        match tok {
            Tok::Pause(i) => select_pause(app, &words, i, &mut actions),
            Tok::Word(i) if shift => {
                let anchor = app.ui.transcript_sel.map(|(a, _)| a).or(app.ui.transcript.caret).unwrap_or(i);
                select_words(app, &words, anchor, i, &mut actions);
            }
            Tok::Word(i) => place_caret(app, &words, i, &mut actions),
        }
    }
    if resp.hovered()
        && let Some(Tok::Pause(i)) = resp.hover_pos().and_then(hit)
        && let Some(p) = pauses.get(&i)
    {
        resp.clone().on_hover_text(tlf!("{seconds} seconds", seconds = format!("{:.1}", p.duration().seconds())));
    }
    // automation: the visible words and pauses, the segment headers
    for p in &placed {
        if !clip.intersects(p.rect) {
            continue;
        }
        match p.tok {
            Tok::Word(i) => app.auto.add(&format!("text.transcript.word.{i}"), p.rect, words.get(i).map_or("", |w| w.text.as_str())),
            Tok::Pause(i) => app.auto.add(&format!("text.transcript.pause.{i}"), p.rect, "[...]"),
        }
    }
    for (pi, r) in segment_rects {
        if clip.intersects(r) {
            app.auto.add(&format!("text.transcript.segment.{pi}"), r, "Segment");
        }
    }
    run(app, ui, actions);
}

/// The word the playhead is in: the one sounding during the playhead's frame (a click puts the
/// playhead on the frame where the word starts, which may begin a little before it); between two
/// words, when that silence is no pause shown, the word just spoken.
fn playing_word(words: &[SeqWord], ph: Tick, frame: Tick, in_pause: bool) -> Option<usize> {
    let k = words.partition_point(|w| w.start < ph + frame);
    let i = k.checked_sub(1)?;
    let w = words.get(i)?;
    if w.end > ph || (!in_pause && words.get(i + 1).is_some_and(|n| n.start > ph)) { Some(i) } else { None }
}

trait SubAbs {
    fn sub_abs(self, o: f32) -> f32;
}
impl SubAbs for f32 {
    fn sub_abs(self, o: f32) -> f32 {
        (self - o).abs()
    }
}

/// Select words `a..=b`: In/Out around them when "Automatically set In/Out points" is on, else
/// the playhead goes to the first.
fn select_words(app: &mut FilmcraftApp, words: &[SeqWord], a: usize, b: usize, actions: &mut Vec<(String, Value)>) {
    app.ui.transcript_sel = Some((a, b));
    app.ui.transcript.pause = None;
    app.ui.transcript.caret = Some(b);
    if app.session.prefs.transcript.auto_in_out {
        actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
    } else if let Some(w) = words.get(a.min(b)) {
        app.session.set_playhead(w.start);
    }
}

fn select_pause(app: &mut FilmcraftApp, words: &[SeqWord], after: usize, actions: &mut Vec<(String, Value)>) {
    if !app.session.prefs.transcript.auto_in_out {
        clear_selection_marks(app, words, actions);
    }
    app.ui.transcript_sel = None;
    app.ui.transcript.pause = Some(after);
    app.ui.transcript.caret = Some(after);
    if app.session.prefs.transcript.auto_in_out {
        actions.push(("transcript.select".into(), json!({"pauseAfter": after})));
    } else if let Some(p) = tx::pause_after(words, after) {
        app.session.set_playhead(p.start);
    }
}

/// A plain click: the playhead goes to the word; a text selection (and the In/Out it set) is cleared.
fn place_caret(app: &mut FilmcraftApp, words: &[SeqWord], i: usize, actions: &mut Vec<(String, Value)>) {
    clear_selection_marks(app, words, actions);
    app.ui.transcript_sel = None;
    app.ui.transcript.pause = None;
    app.ui.transcript.caret = Some(i);
    if let Some(w) = words.get(i) {
        app.session.set_playhead(w.start);
    }
}

/// Clear In/Out when they are still the ones the current text (or pause) selection set.
fn clear_selection_marks(app: &mut FilmcraftApp, words: &[SeqWord], actions: &mut Vec<(String, Value)>) {
    let rate = app.session.sequence_rate();
    let range = match (app.ui.transcript_sel, app.ui.transcript.pause) {
        (Some((a, b)), _) => tx::word_range(words, a, b, rate),
        (None, Some(p)) => tx::pause_after(words, p).and_then(|p| tx::pause_range(&p, Tick::ZERO, rate)),
        _ => None,
    };
    let Some(r) = range else { return };
    let Some(q) = app.session.active_sequence() else { return };
    if q.mark_in == Some(r.start) && q.mark_out == Some(r.end() - rate.frame_duration()) {
        actions.push(("markers.clearInOut".into(), json!({})));
    }
}

/// The command params of the text selection or the selected pause, if any.
pub fn selection_params(app: &FilmcraftApp) -> Option<Value> {
    if let Some((a, b)) = app.ui.transcript_sel {
        return Some(json!({"from": a.min(b), "to": a.max(b)}));
    }
    app.ui.transcript.pause.map(|p| json!({"pauseAfter": p}))
}

fn small_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon: Icon, id: &str, label: &str, enabled: bool, active: bool) -> egui::Response {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::new(("transcript-tool", id)), if enabled { Sense::click() } else { Sense::hover() });
    if active {
        ui.painter().rect_filled(r, 3.0, t.pill_active_bg);
    } else if enabled && resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    icons::paint(
        ui.painter(),
        r.shrink(5.0),
        icon,
        if !enabled {
            t.text_faint
        } else if active {
            t.icon_active
        } else {
            t.icon
        },
    );
    app.auto.add(id, r, label);
    resp.on_hover_text(label)
}

fn toolbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let filter = app.ui.transcript.filter;
    // the search field (with a filter on, it names the filter and searches nothing)
    let sw = 220.0f32.min(bar.width() - 5.0 * 28.0).max(80.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(sw, 24.0))));
    let hint = match filter {
        Filter::Text => tl!("Search"),
        Filter::Fillers => tl!("Filler words"),
        Filter::Pauses => tl!("Pauses"),
    };
    let sresp = if filter == Filter::Text {
        crate::widgets::search_field(&mut child, &mut app.ui.transcript_search, hint, sw, &t)
    } else {
        let mut empty = String::new();
        crate::widgets::search_field(&mut child, &mut empty, hint, sw, &t)
    };
    app.auto.add("text.transcript.search", sresp.rect, "Search transcript");
    if sresp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("transcript-next"), true));
    }
    let mut x = bar.min.x + sw + 6.0;
    let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
    let fresp = small_button(app, ui, r, Icon::Filter, "text.transcript.filter", tl!("Filter"), true, false);
    if filter != Filter::Text {
        ui.painter().circle_filled(r.right_top() + vec2(-5.0, 5.0), 3.0, t.accent);
    }
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    egui::Popup::menu(&fresp).show(|ui| {
        for (f, id, label) in
            [(Filter::Text, "text", tl!("Text")), (Filter::Fillers, "fillers", tl!("Filler words")), (Filter::Pauses, "pauses", tl!("Pauses"))]
        {
            let r = ui.selectable_label(filter == f, label);
            elems.push((format!("text.transcript.filter.{id}"), r.rect, label.into()));
            if r.clicked() {
                app.ui.transcript.filter = f;
                app.ui.transcript.delete_open = false;
            }
        }
        ui.separator();
        let r = ui.button(tl!("Search settings…"));
        elems.push(("text.transcript.filter.searchSettings".into(), r.rect, "Search settings…".into()));
        if r.clicked() {
            app.ui.transcript.view_options = Some(app.session.prefs.transcript.clone());
        }
    });
    x += 28.0;
    // { }: Automatically set In/Out points
    let on = app.session.prefs.transcript.auto_in_out;
    let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(26.0, 24.0));
    let resp = ui.interact(r, egui::Id::new("transcript-auto-in-out"), Sense::click());
    if on {
        ui.painter().rect_filled(r, 3.0, t.pill_active_bg);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    ui.painter().text(r.center(), Align2::CENTER_CENTER, "{ }", Tokens::semibold(12.0), if on { t.icon_active } else { t.icon });
    app.auto.add("text.transcript.autoInOut", r, "Automatically set In/Out points");
    if resp.on_hover_text(tl!("Automatically set In/Out points")).clicked() {
        actions.push(("prefs.set".into(), json!({"key": "transcript.autoInOut", "value": !on})));
    }
    x += 30.0;
    let sel = selection_params(app);
    for (icon, id, label, cmd) in
        [(Icon::Extract, "text.transcript.extract", tl!("Extract"), "transcript.extract"), (Icon::Lift, "text.transcript.lift", tl!("Lift"), "transcript.lift")]
    {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if r.max.x > bar.max.x - 28.0 {
            break;
        }
        if small_button(app, ui, r, icon, id, label, sel.is_some(), false).clicked()
            && let Some(p) = sel.clone()
        {
            app.ui.transcript_sel = None;
            app.ui.transcript.pause = None;
            actions.push((cmd.into(), p));
        }
        x += 28.0;
    }
    // •••
    let r = Rect::from_min_size(pos2(bar.max.x - 24.0, bar.min.y), vec2(24.0, 24.0));
    let mresp = small_button(app, ui, r, Icon::More, "text.transcript.more", tl!("More options"), true, false);
    egui::Popup::menu(&mresp).align(egui::RectAlign::BOTTOM_END).show(|ui| {
        ui.label(egui::RichText::new(tl!("ACTIONS")).size(10.0).color(t.text_faint));
        let r = ui.button(tl!("Create captions"));
        elems.push(("text.transcript.more.createCaptions".into(), r.rect, "Create captions".into()));
        if r.clicked() {
            actions.push(("transcript.createCaptions".into(), json!({})));
        }
        let r = ui.button(tl!("Transcribe sequence…"));
        elems.push(("text.transcript.more.transcribe".into(), r.rect, "Transcribe sequence…".into()));
        if r.clicked() {
            actions.push(("sequence.transcribe".into(), json!({})));
        }
        ui.separator();
        ui.label(egui::RichText::new(tl!("PREFERENCES")).size(10.0).color(t.text_faint));
        let r = ui.button(tl!("Transcript view options…"));
        elems.push(("text.transcript.more.viewOptions".into(), r.rect, "Transcript view options…".into()));
        if r.clicked() {
            app.ui.transcript.view_options = Some(app.session.prefs.transcript.clone());
        }
        let mut scroll = app.session.prefs.transcript.auto_scroll;
        let r = ui.checkbox(&mut scroll, tl!("Enable auto-scrolling"));
        elems.push(("text.transcript.more.autoScroll".into(), r.rect, "Enable auto-scrolling".into()));
        if r.changed() {
            actions.push(("prefs.set".into(), json!({"key": "transcript.autoScroll", "value": scroll})));
        }
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
}

/// The results row: Delete, "1/33 results", ∧ ∨. Returns the match to scroll to after ∧ ∨.
fn results_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, words: &[SeqWord], found: &[Match], actions: &mut Vec<(String, Value)>) -> Option<Tok> {
    let t = app.tokens;
    let n = found.len();
    let r = Rect::from_min_size(row.min, vec2(24.0, 24.0));
    if small_button(app, ui, r, Icon::Trash, "text.transcript.delete", tl!("Delete"), n > 0, app.ui.transcript.delete_open).clicked() {
        app.ui.transcript.delete_open = !app.ui.transcript.delete_open;
    }
    let cur = app.ui.transcript.current;
    let count = if n == 0 { tl!("no results").to_string() } else { tlf!("{n}/{total} results", n = (cur + 1).to_string(), total = n.to_string()) };
    let tr = ui.painter().text(pos2(row.min.x + 32.0, row.center().y), Align2::LEFT_CENTER, &count, Tokens::ui(12.0), t.text_dim);
    app.auto.add("text.transcript.count", tr, &count);
    let mut step: i64 = 0;
    let prev = Rect::from_min_size(pos2(row.max.x - 52.0, row.min.y), vec2(24.0, 24.0));
    if small_button(app, ui, prev, Icon::ChevronUp, "text.transcript.prev", tl!("Previous result"), n > 0, false).clicked() {
        step = -1;
    }
    let next = Rect::from_min_size(pos2(row.max.x - 24.0, row.min.y), vec2(24.0, 24.0));
    if small_button(app, ui, next, Icon::ChevronDown, "text.transcript.next", tl!("Next result"), n > 0, false).clicked() {
        step = 1;
    }
    if ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new("transcript-next"))).unwrap_or(false) {
        step = 1;
    }
    if step == 0 || n == 0 {
        return None;
    }
    let i = (cur as i64 + step).rem_euclid(n as i64) as usize;
    app.ui.transcript.current = i;
    let m = found.get(i)?;
    Some(match *m {
        Match::Words { from, to } => {
            select_words(app, words, from, to, actions);
            Tok::Word(from)
        }
        Match::Pause(p) => {
            select_pause(app, words, p.after, actions);
            Tok::Pause(p.after)
        }
    })
}

/// The Delete row: Extract / Lift, "Delete all", "Delete" (the current match).
fn delete_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, words: &[SeqWord], found: &[Match], query: &str, actions: &mut Vec<(String, Value)>) {
    let filter = app.ui.transcript.filter;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row).id_salt("transcript-delete-row"));
    child.horizontal_centered(|ui| {
        for (lift, id, label) in [(false, "extract", tl!("Extract")), (true, "lift", tl!("Lift"))] {
            let r = ui.radio(app.ui.transcript.lift == lift, label);
            elems.push((format!("text.transcript.deleteMode.{id}"), r.rect, label.into()));
            if r.clicked() {
                app.ui.transcript.lift = lift;
            }
        }
        ui.add_space(8.0);
        let any = !found.is_empty();
        let r = ui.add_enabled(any, egui::Button::new(tl!("Delete all")));
        elems.push(("text.transcript.deleteAll".into(), r.rect, "Delete all".into()));
        if r.clicked() {
            let mut p = json!({"filter": filter.name(), "lift": app.ui.transcript.lift});
            if filter == Filter::Text {
                p["query"] = json!(query);
            }
            actions.push(("transcript.deleteAll".into(), p));
            app.ui.transcript_sel = None;
            app.ui.transcript.pause = None;
        }
        let r = ui.add_enabled(any, egui::Button::new(tl!("Delete")));
        elems.push(("text.transcript.deleteOne".into(), r.rect, "Delete".into()));
        if r.clicked()
            && let Some(m) = found.get(app.ui.transcript.current)
        {
            let p = match m {
                Match::Words { from, to } => json!({"from": from, "to": to}),
                Match::Pause(p) => json!({"pauseAfter": p.after}),
            };
            actions.push((if app.ui.transcript.lift { "transcript.lift" } else { "transcript.extract" }.into(), p));
            app.ui.transcript_sel = None;
            app.ui.transcript.pause = None;
        }
    });
    let _ = words;
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
}

/// What the job is doing, for the progress row.
fn job_verb(j: &filmcraft_engine::Job) -> &'static str {
    let status = j.progress.status.lock().map(|s| s.clone()).unwrap_or_default();
    if status.starts_with("Downloading") {
        tl!("Downloading speech model…")
    } else if status.starts_with("Loading") {
        tl!("Loading speech model…")
    } else {
        tl!("Transcribing…")
    }
}

/// "Transcribing… 42 %", the bar and Cancel.
fn progress_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, j: &filmcraft_engine::Job, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let f = j.progress.fraction().clamp(0.0, 1.0);
    let text = format!("{} {:.0} %", job_verb(j), f * 100.0);
    let cancel = Rect::from_min_size(pos2(row.max.x - 70.0, row.min.y + 1.0), vec2(70.0, 22.0));
    let label_w = 190.0f32.min(row.width() * 0.45);
    ui.painter().text(pos2(row.min.x, row.center().y), Align2::LEFT_CENTER, &text, Tokens::ui(12.0), t.text);
    let bar = Rect::from_min_max(pos2(row.min.x + label_w, row.center().y - 3.0), pos2(cancel.min.x - 10.0, row.center().y + 3.0));
    if bar.width() > 10.0 {
        ui.painter().rect_filled(bar, 3.0, t.field_bg);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, bar.height())), 3.0, t.accent);
    }
    app.auto.add("text.transcript.progress", row, &text);
    let resp = ui.interact(cancel, egui::Id::new("transcript-cancel"), Sense::click());
    ui.painter().rect_filled(cancel, 11.0, if resp.hovered() { t.hover } else { t.field_bg });
    ui.painter().text(cancel.center(), Align2::CENTER_CENTER, tl!("Cancel"), Tokens::semibold(11.5), t.text);
    app.auto.add("text.transcript.cancel", cancel, "Cancel");
    if resp.clicked() {
        actions.push(("transcript.cancel".into(), json!({})));
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
}

/// "Interview.wav is untranscribed" / "3 clips are untranscribed" when audio clips of the sequence
/// have no transcript yet.
fn untranscribed(app: &FilmcraftApp) -> Option<String> {
    let q = app.session.active_sequence()?;
    let p = &app.session.project;
    let mut missing: Vec<filmcraft_engine::project::ItemId> = Vec::new();
    for it in q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.enabled) {
        let Some((media, _, _)) = p.resolve_media(it.item) else { continue };
        if !p.transcripts.contains_key(&media) && !missing.contains(&media) && p.item(media).is_some_and(|i| i.has_audio()) {
            missing.push(media);
        }
    }
    match missing.as_slice() {
        [] => None,
        [one] => Some(tlf!("{name} is untranscribed", name = p.item(*one).map(|i| i.name.clone()).unwrap_or_default())),
        many => Some(tlf!("{n} clips are untranscribed", n = many.len().to_string())),
    }
}

fn banner(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, text: &str, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    ui.painter().rect_filled(row, 3.0, t.row_alt);
    ui.painter().text(pos2(row.min.x + 8.0, row.center().y), Align2::LEFT_CENTER, format!("› {text}"), Tokens::ui(12.0), t.text_dim);
    let b = Rect::from_min_size(pos2(row.max.x - 92.0, row.min.y + 2.0), vec2(88.0, 20.0));
    let resp = ui.interact(b, egui::Id::new("transcript-banner-transcribe"), Sense::click());
    ui.painter().rect_filled(b, 10.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(b.center(), Align2::CENTER_CENTER, tl!("Transcribe"), Tokens::semibold(11.5), Color32::WHITE);
    app.auto.add("text.transcript.banner.transcribe", b, "Transcribe");
    if resp.clicked() {
        actions.push(("sequence.transcribe".into(), json!({})));
    }
}

fn empty_state(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, job: Option<&filmcraft_engine::Job>, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let c = rect.center();
    icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
    if let Some(j) = job {
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, tl!("Transcribing source clips"), Tokens::semibold(16.0), t.text);
        let row = Rect::from_center_size(c + vec2(0.0, 6.0), vec2((rect.width() - 40.0).clamp(120.0, 460.0), 24.0));
        progress_row(app, ui, row, j, actions);
        return;
    }
    ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, tl!("Transcribe source clips"), Tokens::semibold(16.0), t.text);
    let note = if app.session.transcriber.is_some() || filmcraft_engine::transcript::speech_available() {
        tl!("Transcribe your source clips to view your sequence transcript.")
    } else {
        tl!("This build has no speech-to-text; import a transcript with transcript.set.")
    };
    ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, note, Tokens::ui(12.0), t.text_dim);
    let r = Rect::from_center_size(c + vec2(0.0, 26.0), vec2(160.0, 26.0));
    let resp = ui.interact(r, egui::Id::new("text.transcript.generate"), Sense::click());
    ui.painter().rect_filled(r, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("Transcribe"), Tokens::semibold(12.0), Color32::WHITE);
    app.auto.add("text.transcript.generate", r, "Transcribe");
    if resp.clicked() {
        actions.push(("sequence.transcribe".into(), json!({})));
    }
}

/// Transcript View Options: Transcript view (Filler words, Pauses, Minimum pause length) and
/// Search settings; Save sets the preferences.
fn view_options_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.transcript.view_options.clone() else { return };
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut action = None;
    egui::Window::new(tl!("Transcript View Options"))
        .id(egui::Id::new("transcript-view-options"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(tl!("Transcript view")).strong());
            let r = ui.checkbox(&mut d.filler_words, tl!("Filler words"));
            elems.push(("transcriptViewOptions.fillerWords".into(), r.rect, "Filler words".into()));
            let r = ui.checkbox(&mut d.pauses, tl!("Pauses"));
            elems.push(("transcriptViewOptions.pauses".into(), r.rect, "Pauses".into()));
            ui.horizontal(|ui| {
                ui.add_enabled_ui(d.pauses, |ui| {
                    ui.label(tl!("Minimum pause length"));
                    use filmcraft_engine::transcript::{MAX_PAUSE_SECONDS, MIN_PAUSE_SECONDS};
                    ui.add(egui::Slider::new(&mut d.min_pause_length, MIN_PAUSE_SECONDS..=MAX_PAUSE_SECONDS).show_value(false));
                    let r = ui.add(egui::DragValue::new(&mut d.min_pause_length).range(MIN_PAUSE_SECONDS..=MAX_PAUSE_SECONDS).speed(0.01).max_decimals(2));
                    elems.push(("transcriptViewOptions.minPauseLength".into(), r.rect, "Minimum pause length".into()));
                    ui.label(tl!("seconds"));
                });
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tl!("Search settings")).strong());
            let r = ui.checkbox(&mut d.whole_words, tl!("Find whole words only"));
            elems.push(("transcriptViewOptions.wholeWords".into(), r.rect, "Find whole words only".into()));
            let r = ui.checkbox(&mut d.match_case, tl!("Match capitalization"));
            elems.push(("transcriptViewOptions.matchCase".into(), r.rect, "Match capitalization".into()));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                elems.push(("transcriptViewOptions.cancel".into(), r.rect, "Cancel".into()));
                if r.clicked() {
                    action = Some(false);
                }
                let r = ui.button(tl!("Save"));
                elems.push(("transcriptViewOptions.save".into(), r.rect, "Save".into()));
                if r.clicked() {
                    action = Some(true);
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(false);
    }
    match action {
        Some(true) => {
            app.ui.transcript.view_options = None;
            let v = json!({
                "transcript.fillerWords": d.filler_words, "transcript.pauses": d.pauses, "transcript.minPauseLength": d.min_pause_length,
                "transcript.wholeWords": d.whole_words, "transcript.matchCase": d.match_case,
            });
            if let Err(e) = app.session.execute("prefs.set", json!({"values": v})) {
                app.ui.status = e.to_string();
            }
        }
        Some(false) => app.ui.transcript.view_options = None,
        None => app.ui.transcript.view_options = Some(d),
    }
}

fn run(app: &mut FilmcraftApp, ui: &egui::Ui, actions: Vec<(String, Value)>) {
    let ctx = ui.ctx().clone();
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, &ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}
