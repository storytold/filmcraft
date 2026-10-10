//! Audio Meters: stereo peak meters of the Mix (dB scale, peak hold) fed by the playing mixer
//! graph, plus the BS.1770 loudness readout.

use egui::{Align2, Rect, pos2, vec2};

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Feed the loudness meter with exactly the programme audio played since the last frame
/// (consecutive, non-overlapping blocks, so gating and integration are correct). Seeks reset it.
pub(crate) fn feed_loudness(app: &mut FilmcraftApp) {
    if !app.playback.playing {
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
        *n = now;
    }
}

fn lufs_text(v: f64) -> String {
    if v.is_finite() && v > -70.0 { format!("{v:.1}") } else { "—".into() }
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
    let lufs_h = if rect.height() > 260.0 { 64.0 } else { 0.0 };
    let area = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.min.y + 8.0), pos2(rect.max.x - 26.0, rect.max.y - 30.0 - lufs_h));
    if lufs_h > 0.0 {
        let (m, s, i, tp) = app.loudness.as_ref().map_or((f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY), |(l, _)| {
            (l.momentary(), l.short_term(), l.integrated(), l.true_peak_dbtp())
        });
        let lr = Rect::from_min_max(pos2(rect.min.x + 4.0, rect.max.y - lufs_h), pos2(rect.max.x - 4.0, rect.max.y - 4.0));
        ui.painter().rect_filled(lr, 2.0, t.meter_readout_bg);
        let rows = [("M", lufs_text(m)), ("S", lufs_text(s)), ("I", lufs_text(i)), ("TP", lufs_text(tp))];
        for (k, (label, val)) in rows.iter().enumerate() {
            let y = lr.min.y + 8.0 + k as f32 * 14.0;
            ui.painter().text(pos2(lr.min.x + 6.0, y), Align2::LEFT_CENTER, *label, Tokens::ui(9.5), t.text_dim);
            let hot = *label == "I" || (*label == "TP" && tp > -1.0);
            let col = if *label == "TP" && tp > -1.0 {
                egui::Color32::from_rgb(0xe0, 0x50, 0x40)
            } else if hot {
                t.hot_text
            } else {
                t.text
            };
            ui.painter().text(
                pos2(lr.max.x - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{val} {}", if *label == "TP" { "dBTP" } else { "LUFS" }),
                Tokens::mono(9.5),
                col,
            );
        }
        app.auto.add("audioMeters.loudness", lr, &format!("M {} S {} I {} LUFS TP {} dBTP", lufs_text(m), lufs_text(s), lufs_text(i), lufs_text(tp)));
    }
    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.min.x + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 4.0, area.max.y + 4.0)), 0.0, t.meter_bg);
    let w = ((area.width() - 4.0 * (nch as f32 - 1.0)) / nch as f32).max(3.0);
    for c in 0..nch {
        let r = Rect::from_min_size(pos2(area.min.x + c as f32 * (w + 4.0), area.min.y), vec2(w, area.height()));
        let m = st.get(c).copied().unwrap_or([-90.0; 2]);
        crate::widgets::meter_bar(ui.painter(), r, m[0], m[1], &t);
        app.auto.add(&format!("audioMeters.channel.{}", names[c]), r, &format!("{} {:.1} dB", names[c], m[0]));
    }
    let mut db = 0;
    while db >= -57 {
        let y = area.min.y + area.height() * (-db as f32 / 60.0);
        ui.painter().text(pos2(rect.max.x - 6.0, y), Align2::RIGHT_CENTER, format!("{db}"), Tokens::ui(8.5), t.text_dim);
        db -= 3;
    }
    ui.painter().text(pos2(rect.max.x - 6.0, area.max.y + 10.0), Align2::RIGHT_CENTER, "dB", Tokens::ui(8.5), t.text_dim);
    for c in 0..nch {
        let r = Rect::from_center_size(pos2(area.min.x + c as f32 * (w + 4.0) + w / 2.0, area.max.y + 16.0), vec2(14.0, 14.0));
        let label = if nch == 2 { "S" } else { names[c] };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(if nch == 2 { 10.0 } else { 8.5 }), t.text_dim);
    }
    if app.playback.playing {
        ui.ctx().request_repaint();
    }
    app.auto.add("audioMeters", area, "Audio Meters");
}
