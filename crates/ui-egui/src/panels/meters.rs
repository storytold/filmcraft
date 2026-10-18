//! Audio Meters: stereo peak meters of the Mix (dB scale, peak hold) fed by the playing mixer
//! graph, plus the BS.1770 loudness readout.

use egui::{Align2, Rect, pos2, vec2};

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Feed the loudness readout the panel shows. With a live audio device the mixing thread already
/// measured the programme as it mixed it ([`crate::playback_mix`] posts the readout), so this only
/// reads it back: re-mixing the span here — the old way — duplicated that work on the UI thread at
/// frame rate and fought the mixing thread for the sources and the mixer-graph cache, which showed
/// up as meters lagging behind the sound and a generally sluggish UI.
///
/// Without an audio device nothing else mixes the programme (the meters also stand still), so the
/// old per-frame re-mix stays as the fallback: consecutive, non-overlapping blocks, so gating and
/// integration are correct. Seeks reset it.
pub(crate) fn feed_loudness(app: &mut FilmcraftApp) {
    if !app.playback.playing {
        return;
    }
    if app.playback.audio_clock {
        app.loudness_values = app.session.previews.live.loudness_readout();
        return;
    }
    let Some(seq) = app.session.active_sequence() else { return };
    let sr = seq.settings.sample_rate as i64;
    let now = app.session.playhead().to_units_floor(sr);
    let fresh = match &app.loudness {
        Some((_, next)) => now < *next || now - *next > sr,
        None => true,
    };
    if fresh {
        app.loudness = Some((filmcraft_audio_dsp::LoudnessMeter::new(sr as f64, 2), now));
        return;
    }
    let next = app.loudness.as_ref().map_or(now, |l| l.1);
    if now <= next {
        return;
    }
    let provider = app.session.media.provider(app.session.project.clone(), app.session.services.clone());
    let Some(seq_id) = app.session.state.active_sequence else { return };
    let buf = app.session.previews.mix(&app.session.project, seq_id, next, (now - next) as usize, &provider);
    if let Some((m, n)) = app.loudness.as_mut() {
        let chans: Vec<&[f32]> = buf.channels.iter().take(2).map(Vec::as_slice).collect();
        if chans.len() == 2 {
            m.process(&chans);
        } else if let Some(c) = chans.first() {
            m.process(&[c, c]);
        }
        app.loudness_values = [m.momentary(), m.short_term(), m.integrated(), m.true_peak_dbtp()];
        *n = now;
    }
}

fn lufs_text(v: f64) -> String {
    if v.is_finite() && v > -70.0 { format!("{v:.1}") } else { "—".into() }
}

const SCALE_FONT: f32 = 8.5;
const READOUT_FONT: f32 = 9.5;

/// Where the panel's parts go. What doesn't fit is left out rather than drawn over something
/// else: as the panel narrows, the loudness readout drops its units and then goes, and the dB
/// scale goes once the bars would get too thin; as it gets shorter, the scale numbers thin out to
/// every 6, 12 or 24 dB and then go, and the channel names and "dB" under the bars go.
#[derive(Debug)]
struct Parts {
    /// The bars.
    area: Rect,
    /// Scale numbers and where their text goes.
    scale: Vec<(i32, Rect)>,
    /// "dB" under the scale.
    unit: Option<Rect>,
    /// The name under each bar.
    names: Vec<Rect>,
    /// The loudness readout, and whether its values keep their units.
    readout: Option<(Rect, bool)>,
}

