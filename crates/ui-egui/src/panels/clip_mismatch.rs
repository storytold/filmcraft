//! Settings ▸ Timeline ▸ Show Clip Mismatch Warning dialog: the first clip put in an empty sequence
//! whose frame size, frame rate or pixel aspect ratio differ (a 120 fps phone or game capture in a
//! 23.976 fps sequence) asks whether the sequence takes the clip's settings, for a drag or a file
//! dropped onto the Timeline and for Insert / Overwrite. The answer runs the edit with
//! `matchSequence` ([`filmcraft_engine::commands::clip_mismatch`]); Escape puts nothing in.
//!
//! Automation ids: `clipMismatch.change`, `clipMismatch.keep`.

use egui::{Frame, Margin, RichText};
use filmcraft_engine::project::{ItemId, SequenceSettings};
use serde_json::{Value, json};

use crate::{Dialog, FilmcraftApp};

/// The clip an edit puts in the sequence: `timeline.place`'s `item` (or the first of its `items`),
/// the Source monitor's clip for Insert and Overwrite.
fn clip_of(app: &FilmcraftApp, command: &str, params: &Value) -> Option<ItemId> {
    match command {
        "timeline.place" => params.get("item").and_then(Value::as_u64).or_else(|| params.get("items")?.as_array()?.first()?.as_u64()).map(ItemId),
        "source.insert" | "source.overwrite" => app.session.state.source_item,
        _ => None,
    }
}

/// Opens the warning instead of running `command` when it would put a first clip in an empty
/// sequence that doesn't match it; true when it did. Parameters that already answer it
/// (`matchSequence`) and the setting turned off run the edit as it is.
pub(crate) fn ask(app: &mut FilmcraftApp, command: &str, params: &Value) -> bool {
    if !app.session.prefs.timeline.clip_mismatch_warning || params.get("matchSequence").is_some() {
        return false;
    }
    let Some(item) = clip_of(app, command, params) else { return false };
    if filmcraft_engine::commands::clip_mismatch(&app.session, item).is_none() {
        return false;
    }
    app.clip_mismatch = Some((command.to_string(), params.clone()));
    app.dialog = Some(Dialog::ClipMismatch);
    true
}

/// Runs an edit dropped onto the Timeline, asking first when [`ask`] does.
pub(crate) fn execute(app: &mut FilmcraftApp, command: &str, params: Value) -> Result<Value, String> {
    if ask(app, command, &params) {
        return Ok(json!({"dialog": "clipMismatch"}));
    }
    app.session.execute(command, params).map_err(|e| e.to_string())
}

/// `1920 x 1080 · 120 fps`, with the pixel aspect ratio when it isn't square.
fn describe(st: &SequenceSettings) -> String {
    let (n, d) = st.par;
    let par = if n == d { String::new() } else { format!(" ({:.4})", f64::from(n) / f64::from(d.max(1))) };
    format!("{} x {}{par} · {} fps", st.width, st.height, st.frame_rate.label())
}

/// The warning; returns whether it stays open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let Some((command, params)) = app.clip_mismatch.clone() else { return false };
    // the sequence or the clip may have changed under it (an undo, another sequence opened)
    let clip = clip_of(app, &command, &params).and_then(|i| filmcraft_engine::commands::clip_mismatch(&app.session, i));
    let (Some(clip), Some(seq)) = (clip, app.session.active_sequence().map(|q| q.settings.clone())) else {
        app.clip_mismatch = None;
        return false;
    };
    let t = app.tokens;
    let mut answer = None;
    let r = egui::Modal::new(egui::Id::new("clip-mismatch-modal")).frame(super::file_dialogs::modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(500.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Clip Mismatch Warning")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("This clip does not match the sequence settings.")).size(13.5).color(t.text));
            ui.add_space(8.0);
            ui.label(RichText::new(tlf!("Clip: {settings}", settings = describe(&clip))).size(12.5).color(t.text_dim));
            ui.label(RichText::new(tlf!("Sequence: {settings}", settings = describe(&seq))).size(12.5).color(t.text_dim));
            ui.add_space(8.0);
            ui.label(RichText::new(tl!("Change the sequence to match the clip's settings?")).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if super::file_dialogs::button(app, ui, "clipMismatch.change", tl!("Change Sequence Settings"), true) {
                    answer = Some(true);
                }
                if super::file_dialogs::button(app, ui, "clipMismatch.keep", tl!("Keep Existing Settings"), false) {
                    answer = Some(false);
                }
            });
        });
    });
    if let Some(change) = answer {
        app.clip_mismatch = None;
        let mut params = params;
        if let Some(p) = params.as_object_mut() {
            p.insert("matchSequence".into(), json!(change));
        }
        if let Err(e) = app.session.execute(&command, params) {
            app.ui.status = e.to_string();
        }
        return false;
    }
    // (the modal's own close also fires on Escape: not while a drag is being cancelled, #580)
    if r.should_close() && !ctx.input(|i| i.pointer.any_down()) {
        app.clip_mismatch = None;
        return false;
    }
    true
}
