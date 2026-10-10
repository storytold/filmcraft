//! CMX 3600 edit decision lists.
//!
//! Supported: `TITLE:`, `FCM:` (drop/non-drop, may change mid-list), cut / dissolve (`D nnn`) /
//! wipe (`Wnnn nnn`) / key (`K B`, `K`, `K O`) events, audio channels (`V`, `A`, `A2`, `AA`, `B`,
//! `AA/V`, `A2/V`, `A3`, `A4`, `AUD 3 4` lines), `M2` motion (speed, reverse, freeze) lines, `BL`
//! black, reel names up to 8 (strict) or 32+ (extended) characters, and the comment extensions
//! `* FROM CLIP NAME:`, `* TO CLIP NAME:`, `* SOURCE FILE:`, `* EFFECT NAME:` and `* LOC:` markers.
//!
//! EDLs carry no frame rate and no media identity beyond reels/comments; see [`ImportOptions`].
//! Source timecodes are imported as media time (a file starting at 01:00:00:00 would need
//! [`crate::rebase_source_timecode`] after probing).

use std::collections::{BTreeSet, HashMap};

use filmcraft_media::Generator;
use filmcraft_project::{
    ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, ParamValue, Project, Sequence, TrackKind, Transition, TransitionAlign, TransitionId, find_effect,
};
use filmcraft_time::{FrameRate, Tick, fields_to_frames, format_timecode_frames};

use crate::common::{
    Builder, MediaSpec, empty_sequence, file_name, file_stem, frames_round, generator_of, item_path, on_frame, overwrite, relative_path, resolve_path,
    sanitize_reel, scaled, settings_for, transition_effect, transition_name,
};
use crate::{ExportOptions, Format, ImportOptions, Imported, ReelMode, Report, Result};

