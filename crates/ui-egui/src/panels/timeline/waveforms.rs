use super::*;

pub(super) fn draw_waveform(app: &mut FilmcraftApp, p: &egui::Painter, body: Rect, it: &TrackItem, col: Color32) {
    let Some(peaks) = request_peaks(app, it.item, it.audio_stream) else { return };
    let pkey = (it.item, it.audio_stream);
    let spp = 256.0;
    let sr = 48_000.0;
    let zone_h = (body.height() * 0.42).max(8.0);
    let area = Rect::from_min_max(pos2(body.min.x + 1.0, body.max.y - zone_h - 1.0), pos2(body.max.x - 1.0, body.max.y - 1.0));
    let clip = p.clip_rect().intersect(area);
    if clip.width() <= 0.0 {
        return;
    }
    let key = Arc::as_ptr(&peaks) as usize;
    let peak = match app.tl.peak_max.get(&pkey) {
        Some(&(k, v)) if k == key => v,
        _ => {
            let v = peaks.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()));
            app.tl.peak_max.insert(pkey, (key, v));
            v
        }
    };
    let gain = waveform_display_gain(peak, it.gain_db);
    let dynamic = app.ui.extras.dynamic_waveforms;
    let mut mesh = egui::Mesh::default();
    let dur_px = body.width().max(1.0);
    for x in (clip.min.x.floor() as i32)..(clip.max.x.ceil() as i32) {
        let f0 = (x as f32 - body.min.x) / dur_px;
        let f1 = (x as f32 + 1.0 - body.min.x) / dur_px;
        let t0 = it.start + Tick((it.duration.0 as f64 * f0 as f64) as i64);
        let t1 = it.start + Tick((it.duration.0 as f64 * f1 as f64) as i64);
        let s0 = (it.source_time_at(t0).seconds() * sr / spp) as usize;
        let s1 = ((it.source_time_at(t1).seconds() * sr / spp) as usize).max(s0 + 1);
        let mut m = 0f32;
        for (a, b) in peaks.iter().skip(s0).take(s1 - s0) {
            m = m.max(a.abs()).max(b.abs());
        }
        // as in Premiere, the waveform shows the clip's Volume
        m *= crate::panels::timeline_volume::gain_at(it, t0);
        // View ▸ Dynamic Audio Waveforms (default): logarithmic scale, −48 dB → 0, 0 dB → full;
        // off: linear amplitude
        let h = if dynamic {
            let db = 20.0 * (m * gain).max(1e-5).log10();
            ((db + 48.0) / 48.0).clamp(0.0, 1.0) * area.height()
        } else {
            (m * gain).clamp(0.0, 1.0) * area.height()
        };
        if h > 0.3 {
            mesh.add_colored_rect(Rect::from_min_max(pos2(x as f32, area.max.y - h), pos2(x as f32 + 1.0, area.max.y)), col);
        }
    }
    p.add(mesh);
    // channel label box
    let cb = Rect::from_min_size(pos2(body.min.x + 3.0, body.max.y - 11.0), vec2(8.0, 9.0));
    if body.width() > 30.0 {
        p.rect_filled(cb, 1.0, Color32::from_black_alpha(160));
        p.text(cb.center(), Align2::CENTER_CENTER, "1", Tokens::ui(7.5), Color32::from_rgb(0xd9, 0xd9, 0xd9));
    }
}

/// Display gain for a clip's waveform: clip gain, plus normalisation of the source to full scale
/// so quiet recordings stay readable, but by at most +12 dB, so a near-silent track (room tone at
/// −50 dBFS, a silent film's empty audio stream) does not look like a loud one.
pub(super) fn waveform_display_gain(source_peak: f32, clip_gain_db: f64) -> f32 {
    const MAX_BOOST: f32 = 3.981_072; // +12 dB
    let boost = (1.0 / source_peak.max(1e-9)).min(MAX_BOOST);
    filmcraft_render::audio::db_to_gain(clip_gain_db) * boost
}