/// Lay out a meter of `names.len()` bars in `rect`; `size` measures a text in a font.
fn parts(rect: Rect, names: &[&str], size: impl Fn(&str, egui::FontId) -> egui::Vec2) -> Parts {
    let n = names.len().max(1) as f32;
    let readout = (rect.height() > 260.0)
        .then(|| {
            // a row: the label at the left and the value at the right, 6 inside a box inset 4
            let room = rect.width() - 20.0;
            let label = size("TP", Tokens::ui(READOUT_FONT)).x;
            let fits = |value: &str| label + 6.0 + size(value, Tokens::mono(READOUT_FONT)).x <= room;
            if fits("-88.8 dBTP") { Some(true) } else { fits("-88.8").then_some(false) }
        })
        .flatten();
    let lufs_h = if readout.is_some() { 64.0 } else { 0.0 };
    let feet = rect.height() >= 90.0;
    let top = rect.min.y + 8.0;
    let bottom = (rect.max.y - lufs_h - if feet { 30.0 } else { 8.0 }).max(top);
    let h = bottom - top;
    let widest = size("-57", Tokens::ui(SCALE_FONT));
    let column = (widest.x + 10.0).max(26.0);
    let step = [3, 6, 12, 24].into_iter().find(|s| h * *s as f32 / 60.0 >= widest.y).filter(|_| rect.width() - 8.0 - column >= n * 6.0 + 4.0 * (n - 1.0));
    let right = if step.is_some() { rect.max.x - column } else { rect.max.x - 8.0 };
    let area = Rect::from_min_max(pos2(rect.min.x + 8.0, top), pos2(right.max(rect.min.x + 8.0), bottom));
    let text_at = |text: &str, font: egui::FontId, at: egui::Pos2, align: Align2| align.anchor_size(at, size(text, font));
    let scale: Vec<(i32, Rect)> = step
        .map(|s| {
            (0..=57 / s)
                .map(|k| {
                    let y = area.min.y + h * (k * s) as f32 / 60.0;
                    (-(k * s), text_at(&(-(k * s)).to_string(), Tokens::ui(SCALE_FONT), pos2(rect.max.x - 6.0, y), Align2::RIGHT_CENTER))
                })
                .collect()
        })
        .unwrap_or_default();
    let (w, pitch) = bar_pitch(area, names.len());
    let feet_names: Vec<Rect> = names
        .iter()
        .enumerate()
        .map(|(c, name)| {
            let (text, font) = foot(name, names.len());
            text_at(text, font, pos2(area.min.x + c as f32 * pitch + w / 2.0, area.max.y + 16.0), Align2::CENTER_CENTER)
        })
        .collect();
    let names = if feet && feet_names.iter().all(|r| r.width() < pitch) { feet_names } else { Vec::new() };
    let unit = (feet && step.is_some())
        .then(|| text_at("dB", Tokens::ui(SCALE_FONT), pos2(rect.max.x - 6.0, area.max.y + 10.0), Align2::RIGHT_CENTER))
        .filter(|u| !names.iter().chain(scale.iter().map(|(_, r)| r)).any(|r| overlaps(*u, *r)));
    let readout = readout.map(|units| (Rect::from_min_max(pos2(rect.min.x + 4.0, rect.max.y - lufs_h), pos2(rect.max.x - 4.0, rect.max.y - 4.0)), units));
    Parts { area, scale, unit, names, readout }
}

/// The text under a bar of a meter with `n` bars, and its font: an "S" under each bar of a stereo
/// meter, the channel names under a 5.1 one.
fn foot(name: &str, n: usize) -> (&str, egui::FontId) {
    if n == 2 { ("S", Tokens::ui(10.0)) } else { (name, Tokens::ui(SCALE_FONT)) }
}