/// Heuristic: does this text look like an EDL?
pub(crate) fn looks_like_edl(text: &str) -> bool {
    text.lines().take(40).any(|l| {
        let l = l.trim_start();
        l.starts_with("TITLE:") || l.starts_with("FCM:") || parse_event_line(l).is_some()
    })
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tc {
    h: i64,
    m: i64,
    s: i64,
    f: i64,
    df: bool,
}

fn parse_tc(s: &str) -> Option<Tc> {
    let parts: Vec<&str> = s.split([':', ';', '.', ',']).collect();
    if parts.len() != 4 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let n: Vec<i64> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
    Some(Tc { h: n[0], m: n[1], s: n[2], f: n[3], df: s.contains(';') || s.contains(',') })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trans {
    Cut,
    Dissolve,
    Wipe(u32),
    KeyBackground,
    Key,
    KeyOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Ch {
    V,
    A(usize),
}

fn parse_channels(s: &str) -> Option<Vec<Ch>> {
    let u = s.to_ascii_uppercase();
    match u.as_str() {
        "B" => return Some(vec![Ch::V, Ch::A(1)]),
        "NONE" => return Some(vec![]),
        _ => {}
    }
    let mut out = Vec::new();
    for part in u.split('/') {
        match part {
            "V" => out.push(Ch::V),
            "A" | "A1" => out.push(Ch::A(1)),
            "AA" => out.extend([Ch::A(1), Ch::A(2)]),
            p if p.starts_with('A') && p[1..].parse::<usize>().is_ok_and(|n| (1..=99).contains(&n)) => out.push(Ch::A(p[1..].parse().ok()?)),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Clone, Debug)]
struct Line {
    num: String,
    reel: String,
    chans: Vec<Ch>,
    trans: Trans,
    dur: i64,
    tcs: [Tc; 4],
}

fn parse_event_line(l: &str) -> Option<Line> {
    let toks: Vec<&str> = l.split_whitespace().collect();
    if toks.len() < 8 || !toks[0].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = toks.len();
    let tcs = [parse_tc(toks[n - 4])?, parse_tc(toks[n - 3])?, parse_tc(toks[n - 2])?, parse_tc(toks[n - 1])?];
    let reel = toks[1].to_string();
    let chans = parse_channels(toks[2])?;
    let mid = &toks[3..n - 4];
    let first = mid.first()?.to_ascii_uppercase();
    let num_at = |i: usize| mid.get(i).and_then(|t| t.parse::<i64>().ok());
    let (trans, dur) = match first.as_str() {
        "C" => (Trans::Cut, 0),
        "D" => (Trans::Dissolve, num_at(1).unwrap_or(0)),
        "K" => match mid.get(1).map(|s| s.to_ascii_uppercase()) {
            Some(ref s) if s == "B" => (Trans::KeyBackground, 0),
            Some(ref s) if s == "O" => (Trans::KeyOut, num_at(2).unwrap_or(0)),
            _ => (Trans::Key, num_at(1).unwrap_or(0)),
        },
        "KB" => (Trans::KeyBackground, 0),
        "KO" => (Trans::KeyOut, num_at(1).unwrap_or(0)),
        w if w.starts_with('W') => (Trans::Wipe(w[1..].parse().unwrap_or(1)), num_at(1).unwrap_or(0)),
        _ => return None,
    };
    Some(Line { num: toks[0].to_string(), reel, chans, trans, dur, tcs })
}

#[derive(Clone, Debug, Default)]
struct Event {
    lines: Vec<Line>,
    from_clip: Option<String>,
    to_clip: Option<String>,
    files: Vec<String>,
    effect: Option<String>,
    /// (reel, fps, entry tc)
    m2: Vec<(String, f64)>,
    locs: Vec<(Tc, String, String)>,
    /// FCM in force for this event.
    df: bool,
}

struct Parsed {
    title: Option<String>,
    events: Vec<Event>,
    any_df: bool,
    max_ff: i64,
}

fn parse(text: &str, report: &mut Report) -> Parsed {
    let mut p = Parsed { title: None, events: Vec::new(), any_df: false, max_ff: 0 };
    let mut df = false;
    for raw in text.lines() {
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let upper = l.to_ascii_uppercase();
        if let Some(t) = l.strip_prefix("TITLE:") {
            p.title = Some(t.trim().to_string());
            continue;
        }
        if upper.starts_with("FCM:") {
            df = upper.contains("DROP") && !upper.contains("NON");
            p.any_df |= df;
            continue;
        }
        if let Some(c) = l.strip_prefix('*') {
            let c = c.trim();
            let Some(ev) = p.events.last_mut() else { continue };
            let cu = c.to_ascii_uppercase();
            let value = |prefix: &str| c[prefix.len()..].trim().to_string();
            if cu.starts_with("FROM CLIP NAME:") {
                ev.from_clip = Some(value("FROM CLIP NAME:"));
            } else if cu.starts_with("TO CLIP NAME:") {
                ev.to_clip = Some(value("TO CLIP NAME:"));
            } else if cu.starts_with("SOURCE FILE:") {
                ev.files.push(value("SOURCE FILE:"));
            } else if cu.starts_with("EFFECT NAME:") {
                ev.effect = Some(value("EFFECT NAME:"));
            } else if cu.starts_with("LOC:") {
                let rest = value("LOC:");
                let mut it = rest.splitn(3, char::is_whitespace).filter(|s| !s.is_empty());
                if let Some(tc) = it.next().and_then(parse_tc) {
                    let rest2: Vec<&str> = rest.split_whitespace().skip(1).collect();
                    let (color, name) = match rest2.split_first() {
                        Some((c, n)) if marker_color(c).is_some() => (c.to_string(), n.join(" ")),
                        _ => (String::new(), rest2.join(" ")),
                    };
                    ev.locs.push((tc, color, name));
                }
            }
            continue;
        }
        if upper.starts_with("M2") {
            let toks: Vec<&str> = l.split_whitespace().collect();
            if toks.len() >= 3 {
                // `M2 REEL fps tc`; reels with spaces are joined back.
                let fps_i = toks.iter().rposition(|t| t.parse::<f64>().is_ok() && parse_tc(t).is_none()).unwrap_or(2);
                let reel = toks[1..fps_i.max(2)].join(" ");
                if let Ok(fps) = toks[fps_i].parse::<f64>()
                    && let Some(ev) = p.events.last_mut()
                {
                    ev.m2.push((reel, fps));
                }
            }
            continue;
        }
        if upper.starts_with("AUD") {
            let extra: Vec<Ch> = l.split_whitespace().skip(1).filter_map(|t| t.parse::<usize>().ok()).map(Ch::A).collect();
            if let Some(line) = p.events.last_mut().and_then(|e| e.lines.last_mut()) {
                line.chans.extend(extra);
            }
            continue;
        }
        if upper.starts_with("SPLIT:") {
            report.warn("EDL split (L-cut/J-cut) lines are not supported; the split was ignored");
            continue;
        }
        if let Some(line) = parse_event_line(l) {
            for tc in &line.tcs {
                p.max_ff = p.max_ff.max(tc.f);
                p.any_df |= tc.df;
            }
            if let Some(e) = p.events.last_mut().filter(|e| e.lines.last().is_some_and(|last| last.num == line.num)) {
                e.lines.push(line);
            } else {
                p.events.push(Event { lines: vec![line], df, ..Default::default() });
            }
            continue;
        }
        if upper.starts_with(">>>") || upper.starts_with("REM") {
            continue;
        }
        report.warn(format!("unrecognised EDL line ignored: {}", l.chars().take(60).collect::<String>()));
    }
    p
}

fn guess_rate(p: &Parsed) -> FrameRate {
    if p.any_df {
        if p.max_ff >= 30 { FrameRate::FPS_59_94 } else { FrameRate::FPS_29_97 }
    } else if p.max_ff >= 30 {
        FrameRate::FPS_60
    } else if p.max_ff >= 25 {
        FrameRate::FPS_30
    } else if p.max_ff >= 24 {
        FrameRate::FPS_25
    } else {
        FrameRate::FPS_24
    }
}

fn marker_color(s: &str) -> Option<Label> {
    Some(match s.to_ascii_uppercase().as_str() {
        "RED" => Label::Rose,
        "GREEN" => Label::Green,
        "BLUE" => Label::Blue,
        "CYAN" => Label::Teal,
        "MAGENTA" => Label::Magenta,
        "YELLOW" => Label::Yellow,
        "WHITE" => Label::Lavender,
        "BLACK" => Label::Brown,
        _ => return None,
    })
}

fn color_word(l: Label) -> &'static str {
    match l {
        Label::Rose => "RED",
        Label::Green | Label::Forest => "GREEN",
        Label::Blue | Label::Iris | Label::Cerulean => "BLUE",
        Label::Teal | Label::Caribbean => "CYAN",
        Label::Magenta | Label::Purple | Label::Violet => "MAGENTA",
        Label::Lavender => "WHITE",
        Label::Brown => "BLACK",
        Label::Yellow | Label::Mango | Label::Tan => "YELLOW",
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

struct Ctx<'a> {
    b: Builder,
    seq: Sequence,
    rate: FrameRate,
    start_tc: i64,
    opts: &'a ImportOptions,
    has_video: bool,
    has_audio: bool,
}

impl Ctx<'_> {
    fn frames(&self, tc: Tc, df: bool) -> i64 {
        fields_to_frames(tc.h, tc.m, tc.s, tc.f, self.rate, tc.df || df)
    }

    fn rec(&self, tc: Tc, df: bool) -> Tick {
        self.rate.tick_of(self.frames(tc, df) - self.start_tc)
    }

    fn src(&self, tc: Tc, df: bool) -> Tick {
        self.rate.tick_of(self.frames(tc, df))
    }

    /// Media item for a line.
    fn media(&mut self, reel: &str, clip: Option<&str>, file: Option<&str>, chans: &[Ch]) -> ItemId {
        let (w, h) = (self.seq.settings.width, self.seq.settings.height);
        let spec = MediaSpec {
            duration: None,
            video: chans.contains(&Ch::V).then_some((w, h, self.rate)).or(self.has_video.then_some((w, h, self.rate))),
            audio: chans.iter().any(|c| matches!(c, Ch::A(_))).then_some((48_000, 2)).or(self.has_audio.then_some((48_000, 2))),
            ..Default::default()
        };
        if let Some(f) = file {
            let path = resolve_path(f, self.opts.base_dir.as_deref());
            let name = file_name(&path).to_string();
            return self.b.file_media(&format!("file:{path}"), &name, &path, &spec, None);
        }
        let name = clip.filter(|c| !c.is_empty()).unwrap_or(reel).to_string();
        let key = format!("reel:{reel}|{name}");
        if let Some(id) = self.b.find_media(&key) {
            return id;
        }
        // Without a file, the clip name (or reel) is the best path guess; relinking resolves it.
        let path = resolve_path(&name, self.opts.base_dir.as_deref());
        let id = self.b.file_media(&key, &name, &path, &spec, None);
        if let Some(m) = self.b.p.item_mut(id) {
            m.metadata.insert("Tape Name".into(), reel.to_string());
            if let Some(mc) = m.as_media_mut() {
                mc.offline = true;
            }
        }
        id
    }

    fn track_index(&mut self, ch: Ch, video_base: usize) -> (TrackKind, usize) {
        match ch {
            Ch::V => (TrackKind::Video, video_base),
            Ch::A(n) => (TrackKind::Audio, n.saturating_sub(1)),
        }
    }

    /// Put an item for `line` on each of its channels (overwrite). Returns (track, clip) per channel.
    #[allow(clippy::too_many_arguments)]
    fn lay(
        &mut self,
        line: &Line,
        item: ItemId,
        name: &str,
        start: Tick,
        dur: Tick,
        src_in: Tick,
        m2: Option<f64>,
        video_base: usize,
    ) -> Vec<(TrackKind, usize, filmcraft_project::ClipId)> {
        let mut out = Vec::new();
        let link = if line.chans.len() > 1 { Some(self.b.link_id()) } else { None };
        let base = self.rate.timecode_base() as f64;
        let chans: BTreeSet<Ch> = line.chans.iter().copied().collect();
        for ch in chans {
            let (kind, idx) = self.track_index(ch, video_base);
            let mut ti = self.b.clip(item, kind, name, start, dur, src_in);
            ti.link = link;
            if let Some(fps) = m2 {
                if fps == 0.0 {
                    ti.frame_hold = Some(src_in);
                } else {
                    ti.speed = (fps.abs() / base * 10_000.0).round() / 10_000.0;
                    ti.reverse = fps < 0.0;
                }
            }
            let id = ti.id;
            self.b.ensure_tracks(&mut self.seq, kind, idx + 1);
            let mut next = self.b.p.next_id;
            let track = &mut self.seq.tracks_mut(kind)[idx];
            overwrite(track, ti, &mut || {
                let v = next;
                next += 1;
                v
            });
            self.b.p.next_id = next;
            out.push((kind, idx, id));
        }
        out
    }
}

pub(crate) fn import(text: &str, opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let parsed = parse(text, report);
    if parsed.events.is_empty() {
        return Err(crate::Error::parse(Format::Edl, "no events found"));
    }
    let rate = match opts.edl_frame_rate {
        Some(r) => r,
        None => {
            let r = guess_rate(&parsed);
            report.info(format!("EDL has no frame rate; assumed {r} (set ImportOptions::edl_frame_rate to override)"));
            r
        }
    };
    let title = parsed.title.clone().or_else(|| opts.name.clone()).unwrap_or_else(|| "EDL Sequence".into());
    let mut settings = settings_for(rate, 1920, 1080, parsed.any_df);
    settings.drop_frame = parsed.any_df && rate.supports_drop_frame();
    let mut ctx = Ctx { b: Builder::new(&title), seq: empty_sequence(settings), rate, start_tc: 0, opts, has_video: false, has_audio: false };
    for ev in &parsed.events {
        for l in &ev.lines {
            ctx.has_video |= l.chans.contains(&Ch::V);
            ctx.has_audio |= l.chans.iter().any(|c| matches!(c, Ch::A(_)));
        }
    }
    // Sequence start: the hour of the first record timecode.
    let first_rec = {
        let c = &ctx;
        parsed.events.iter().flat_map(|e| e.lines.iter().map(move |l| c.frames(l.tcs[2], e.df))).min().unwrap_or(0)
    };
    let (_, h, _, _, _) = filmcraft_time::frames_to_fields(first_rec, rate, ctx.seq.settings.drop_frame);
    ctx.start_tc = fields_to_frames(h, 0, 0, 0, rate, ctx.seq.settings.drop_frame).min(first_rec);
    ctx.seq.start_timecode = ctx.start_tc;
    if source_tc_warning(&parsed) {
        report.info("EDL source timecodes were imported as media time; rebase after probing media that has a start timecode");
    }

    for ev in &parsed.events {
        import_event(&mut ctx, ev, report);
        for (tc, color, name) in &ev.locs {
            let t = ctx.rec(*tc, ev.df);
            let id = MarkerId(ctx.b.alloc());
            ctx.seq.markers.push(Marker {
                id,
                start: t,
                duration: Tick::ZERO,
                name: name.clone(),
                comment: String::new(),
                kind: MarkerKind::Comment,
                color: marker_color(color).unwrap_or(Label::Green),
            });
        }
    }
    ctx.seq.markers.sort_by_key(|m| m.start);
    if ctx.seq.video_tracks.is_empty() {
        ctx.b.ensure_tracks(&mut ctx.seq, TrackKind::Video, 1);
    }
    if ctx.seq.audio_tracks.is_empty() {
        ctx.b.ensure_tracks(&mut ctx.seq, TrackKind::Audio, 1);
    }
    let seq_id = ctx.b.reserve_sequence(&title, ctx.seq.settings.clone(), None);
    let seq = std::mem::replace(&mut ctx.seq, empty_sequence(Default::default()));
    ctx.b.put_sequence(seq_id, seq);
    ctx.b.top.push(seq_id);
    Ok(ctx.b.finish())
}

fn source_tc_warning(p: &Parsed) -> bool {
    p.events.iter().flat_map(|e| &e.lines).any(|l| l.reel != "BL" && l.tcs[0].h > 0)
}

fn import_event(ctx: &mut Ctx, ev: &Event, report: &mut Report) {
    let df = ev.df;
    let m2_for = |reel: &str| ev.m2.iter().find(|(r, _)| r == reel).map(|(_, f)| *f);
    match ev.lines.as_slice() {
        [l1, l2] if matches!(l2.trans, Trans::Dissolve | Trans::Wipe(_)) => {
            let t0 = ctx.rec(l2.tcs[2], df);
            let t1 = ctx.rec(l2.tcs[3], df);
            let d = ctx.rate.tick_of(l2.dur);
            // Resolve separately for each track kind: named audio dissolves have
            // different definitions from their video counterparts.
            let mut video_effect = None;
            let mut audio_effect = None;
            let to_black = l2.reel.eq_ignore_ascii_case("BL");
            let from_black = l1.reel.eq_ignore_ascii_case("BL");
            // Incoming clip.
            let mut placed = Vec::new();
            if !to_black {
                let file = if ev.files.len() >= 2 { ev.files.get(1) } else { ev.files.first() };
                let clip = ev.to_clip.as_deref().or(ev.from_clip.as_deref().filter(|_| from_black));
                let item = ctx.media(&l2.reel, clip, file.map(String::as_str), &l2.chans);
                let name = clip.unwrap_or(&l2.reel).to_string();
                let src = ctx.src(l2.tcs[0], df);
                placed = ctx.lay(l2, item, &name, t0, t1 - t0, src, m2_for(&l2.reel), 0);
            }
            // Transition on each channel of the incoming line.
            let chans: BTreeSet<Ch> = l2.chans.iter().copied().collect();
            for ch in chans {
                let (kind, idx) = ctx.track_index(ch, 0);
                ctx.b.ensure_tracks(&mut ctx.seq, kind, idx + 1);
                let to = placed.iter().find(|(k, i, _)| *k == kind && *i == idx).map(|p| p.2);
                let track = &mut ctx.seq.tracks_mut(kind)[idx];
                let from = if from_black { None } else { track.items.iter().find(|i| i.end() == t0).map(|i| i.id) };
                let mut align = TransitionAlign::StartAtCut;
                if to_black {
                    // Fade out: extend the outgoing clip through the transition.
                    if let Some(fid) = from {
                        let blocked = track.items.iter().any(|o| o.id != fid && o.start < t0 + d && o.end() > t0);
                        if let Some(it) = track.item_mut(fid)
                            && !blocked
                        {
                            it.duration += d;
                        }
                    }
                    align = TransitionAlign::EndAtCut;
                }
                if from.is_none() && to.is_none() {
                    continue;
                }
                let audio = kind == TrackKind::Audio;
                let cache = if audio { &mut audio_effect } else { &mut video_effect };
                let eff = cache.get_or_insert_with(|| transition_for(l2.trans, ev.effect.as_deref(), audio, report)).clone();
                let id = TransitionId(ctx.b.alloc());
                let track = &mut ctx.seq.tracks_mut(kind)[idx];
                track.transitions.push(Transition { id, effect: eff, start: t0, duration: d, from, to, align, reverse: false });
                track.sort();
            }
        }
        [l1, l2] if l1.trans == Trans::KeyBackground && matches!(l2.trans, Trans::Key | Trans::KeyOut) => {
            report.info("EDL key events imported as background on V1 and foreground on V2");
            if l2.trans == Trans::KeyOut {
                report.warn("EDL key-out fade was imported as a plain key");
            }
            for (i, l) in [l1, l2].into_iter().enumerate() {
                lay_cut(ctx, ev, l, i, df, &m2_for(&l.reel));
            }
        }
        lines => {
            if lines.len() > 1 {
                report.warn(format!("EDL event {} has {} lines; imported as separate cuts", lines[0].num, lines.len()));
            }
            for l in lines {
                match l.trans {
                    Trans::Cut | Trans::KeyBackground => {}
                    Trans::Key | Trans::KeyOut => {
                        lay_cut(ctx, ev, l, 1, df, &m2_for(&l.reel));
                        continue;
                    }
                    _ => report.warn(format!("EDL event {}: transition without an outgoing source line imported as a cut", l.num)),
                }
                lay_cut(ctx, ev, l, 0, df, &m2_for(&l.reel));
            }
        }
    }
}

fn lay_cut(ctx: &mut Ctx, ev: &Event, l: &Line, video_base: usize, df: bool, m2: &Option<f64>) {
    if l.reel.eq_ignore_ascii_case("BL") {
        if video_base > 0 || !l.chans.contains(&Ch::V) {
            return;
        }
        // Black filler on V1 is a gap.
        return;
    }
    let t0 = ctx.rec(l.tcs[2], df);
    let t1 = ctx.rec(l.tcs[3], df);
    if t1 <= t0 {
        return;
    }
    let clip = if video_base > 0 { ev.to_clip.as_deref().or(ev.from_clip.as_deref()) } else { ev.from_clip.as_deref() };
    let file = if video_base > 0 && ev.files.len() > 1 { ev.files.get(1) } else { ev.files.first() };
    let item = ctx.media(&l.reel, clip, file.map(String::as_str), &l.chans);
    let name = clip.unwrap_or(&l.reel).to_string();
    let src = ctx.src(l.tcs[0], df);
    ctx.lay(l, item, &name, t0, t1 - t0, src, *m2, video_base);
}

fn transition_for(t: Trans, effect_name: Option<&str>, audio: bool, report: &mut Report) -> filmcraft_project::EffectInstance {
    if let Some(n) = effect_name {
        let effect = transition_effect(n, audio, report);
        if audio && effect.def().is_some_and(|d| d.kind != filmcraft_project::EffectKind::AudioTransition) {
            report.warn(format!("transition \"{n}\" is not an audio transition; imported as Constant Power"));
            return find_effect("constant_power").map(|d| d.instance()).unwrap_or(effect);
        }
        return effect;
    }
    if audio {
        // Unnamed EDL audio dissolves default to constant-power fading.
        return transition_effect("Constant Power", true, report);
    }
    match t {
        Trans::Wipe(code) => {
            let mut e = transition_effect("wipe", false, report);
            match code {
                1 => {}
                2 => {
                    e.params.insert("direction".into(), filmcraft_project::Param::new(ParamValue::Choice(0)));
                }
                c => report.warn(format!("SMPTE wipe code {c:03} imported as a plain Wipe")),
            }
            e
        }
        _ => transition_effect("Cross Dissolve", false, report),
    }
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

struct OutLine {
    item: Option<ItemId>,
    reel: String,
    chan: String,
    trans: String,
    dur: Option<i64>,
    src: (Tick, Tick),
    rec: (Tick, Tick),
}

struct OutEvent {
    lines: Vec<OutLine>,
    comments: Vec<String>,
    m2: Option<(String, f64, Tick)>,
    /// Merge key for combining linked V/A cut events: (link, item, rec, src, speed bits).
    merge: Option<(u64, ItemId, Tick, Tick, Tick, u64, bool)>,
    channels: BTreeSet<Ch>,
    order: (Tick, u8),
}

struct Exp<'a> {
    p: &'a Project,
    seq: &'a Sequence,
    rate: FrameRate,
    df: bool,
    opts: &'a ExportOptions,
    reels: HashMap<ItemId, String>,
    used_reels: HashMap<String, ItemId>,
}

impl Exp<'_> {
    fn reel(&mut self, item: ItemId, report: &mut Report) -> String {
        if let Some(r) = self.reels.get(&item) {
            return r.clone();
        }
        let max = self.opts.edl.reel_len.max(1);
        let base = crate::common::base_item(self.p, item);
        let r = if let Some(g) = generator_of(self.p, base) {
            match g {
                Generator::BlackVideo => "BL".to_string(),
                Generator::ColorMatte { .. } => {
                    report.warn("colour mattes exported to EDL as black (BL)");
                    "BL".to_string()
                }
                _ => {
                    report.warn("synthetic media exported to EDL as reel AX");
                    "AX".to_string()
                }
            }
        } else if self.p.sequence(base).is_some() {
            // a nested sequence has no tape and no file: reel AX in every reel mode (Premiere Pro
            // writes the same); its name is in the clip name comment
            "AX".to_string()
        } else {
            match self.opts.edl.reel_mode {
                ReelMode::Ax => "AX".to_string(),
                ReelMode::ClipName => sanitize_reel(&self.p.item(item).map(|i| i.name.clone()).unwrap_or_default(), max),
                ReelMode::FileName => {
                    let tape = self.p.item(item).and_then(|i| i.metadata.get("Tape Name")).cloned();
                    let src = tape.unwrap_or_else(|| match item_path(self.p, base) {
                        Some(path) => file_stem(path).to_string(),
                        None => self.p.item(item).map(|i| i.name.clone()).unwrap_or_default(),
                    });
                    let mut r = sanitize_reel(&src, max);
                    // Distinct media must get distinct reels.
                    let mut n = 1;
                    while self.used_reels.get(&r).is_some_and(|other| *other != base) {
                        n += 1;
                        let suffix = format!("{n}");
                        let keep = max.saturating_sub(suffix.len());
                        r = format!("{}{}", &sanitize_reel(&src, max)[..keep.min(sanitize_reel(&src, max).len())], suffix);
                    }
                    self.used_reels.insert(r.clone(), base);
                    r
                }
            }
        };
        self.reels.insert(item, r.clone());
        r
    }

    fn tc(&self, frames: i64) -> String {
        format_timecode_frames(frames, self.rate, self.df)
    }

    fn src_frames(&self, item: ItemId, t: Tick) -> i64 {
        let base = crate::common::base_item(self.p, item);
        let start = self
            .p
            .item(base)
            .and_then(|i| i.as_media())
            .and_then(|m| m.info.start_timecode.map(|tc| self.rate.frame_at(m.frame_rate().tick_of(tc))))
            .unwrap_or(0);
        start + frames_round(self.rate, t)
    }

    fn rec_frames(&self, t: Tick) -> i64 {
        self.seq.start_timecode + frames_round(self.rate, t)
    }

    fn file_comment(&self, item: ItemId) -> Option<String> {
        let path = item_path(self.p, crate::common::base_item(self.p, item))?;
        let shown = match &self.opts.relative_to {
            Some(base) => relative_path(path, base).unwrap_or_else(|| path.to_string()),
            None => path.to_string(),
        };
        Some(shown)
    }
}

fn chan_code(set: &BTreeSet<Ch>) -> Option<&'static str> {
    let v: Vec<Ch> = set.iter().copied().collect();
    Some(match v.as_slice() {
        [Ch::V] => "V",
        [Ch::V, Ch::A(1)] => "B",
        [Ch::V, Ch::A(1), Ch::A(2)] => "AA/V",
        [Ch::V, Ch::A(2)] => "A2/V",
        [Ch::A(1)] => "A",
        [Ch::A(2)] => "A2",
        [Ch::A(1), Ch::A(2)] => "AA",
        [Ch::A(3)] => "A3",
        [Ch::A(4)] => "A4",
        _ => return None,
    })
}

fn signed_scaled(d: Tick, speed: f64) -> Tick {
    if d.0 < 0 { -scaled(-d, speed) } else { scaled(d, speed) }
}

pub(crate) fn export(p: &Project, seq_id: ItemId, opts: &ExportOptions, report: &mut Report) -> Result<String> {
    let seq = p.sequence(seq_id).ok_or(crate::Error::NoSequence(seq_id))?;
    let rate = seq.settings.frame_rate;
    let df = seq.settings.drop_frame && rate.supports_drop_frame();
    let mut x = Exp { p, seq, rate, df, opts, reels: HashMap::new(), used_reels: HashMap::new() };
    let title = opts.name.clone().or_else(|| p.item(seq_id).map(|i| i.name.clone())).unwrap_or_default();

    let mut events: Vec<OutEvent> = Vec::new();
    let mut tracks: Vec<(TrackKind, usize, Ch)> = Vec::new();
    if let Some(v) = opts.edl.video_track
        && v < seq.video_tracks.len()
    {
        tracks.push((TrackKind::Video, v, Ch::V));
    }
    for &a in &opts.edl.audio_tracks {
        if a < seq.audio_tracks.len() {
            if a >= 4 {
                report.warn("EDL export supports at most four audio channels; A5 and above were skipped");
                continue;
            }
            tracks.push((TrackKind::Audio, a, Ch::A(a + 1)));
        }
    }
    if seq.video_tracks.len() > 1
        && opts.edl.video_track.is_some()
        && seq.video_tracks.iter().enumerate().any(|(i, t)| Some(i) != opts.edl.video_track && !t.items.is_empty())
    {
        report.info("EDLs hold one video track; other video tracks were not exported (use export_edl_per_track)");
    }

    for (kind, idx, ch) in tracks {
        let track = &seq.tracks(kind)[idx];
        let chs = match ch {
            Ch::V => "V".to_string(),
            Ch::A(1) => "A".to_string(),
            Ch::A(n) => format!("A{n}"),
        };
        for it in &track.items {
            if !on_frame(rate, it.start) || !on_frame(rate, it.duration) {
                report.warn("sub-frame clip positions were rounded to frames in the EDL");
            }
            if matches!(p.item(it.item).map(|i| &i.kind), Some(ItemKind::Sequence(_))) {
                report.warn("nested sequences are exported to EDL as a single source (reel AX)");
            }
            if !it.enabled {
                report.warn("disabled clips are exported to EDL as normal events");
            }
            let tr_in = track.transitions.iter().find(|t| t.to == Some(it.id));
            let tr_out = track.transitions.iter().find(|t| t.from == Some(it.id));
            let reel = x.reel(it.item, report);
            if let Some(clip) = crate::common::uncarried_clip(p, it, rate) {
                report.warn(format!("{clip} cannot be represented in an EDL: it is written as an ordinary event (reel {reel})"));
            }
            let mut rec0 = it.start;
            let mut rec1 = it.end();
            let mut src0 = it.source_in;
            if let Some(t) = tr_out {
                rec1 = rec1.min(t.start).max(rec0);
            }
            let speed = if it.frame_hold.is_some() { 0.0 } else { it.speed };
            let mut lines = Vec::new();
            let mut comments = Vec::new();
            let mut trans_name = None;
            if let Some(t) = tr_in {
                let s = t.start;
                let (l1_reel, l1_src, from_name, from_item) = match t.from.and_then(|f| track.item(f)) {
                    Some(a) => {
                        let r = x.reel(a.item, report);
                        (r, a.source_in + signed_scaled(s - a.start, a.speed), Some(a.name.clone()), Some(a.item))
                    }
                    None => ("BL".to_string(), Tick::ZERO, None, None),
                };
                lines.push(OutLine { item: from_item, reel: l1_reel, chan: chs.clone(), trans: "C".into(), dur: None, src: (l1_src, l1_src), rec: (s, s) });
                src0 = match it.frame_hold {
                    Some(h) => h,
                    None => it.source_in + signed_scaled(s - it.start, it.speed),
                };
                if src0 < Tick::ZERO {
                    report.warn("a transition needs more source media than available; its source in was clamped to 0");
                    src0 = Tick::ZERO;
                }
                rec0 = s;
                let (code, name) = edl_transition(&t.effect, report);
                trans_name = Some(name);
                let src_end = src0 + (rec1 - rec0);
                lines.push(OutLine {
                    item: Some(it.item),
                    reel: reel.clone(),
                    chan: chs.clone(),
                    trans: code,
                    dur: Some(frames_round(rate, t.duration)),
                    src: (src0, src_end),
                    rec: (rec0, rec1),
                });
                if opts.edl.clip_names {
                    if let Some(n) = from_name {
                        comments.push(format!("FROM CLIP NAME: {n}"));
                    }
                    comments.push(format!("TO CLIP NAME: {}", it.name));
                }
                if opts.edl.source_files {
                    if let Some(f) = from_item.and_then(|fi| x.file_comment(fi)) {
                        comments.push(format!("SOURCE FILE: {f}"));
                    }
                    if let Some(f) = x.file_comment(it.item) {
                        comments.push(format!("SOURCE FILE: {f}"));
                    }
                }
            } else {
                if it.frame_hold.is_some() {
                    src0 = it.frame_hold.unwrap_or(it.source_in);
                }
                lines.push(OutLine {
                    item: Some(it.item),
                    reel: reel.clone(),
                    chan: chs.clone(),
                    trans: "C".into(),
                    dur: None,
                    src: (src0, src0 + (rec1 - rec0)),
                    rec: (rec0, rec1),
                });
                if opts.edl.clip_names {
                    comments.push(format!("FROM CLIP NAME: {}", it.name));
                }
                if opts.edl.source_files
                    && let Some(f) = x.file_comment(it.item)
                {
                    comments.push(format!("SOURCE FILE: {f}"));
                }
            }
            if let (true, Some(n)) = (opts.edl.effect_names, trans_name) {
                comments.push(format!("EFFECT NAME: {n}"));
            }
            let m2 = if speed != 1.0 || it.reverse {
                let fps = if it.frame_hold.is_some() { 0.0 } else { speed * rate.timecode_base() as f64 * if it.reverse { -1.0 } else { 1.0 } };
                Some((reel.clone(), fps, src0))
            } else {
                None
            };
            if let Some(ef) = crate::common::standard_effects(it).next() {
                report.warn(format!("effect \"{}\" cannot be represented in an EDL", ef.def().map(|d| d.name).unwrap_or(&ef.effect)));
            }
            let simple = tr_in.is_none() && m2.is_none();
            events.push(OutEvent {
                merge: if simple { it.link.map(|l| (l, it.item, rec0, rec1, src0, 0, it.enabled)) } else { None },
                lines,
                comments,
                m2,
                channels: [ch].into_iter().collect(),
                order: (rec0, if kind == TrackKind::Video { 0 } else { 1 + idx as u8 }),
            });
            // Fade to black after this clip.
            if let Some(t) = tr_out.filter(|t| t.to.is_none()) {
                let s = t.start;
                let a_src = it.source_in + signed_scaled(s - it.start, it.speed);
                let (code, name) = edl_transition(&t.effect, report);
                let d = t.duration;
                let mut comments = Vec::new();
                if opts.edl.clip_names {
                    comments.push(format!("FROM CLIP NAME: {}", it.name));
                }
                if opts.edl.effect_names {
                    comments.push(format!("EFFECT NAME: {name}"));
                }
                events.push(OutEvent {
                    lines: vec![
                        OutLine { item: Some(it.item), reel: reel.clone(), chan: chs.clone(), trans: "C".into(), dur: None, src: (a_src, a_src), rec: (s, s) },
                        OutLine {
                            item: None,
                            reel: "BL".into(),
                            chan: chs.clone(),
                            trans: code,
                            dur: Some(frames_round(rate, d)),
                            src: (Tick::ZERO, d),
                            rec: (s, s + d),
                        },
                    ],
                    comments,
                    m2: None,
                    merge: None,
                    channels: [ch].into_iter().collect(),
                    order: (s, if kind == TrackKind::Video { 0 } else { 1 + idx as u8 }),
                });
            }
        }
    }

    // Merge identical linked cut events across channels (V + A → B, A + A2 → AA …).
    let mut merged: Vec<OutEvent> = Vec::new();
    for ev in events {
        if let Some(key) = ev.merge
            && let Some(prev) = merged.iter_mut().find(|m| m.merge == Some(key))
        {
            let mut set = prev.channels.clone();
            set.extend(ev.channels.iter().copied());
            if let Some(code) = chan_code(&set) {
                prev.channels = set;
                for l in &mut prev.lines {
                    l.chan = code.to_string();
                }
                continue;
            }
        }
        merged.push(ev);
    }
    merged.sort_by_key(|e| e.order);

    let mut out = String::new();
    out.push_str(&format!("TITLE: {title}\n"));
    out.push_str(if df { "FCM: DROP FRAME\n" } else { "FCM: NON-DROP FRAME\n" });
    let w = opts.edl.reel_len.max(8);
    let mut markers: Vec<&Marker> = if opts.edl.markers { seq.markers.iter().collect() } else { Vec::new() };
    markers.sort_by_key(|m| m.start);
    let mut mi = 0;
    let n_events = merged.len();
    for (i, ev) in merged.iter().enumerate() {
        out.push('\n');
        let num = i + 1;
        for l in &ev.lines {
            let dur = l.dur.map(|d| format!("{d:03}")).unwrap_or_default();
            out.push_str(&format!(
                "{num:03}  {:<w$} {:<5} {:<4} {:>3} {} {} {} {}\n",
                l.reel,
                l.chan,
                l.trans,
                dur,
                x.tc(src_tc(&x, l, l.src.0)),
                x.tc(src_tc(&x, l, l.src.1)),
                x.tc(x.rec_frames(l.rec.0)),
                x.tc(x.rec_frames(l.rec.1)),
            ));
        }
        if let Some((reel, fps, entry)) = &ev.m2
            && let Some(l) = ev.lines.last()
        {
            let sign = if *fps < 0.0 { "-" } else { "" };
            out.push_str(&format!("M2   {:<w$}       {sign}{:05.1}                {}\n", reel, fps.abs(), x.tc(src_tc(&x, l, *entry))));
        }
        for c in &ev.comments {
            out.push_str(&format!("* {c}\n"));
        }
        // Markers up to the end of this event (all remaining after the last one).
        let end = ev.lines.last().map(|l| l.rec.1).unwrap_or(Tick::ZERO);
        while mi < markers.len() && (markers[mi].start < end || i + 1 == n_events) {
            let m = markers[mi];
            out.push_str(&format!("* LOC: {} {:<7} {}\n", x.tc(x.rec_frames(m.start)), color_word(m.color), m.name));
            mi += 1;
        }
    }
    Ok(out)
}

/// Source frame for a line position (BL lines are plain frame counts from 0).
fn src_tc(x: &Exp, l: &OutLine, t: Tick) -> i64 {
    match l.item {
        Some(i) if l.reel != "BL" => x.src_frames(i, t),
        _ => frames_round(x.rate, t),
    }
}

/// EDL transition code and effect name.
fn edl_transition(e: &filmcraft_project::EffectInstance, report: &mut Report) -> (String, String) {
    let name = transition_name(e).to_ascii_uppercase();
    match e.effect.as_str() {
        "cross_dissolve" | "constant_power" | "constant_gain" | "exponential_fade" | "film_dissolve" | "additive_dissolve" | "non_additive_dissolve" => {
            ("D".into(), name)
        }
        "wipe" => {
            let dir = e.param("direction").and_then(|p| p.value.as_f64()).unwrap_or(3.0) as u32;
            match dir {
                0 => ("W002".into(), name),
                3 => ("W001".into(), name),
                _ => {
                    report.info("wipe direction approximated by SMPTE wipe 001 in the EDL");
                    ("W001".into(), name)
                }
            }
        }
        _ => {
            report.info(format!("transition \"{}\" written as a dissolve with an EFFECT NAME comment", transition_name(e)));
            ("D".into(), name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        assert_eq!(parse_channels("B"), Some(vec![Ch::V, Ch::A(1)]));
        assert_eq!(parse_channels("AA/V"), Some(vec![Ch::A(1), Ch::A(2), Ch::V]));
        assert_eq!(parse_channels("A4"), Some(vec![Ch::A(4)]));
        assert_eq!(parse_channels("X"), None);
    }

    #[test]
    fn event_lines() {
        let l = parse_event_line("002  REEL_B   V     D    030 02:00:10:00 02:00:15:00 01:00:05:00 01:00:10:00").unwrap();
        assert_eq!(l.trans, Trans::Dissolve);
        assert_eq!(l.dur, 30);
        let k = parse_event_line("003  KEY      V     K B      00:00:00:00 00:00:01:00 01:00:00:00 01:00:01:00").unwrap();
        assert_eq!(k.trans, Trans::KeyBackground);
        let w = parse_event_line("004  R        V     W001 015 00:00:00:00 00:00:01:00 01:00:00:00 01:00:01:00").unwrap();
        assert_eq!(w.trans, Trans::Wipe(1));
        assert!(parse_event_line("* comment").is_none());
        assert!(parse_tc("01;00;00;02").unwrap().df);
    }
}
