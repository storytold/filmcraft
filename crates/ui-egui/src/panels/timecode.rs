//! Timecode panel: the time of the active monitor in large type, plus optional smaller rows. Each
//! row shows Current Time, Media Time (the source timecode of the clip under the playhead),
//! Duration, In/Out Duration or Remaining, of the active monitor, the Program or the Source, in
//! any time display (timecode, frames, feet + frames, samples, seconds). Right-click a row to
//! change it; "+" adds a row.
//!
//! Automation ids: `timecode.row.<i>` (the value), `timecode.addRow`, and in a row's menu
//! `timecode.row.<i>.mode.<mode>`, `.source.<source>`, `.display.<n>`, `.remove`.
//! State: `ui.panels.timecode`.

use egui::{Align2, Rect, Sense, pos2, vec2};
use filmcraft_project::ItemKind;
use filmcraft_time::{FrameRate, Tick, TimeDisplay, format_time};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::panels::panel_state::{TcMode, TcSource, TimecodeRow};
use crate::theme::Tokens;

/// What a row shows: (value text, caption).
pub fn row_text(app: &FilmcraftApp, row: &TimecodeRow) -> (String, String) {
    let s = &app.session;
    let source = match row.source {
        TcSource::Active if matches!(app.ui.focused, PanelKind::Source) && s.state.source_item.is_some() => TcSource::Source,
        TcSource::Active => TcSource::Program,
        x => x,
    };
    struct View {
        name: String,
        rate: FrameRate,
        df: bool,
        t: Tick,
        dur: Tick,
        marks: (Option<Tick>, Option<Tick>),
        /// The media timecode at `t` (None = no clip there).
        media: Option<(Tick, FrameRate)>,
        sample_rate: i64,
    }
    let view = match source {
        TcSource::Source => s.state.source_item.and_then(|i| s.project.item(i)).map(|it| {
            let start = it.as_media().and_then(|m| m.info.start_timecode).map(|f| it.frame_rate().tick_of(f)).unwrap_or_default();
            let marks = match &it.kind {
                ItemKind::Media(m) => (m.mark_in, m.mark_out),
                ItemKind::Sequence(q) => (q.mark_in, q.mark_out),
                _ => (None, None),
            };
            let sr = it.as_media().and_then(|m| m.info.audio()).map(|a| a.sample_rate as i64).unwrap_or(48000);
            View {
                name: it.name.clone(),
                rate: it.frame_rate(),
                df: false,
                t: s.state.source_playhead,
                dur: it.duration(),
                marks,
                media: Some((start + s.state.source_playhead, it.frame_rate())),
                sample_rate: sr,
            }
        }),
        _ => s.active_sequence().map(|q| {
            let t = s.playhead();
            // the topmost enabled video clip under the playhead
            let media = q.video_tracks.iter().rev().find_map(|tr| tr.item_at(t).filter(|c| c.enabled)).and_then(|c| {
                let it = s.project.item(c.item)?;
                let m = it.as_media()?;
                let rate = m.frame_rate();
                let start = m.info.start_timecode.map(|f| rate.tick_of(f)).unwrap_or_default();
                Some((start + c.source_time_at(t), rate))
            });
            View {
                name: s.state.active_sequence.and_then(|i| s.project.item(i)).map(|i| i.name.clone()).unwrap_or_default(),
                rate: q.settings.frame_rate,
                df: q.settings.drop_frame,
                t,
                dur: q.duration(),
                marks: (q.mark_in, q.mark_out),
                media,
                sample_rate: q.settings.sample_rate as i64,
            }
        }),
    };
    let Some(v) = view else {
        let mode = crate::i18n::t(row.mode.label());
        return ("--:--:--:--".into(), if source == TcSource::Source { tlf!("{mode} · no clip", mode) } else { tlf!("{mode} · no sequence", mode) });
    };
    let fmt = |t: Tick, rate: FrameRate, df: bool| format_time(t, rate, df, row.display, v.sample_rate);
    let (a, b) = (v.marks.0.unwrap_or(Tick::ZERO), v.marks.1.unwrap_or(v.dur));
    let value = match row.mode {
        TcMode::Current => fmt(v.t, v.rate, v.df),
        TcMode::Media => match v.media {
            Some((t, r)) => fmt(t, r, false),
            None => "--:--:--:--".into(),
        },
        TcMode::Duration => fmt(v.dur, v.rate, v.df),
        TcMode::InOut => fmt((b - a).max(Tick::ZERO), v.rate, v.df),
        TcMode::Remaining => fmt((b - v.t).max(Tick::ZERO), v.rate, v.df),
    };
    let src = match source {
        TcSource::Source => tl!("Source"),
        _ => tl!("Program"),
    };
    (value, format!("{src} · {} · {}", crate::i18n::t(row.mode.label()), v.name))
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let rows = app.ui.panels.timecode.rows.clone();
    let show_name = app.ui.panels.timecode.show_name;
    let mut y = rect.min.y + 10.0;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut changes: Vec<(usize, Option<TimecodeRow>)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let big = i == 0;
        let (value, caption) = row_text(app, row);
        let size = if big { (rect.height() * 0.32).clamp(22.0, 64.0).min(rect.width() / 7.0) } else { 20.0 };
        let h = size * 1.25 + if show_name { 16.0 } else { 4.0 };
        let r = Rect::from_min_size(pos2(rect.min.x + 8.0, y), vec2(rect.width() - 16.0, h));
        if r.max.y > rect.max.y - 24.0 && i > 0 {
            break;
        }
        let vr = ui.painter().text(pos2(r.center().x, r.min.y), Align2::CENTER_TOP, &value, Tokens::mono(size), if big { t.timecode } else { t.text });
        if show_name {
            ui.painter().text(pos2(r.center().x, vr.max.y + 2.0), Align2::CENTER_TOP, &caption, Tokens::ui(11.0), t.text_dim);
        }
        let resp = ui.interact(r, egui::Id::new(("timecode-row", i)), Sense::click());
        elems.push((format!("timecode.row.{i}"), vr, value.clone()));
        egui::Popup::context_menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            let mut nr = *row;
            let mut picks: Vec<(String, Rect, String, bool)> = Vec::new();
            let mut pick = |ui: &mut egui::Ui, id: String, label: &str, on: bool| {
                let r = ui.selectable_label(on, label);
                picks.push((id, r.rect, label.to_string(), r.clicked()));
                r.clicked()
            };
            for m in TcMode::ALL {
                if pick(ui, format!("timecode.row.{i}.mode.{}", serde_name(&m)), crate::i18n::t(m.label()), row.mode == m) {
                    nr.mode = m;
                }
            }
            ui.separator();
            for src in TcSource::ALL {
                if pick(ui, format!("timecode.row.{i}.source.{}", serde_name(&src)), crate::i18n::t(src.label()), row.source == src) {
                    nr.source = src;
                }
            }
            ui.separator();
            for (n, d) in TimeDisplay::ALL.into_iter().enumerate() {
                if pick(ui, format!("timecode.row.{i}.display.{n}"), crate::i18n::t(d.label()), row.display == d) {
                    nr.display = d;
                }
            }
            let mut remove = false;
            if rows.len() > 1 {
                ui.separator();
                remove = pick(ui, format!("timecode.row.{i}.remove"), tl!("Remove Row"), false);
            }
            for (id, r, l, _) in &picks {
                elems.push((id.clone(), *r, l.clone()));
            }
            if remove {
                changes.push((i, None));
                ui.close();
            } else if nr != *row {
                changes.push((i, Some(nr)));
                ui.close();
            }
        });
        y = r.max.y + 8.0;
    }
    // "+" adds a row (same settings as the last one, Duration by default)
    let ar = Rect::from_min_size(pos2(rect.max.x - 28.0, rect.max.y - 26.0), vec2(22.0, 22.0));
    let aresp = ui.interact(ar, egui::Id::new("timecode-add"), Sense::click());
    ui.painter().text(ar.center(), Align2::CENTER_CENTER, "+", Tokens::ui(16.0), if aresp.hovered() { t.tab_text_active } else { t.icon });
    elems.push(("timecode.addRow".into(), ar, "Add Row".into()));
    if aresp.on_hover_text(tl!("Add a timecode row")).clicked() {
        app.ui.panels.timecode.rows.push(TimecodeRow { mode: TcMode::Duration, ..rows.last().copied().unwrap_or_default() });
    }
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    for (i, c) in changes {
        let rows = &mut app.ui.panels.timecode.rows;
        match c {
            Some(r) if i < rows.len() => rows[i] = r,
            None if i < rows.len() && rows.len() > 1 => {
                rows.remove(i);
            }
            _ => {}
        }
    }
}

fn serde_name<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}