/// A key that changes when the sound of sequence `item` does: its audio segments (clips,
/// transitions, mixer state; see `filmcraft_render::preview::audio_segments`) and its length.
fn sequence_audio_key(project: &filmcraft_project::Project, item: ItemId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for seg in filmcraft_render::preview::audio_segments(project, item) {
        (seg.first_sample, seg.samples, seg.hash).hash(&mut h);
    }
    project.sequence(item).map(|q| (q.duration().0, q.settings.sample_rate)).hash(&mut h);
    h.finish()
}

/// Waveform peaks of a nested sequence's mix, in the same form as a media item's (min, max per
/// 256 samples at 48 kHz). They are mixed in the background, and again whenever the sequence's
/// sound changes; until the new ones are ready the old ones stay on show.
fn request_sequence_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    let revision = app.session.revision;
    let key = match app.tl.seq_keys_seen.get(&item) {
        Some(&(rev, key)) if rev == revision => key,
        _ => {
            let key = sequence_audio_key(&app.session.project, item);
            app.tl.seq_keys_seen.insert(item, (revision, key));
            key
        }
    };
    let pkey = (item, 0);
    let have = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&pkey).cloned();
    let current = app.tl.seq_peak_keys.lock().unwrap_or_else(|e| e.into_inner()).get(&item) == Some(&key);
    if current && have.is_some() {
        return have;
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return have;
        }
        pend.push(pkey);
    }
    let (peaks, keys, pending) = (app.tl.peaks.clone(), app.tl.seq_peak_keys.clone(), app.tl.peaks_pending.clone());
    let project = app.session.project.clone();
    let provider = app.session.media.provider(project.clone(), app.session.services.clone());
    let run = move || {
        // a panic in the mix must not take the app down with it, nor leave the item pending for good
        let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let seq = project.sequence(item)?;
            let sr = seq.settings.sample_rate.max(1);
            let total = seq.duration().to_units_floor(sr as i64).max(0) as usize;
            // one peak per 256 samples at 48 kHz, whatever the sequence's own rate
            let bucket = ((256u64 * sr as u64) / 48_000).max(1) as usize;
            let chunk = (bucket * 750).max(1);
            let mut out = Vec::with_capacity(total / bucket + 1);
            let mut s = 0usize;
            while s < total {
                let n = chunk.min(total - s);
                let buf = filmcraft_render::audio::mix_sequence(&project, seq, s as i64, n, &provider);
                let ch = buf.channels.first()?;
                for c in ch.chunks(bucket) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
                s += n;
            }
            Some(out)
        }));
        if let Ok(Some(out)) = mixed {
            peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(out));
            keys.lock().unwrap_or_else(|e| e.into_inner()).insert(item, key);
        }
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != pkey);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    have
}

/// Min/max peaks (one per 256 samples at 48 kHz) of audio stream `stream` of `src`, `dur` long.
pub(super) fn source_peaks(src: &dyn filmcraft_media::MediaSource, stream: usize, dur: Tick) -> Vec<(f32, f32)> {
    let sr = 48_000u32;
    let total = dur.to_units_floor(sr as i64).max(0) as usize;
    let mut out = Vec::with_capacity(total / 256 + 1);
    let chunk = 48_000 * 4;
    let mut s = 0usize;
    while s < total {
        let n = chunk.min(total - s);
        match src.audio_stream(stream, s as i64, n, sr) {
            Ok(buf) => {
                let ch = buf.channels.first().cloned().unwrap_or_default();
                for c in ch.chunks(256) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
            }
            Err(_) => break,
        }
        s += n;
    }
    out
}

/// Waveform peaks of audio stream `stream` of `item` (a clip's `audio_stream`).
pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId, stream: usize) -> Option<Arc<Vec<(f32, f32)>>> {
    if app.session.project.sequence(item).is_some() {
        return request_sequence_peaks(app, item);
    }
    let pkey = (item, stream);
    if let Some(p) = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&pkey) {
        return Some(p.clone());
    }
    // No source: try again on a later frame instead of leaving the item pending forever.
    let src = app.session.source(item)?;
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return None;
        }
        pend.push(pkey);
    }
    let peaks = app.tl.peaks.clone();
    let pending = app.tl.peaks_pending.clone();
    let dur = src.info().duration;
    let run = move || {
        let out = source_peaks(&*src, stream, dur);
        peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(out));
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != pkey);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    None
}
