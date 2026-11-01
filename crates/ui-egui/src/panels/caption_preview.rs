//! Read-only caption navigation and bounded Program auditioning.
use filmcraft_project::{ClipId, ItemId};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;

#[derive(Clone, Debug)]
pub struct Preview {
    epoch: u64,
    sequence: ItemId,
    caption: ClipId,
    start: Tick,
    end: Tick,
    looping: bool,
}

pub fn active(app: &FilmcraftApp, caption: ClipId) -> bool {
    app.playback.playing
        && app
            .caption_preview
            .as_ref()
            .is_some_and(|p| p.caption == caption && p.epoch == app.session.document_epoch && Some(p.sequence) == app.session.state.active_sequence)
}

/// Release only the audition's transport settings. Called by ordinary Play and Stop too.
pub fn release(app: &mut FilmcraftApp) {
    if let Some(p) = app.caption_preview.take() {
        app.playback.looping = p.looping;
        app.playback.stop_at = None;
        app.playback.rest_at = None;
    }
}

/// Also runs when Text is hidden; deleted/re-timed captions and replaced documents cannot keep playing.
pub fn poll(app: &mut FilmcraftApp) {
    let invalid = app.caption_preview.as_ref().is_some_and(|p| {
        !app.playback.playing
            || p.epoch != app.session.document_epoch
            || Some(p.sequence) != app.session.state.active_sequence
            || app.session.active_sequence().and_then(|s| s.find_caption(p.caption)).is_none_or(|(_, c)| c.start != p.start || c.end() != p.end)
    });
    if invalid {
        app.stop();
    }
}

fn recording(app: &FilmcraftApp) -> bool {
    app.session.mixrec.active() || app.session.mcrec.active() || app.session.voiceover.recording()
}

pub fn can_preview(app: &FilmcraftApp) -> bool {
    !recording(app)
}

pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "textPanel.goToCaption" | "textPanel.previewCaption") {
        return None;
    }
    Some(run(app, id, params))
}

fn run(app: &mut FilmcraftApp, id: &str, params: &Value) -> Result<Value, String> {
    let caption = params.get("caption").and_then(Value::as_u64).map(ClipId).ok_or("caption must be an integer id")?;
    let sequence = app.session.state.active_sequence.ok_or("Open a sequence first")?;
    let (start, end) = app.session.active_sequence().and_then(|s| s.find_caption(caption)).map(|(_, c)| (c.start, c.end())).ok_or("Caption is unavailable")?;
    if recording(app) {
        return Err(tl!("Stop recording before navigating or previewing captions.").into());
    }
    if id == "textPanel.previewCaption" && active(app, caption) {
        app.stop();
        return Ok(json!({"playing":false}));
    }
    if id == "textPanel.previewCaption" && (start < Tick::ZERO || end <= start) {
        return Err("Caption has no playable duration".into());
    }
    app.stop_source();
    app.stop_for_scrub();
    app.session.execute("captions.goTo", json!({"caption":caption.0})).map_err(|e| e.to_string())?;
    if id == "textPanel.goToCaption" {
        return Ok(json!({"time":start.0}));
    }
    let looping = app.playback.looping;
    app.play(1.0);
    // Normal Play may restart at sequence end; an explicit caption audition must keep its start.
    app.session.set_playhead(start);
    app.playback.looping = false;
    app.playback.stop_at = Some(end);
    // Rest on the last caption frame, as Play In to Out rests on its inclusive Out point.
    app.playback.rest_at = Some(Tick(end.0.saturating_sub(app.session.sequence_rate().frame_duration().0)).max(start));
    app.caption_preview = Some(Preview { epoch: app.session.document_epoch, sequence, caption, start, end, looping });
    Ok(json!({"playing":true,"start":start.0,"end":end.0}))
}