/// The width of a bar and the distance from one bar to the next.
fn bar_pitch(area: Rect, n: usize) -> (f32, f32) {
    let n = n.max(1) as f32;
    let gap = if area.width() >= n * 6.0 + 4.0 * (n - 1.0) { 4.0 } else { 1.0 };
    let w = ((area.width() - gap * (n - 1.0)) / n).max(1.0);
    (w, w + gap)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.0 && i.height() > 0.0
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    // levels of the Mix as it plays (fast attack, 20 dB/s release, peak hold)
    let st = super::mixer::poll_meters(app, ui).get(&filmcraft_render::mixer::MASTER.0).cloned().unwrap_or_else(|| vec![[-90.0; 2]; 2]);
    // one bar per channel of the Mix (2, or 6 for a 5.1 Mix)
    let nch = app.session.active_sequence().map(|q| filmcraft_render::mixer::width_of(q.settings.audio_master)).unwrap_or(2);
    let names = filmcraft_audio_dsp::channels::Layout::from_channels(nch).names();
    // Premiere: black meter area, scale 0 … −57 dB in 3 dB steps on the right, "dB" at the foot.
    // Loudness readout (BS.1770 / EBU R128) under the bars.
    let parts = parts(rect, names, |text, font| ui.painter().layout_no_wrap(text.to_string(), font, t.text).size());
    let area = parts.area;
    if let Some((lr, units)) = parts.readout {
        let [m, s, i, tp] = app.loudness_values.map(|v| if v.is_finite() { v } else { f64::NEG_INFINITY });
        ui.painter().rect_filled(lr, 2.0, t.meter_readout_bg);
        let rows = [("M", lufs_text(m)), ("S", lufs_text(s)), ("I", lufs_text(i)), ("TP", lufs_text(tp))];
        for (k, (label, val)) in rows.iter().enumerate() {
            let y = lr.min.y + 8.0 + k as f32 * 14.0;
            ui.painter().text(pos2(lr.min.x + 6.0, y), Align2::LEFT_CENTER, *label, Tokens::ui(READOUT_FONT), t.text_dim);
            let hot = *label == "I" || (*label == "TP" && tp > -1.0);
            let col = if *label == "TP" && tp > -1.0 {
                egui::Color32::from_rgb(0xe0, 0x50, 0x40)
            } else if hot {
                t.hot_text
            } else {
                t.text
            };
            let unit = match (units, *label == "TP") {
                (false, _) => "",
                (true, true) => " dBTP",
                (true, false) => " LUFS",
            };
            ui.painter().text(pos2(lr.max.x - 6.0, y), Align2::RIGHT_CENTER, format!("{val}{unit}"), Tokens::mono(READOUT_FONT), col);
        }
        app.auto.add("audioMeters.loudness", lr, &format!("M {} S {} I {} LUFS TP {} dBTP", lufs_text(m), lufs_text(s), lufs_text(i), lufs_text(tp)));
    }
    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.min.x + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 4.0, area.max.y + 4.0)), 0.0, t.meter_bg);
    let (w, pitch) = bar_pitch(area, names.len());
    for (c, name) in names.iter().enumerate() {
        let r = Rect::from_min_size(pos2(area.min.x + c as f32 * pitch, area.min.y), vec2(w, area.height()));
        let m = st.get(c).copied().unwrap_or([-90.0; 2]);
        crate::widgets::meter_bar(ui.painter(), r, m[0], m[1], &t);
        app.auto.add(&format!("audioMeters.channel.{name}"), r, &format!("{name} {:.1} dB", m[0]));
    }
    for (db, r) in &parts.scale {
        ui.painter().text(r.right_center(), Align2::RIGHT_CENTER, db.to_string(), Tokens::ui(SCALE_FONT), t.text_dim);
    }
    if let Some(r) = parts.scale.iter().map(|(_, r)| *r).reduce(|a, b| a.union(b)) {
        let numbers: Vec<String> = parts.scale.iter().map(|(db, _)| db.to_string()).collect();
        app.auto.add("audioMeters.scale", r, &numbers.join(" "));
    }
    if let Some(r) = parts.unit {
        ui.painter().text(r.right_center(), Align2::RIGHT_CENTER, "dB", Tokens::ui(SCALE_FONT), t.text_dim);
    }
    for (r, name) in parts.names.iter().zip(names) {
        let (text, font) = foot(name, names.len());
        ui.painter().text(r.center(), Align2::CENTER_CENTER, text, font, t.text_dim);
    }
    if app.playback.playing {
        ui.ctx().request_repaint();
    }
    app.auto.add("audioMeters", area, "Audio Meters");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with a measure of text in egui's own fonts.
    fn with_text_size(f: impl FnOnce(&dyn Fn(&str, egui::FontId) -> egui::Vec2)) {
        let ctx = egui::Context::default();
        let mut f = Some(f);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            if let Some(f) = f.take() {
                f(&|text, font| ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::WHITE).size());
            }
        })
        .textures_delta
        .clear();
    }

    const STEREO: [&str; 2] = ["L", "R"];
    const SURROUND: [&str; 6] = ["L", "R", "C", "LFE", "Ls", "Rs"];

    fn at(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(w, h))
    }

    /// The default size in the Editing workspace shows everything, as before.
    #[test]
    fn a_roomy_panel_shows_every_part() {
        with_text_size(|size| {
            let p = parts(at(116.0, 400.0), &STEREO, size);
            assert_eq!(p.scale.iter().map(|(db, _)| *db).collect::<Vec<_>>(), (0..20).map(|k| -3 * k).collect::<Vec<_>>());
            assert!(p.unit.is_some());
            assert_eq!(p.names.len(), 2);
            assert_eq!(p.readout.map(|r| r.1), Some(true), "readout with units");
        });
    }

    /// Narrowing drops the readout's units, then the readout, then the scale; the bars stay.
    #[test]
    fn narrowing_drops_readout_units_then_readout_then_scale() {
        with_text_size(|size| {
            let mut seen = Vec::new();
            for w in (40..=116).rev() {
                let p = parts(at(w as f32, 400.0), &STEREO, size);
                let stage = match (p.readout.map(|r| r.1), p.scale.is_empty()) {
                    (Some(true), false) => 0,
                    (Some(false), false) => 1,
                    (None, false) => 2,
                    (None, true) => 3,
                    other => panic!("{w}: {other:?}"),
                };
                if seen.last() != Some(&stage) {
                    seen.push(stage);
                }
                assert!(p.area.width() >= 10.0, "{w}: bars stay readable");
            }
            assert_eq!(seen, [0, 1, 2, 3]);
        });
    }

    /// Getting shorter thins the scale out (3, 6, 12, 24 dB apart) and then hides it, and the
    /// names under the bars go. (The scale gets denser again where the readout or the names
    /// go and give the bars their room.)
    #[test]
    fn shortening_thins_the_scale_then_hides_it() {
        with_text_size(|size| {
            let step = |h: i32| {
                let p = parts(at(116.0, h as f32), &STEREO, size);
                match p.scale.as_slice() {
                    [] => 99,
                    [_] => 60,
                    [a, b, ..] => a.0 - b.0,
                }
            };
            let mut seen = Vec::new();
            for range in [261..=400, 90..=260, 10..=89] {
                let mut steps: Vec<i32> = range.rev().map(step).collect();
                steps.dedup();
                assert!(steps.windows(2).all(|s| s[0] < s[1]), "{steps:?}");
                seen.extend(steps);
            }
            seen.sort();
            seen.dedup();
            assert_eq!(seen, [3, 6, 12, 24, 99]);
            for h in 20..=400 {
                assert_eq!(parts(at(116.0, h as f32), &STEREO, size).names.is_empty(), h < 90, "{h}");
            }
        });
    }

    /// At any size no text overlaps other text, the bars or the readout, and everything stays
    /// inside the panel.
    #[test]
    fn no_text_overlaps_at_any_size() {
        with_text_size(|size| {
            for names in [&STEREO[..], &SURROUND[..]] {
                for w in (20..=400).step_by(3) {
                    for h in (10..=800).step_by(7) {
                        let rect = at(w as f32, h as f32);
                        let p = parts(rect, names, size);
                        let mut texts: Vec<Rect> = p.scale.iter().map(|(_, r)| *r).chain(p.unit).chain(p.names.iter().copied()).collect();
                        if let Some((r, units)) = p.readout {
                            for (k, label) in ["M", "S", "I", "TP"].into_iter().enumerate() {
                                let y = r.min.y + 8.0 + k as f32 * 14.0;
                                let value = if units { "-88.8 dBTP" } else { "-88.8" };
                                let l = Align2::LEFT_CENTER.anchor_size(pos2(r.min.x + 6.0, y), size(label, Tokens::ui(READOUT_FONT)));
                                let v = Align2::RIGHT_CENTER.anchor_size(pos2(r.max.x - 6.0, y), size(value, Tokens::mono(READOUT_FONT)));
                                assert!(!overlaps(l, v) && r.contains_rect(l) && r.contains_rect(v), "{w}×{h} readout row {label}");
                            }
                            texts.push(r);
                        }
                        for (i, a) in texts.iter().enumerate() {
                            assert!(rect.contains_rect(*a), "{w}×{h}: {a:?} outside {rect:?}");
                            assert!(!overlaps(*a, p.area), "{w}×{h}: {a:?} over the bars {:?}", p.area);
                            for b in texts.iter().skip(i + 1) {
                                assert!(!overlaps(*a, *b), "{w}×{h} {}ch: {a:?} overlaps {b:?}", names.len());
                            }
                        }
                    }
                }
            }
        });
    }
}
