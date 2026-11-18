use super::layer_identity::{current_layer_fingerprint, current_layer_layout_fingerprint, current_layer_text, current_layer_uid, layer_fingerprint};
use super::overlay::TEXT_EDIT_ID;
use super::text_edit::{char_boundary_floor, text_of};
use super::{ClipId, Color32, FilmcraftApp, GfxEdit, ImeTarget, LayerView, Pos2, Rect, Session, Stroke, layer_indices, pos2, vec2};

/// The Program monitor editor drawn in one pass, remembered so the end-of-pass IME check can run
/// after that pass's control requests have been drained. It holds only geometry, never a copy of
/// the layer's text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawnImeEditor {
    pub viewport: egui::ViewportId,
    pub pass_nr: u64,
    pub document_epoch: u64,
    pub sequence: Option<u64>,
    pub target: ImeTarget,
    pub clip: u64,
    pub layer: usize,
    /// Identity of the layer drawn at that index; a reorder after the draw is detected here even
    /// when the numeric index (and therefore a uid-less owner key) stayed the same.
    pub fingerprint: u64,
    /// Identity of the committed text drawn in this pass. A control request that changes the
    /// text after the draw must not publish the old cursor geometry with the new text; the next
    /// pass draws and publishes it. A caret/selection-only change keeps the text, so the IME
    /// context stays.
    pub text_stamp: u64,
    /// Every layout-affecting layer property drawn in this pass, evaluated at the playhead. A
    /// same-pass control request can change the font, style, alignment, spacing, box or transform
    /// without changing the committed text or the layer identity; the cursor rectangle from the
    /// old layout must then not be published with the new one.
    pub layout_fingerprint: u64,
}

/// The most marked text drawn; an input method's composition is a few words.
const MAX_PREEDIT_BYTES: usize = 1024;

/// What [`draw_preedit`] drew, for automation: the plate with the marked text, and the active
/// clause's rectangle and text.
pub(super) struct DrawnPreedit {
    pub(super) plate: Rect,
    pub(super) text: String,
    pub(super) clause: Option<(Rect, String)>,
}

/// Draw the marked text from the caret line `top`–`bottom` until it commits: on a dark plate,
/// underlined, with a heavier underline under the clause being converted, or a caret where an
/// empty active range puts it.
pub(super) fn draw_preedit(painter: &egui::Painter, top: Pos2, bottom: Pos2, preedit: craft_ui::ime::Preedit<'_>) -> DrawnPreedit {
    let text = preedit.text.get(..char_boundary_floor(preedit.text, MAX_PREEDIT_BYTES)).unwrap_or_default();
    let chars = text.chars().count();
    let active = preedit.active.map(|r| {
        let start = r.start.min(chars);
        start..r.end.clamp(start, chars)
    });
    let height = (bottom - top).length();
    let size = if height.is_finite() { (height * 0.8).clamp(8.0, 256.0) } else { 12.0 };
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(size), Color32::WHITE);
    // centred on the caret line, where the committed text will be
    let rise = bottom.y - top.y;
    let at = top + vec2(0.0, if rise.is_finite() { (rise - galley.size().y) / 2.0 } else { 0.0 });
    let plate = Rect::from_min_size(at, galley.size());
    painter.rect_filled(plate.expand(2.0), 2.0, Color32::from_black_alpha(160));
    let x = |i: usize| at.x + galley.pos_from_cursor(egui::text::CCursor::new(i)).min.x;
    let y = plate.bottom();
    painter.line_segment([pos2(plate.left(), y), pos2(plate.right(), y)], Stroke::new(1.0, Color32::WHITE));
    let mut clause = None;
    match active {
        Some(r) if r.start < r.end => {
            let (from, to) = (x(r.start), x(r.end));
            painter.line_segment([pos2(from, y), pos2(to, y)], Stroke::new(3.0, Color32::WHITE));
            clause = Some((Rect::from_min_max(pos2(from, plate.top()), pos2(to, y)), text.chars().skip(r.start).take(r.len()).collect()));
        }
        Some(r) => {
            let caret = x(r.start);
            painter.line_segment([pos2(caret, plate.top()), pos2(caret, y)], Stroke::new(1.5, Color32::WHITE));
        }
        None => {}
    }
    painter.galley(at, galley, Color32::WHITE);
    DrawnPreedit { plate, text: text.to_string(), clause }
}

/// Identity of committed text alone (no caret or selection).
fn ime_text_stamp(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in text.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ (text.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Remember the focused Program monitor editor drawn in this pass, for [`finish_graphics_ime`]
/// after the pass's control requests have run, and put the candidate window at its caret.
pub(crate) fn note_graphics_ime_editor(app: &mut FilmcraftApp, ui: &egui::Ui, v: &LayerView, cursor_rect: Rect) {
    let ctx = ui.ctx();
    app.ime_composer.set_caret(ctx, cursor_rect);
    let viewport = ctx.viewport_id();
    let pass_nr = ctx.cumulative_pass_nr_for(viewport);
    let sequence = app.session.state.active_sequence.map(|sequence| sequence.0);
    let target = graphics_ime_target(sequence.unwrap_or(0), v.clip.0, v.layer, v.spec.uid, app.ime_replacement_epoch);
    let fingerprint = current_layer_fingerprint(&app.session, v.clip.0, v.layer).unwrap_or(0);
    let layout_fingerprint = current_layer_layout_fingerprint(&app.session, v.clip.0, v.layer).unwrap_or(0);
    let text_stamp = text_of(&v.spec).map_or(0, |tp| ime_text_stamp(&tp.text));
    app.ime_drawn_editor = Some(DrawnImeEditor {
        viewport,
        pass_nr,
        document_epoch: app.session.document_epoch,
        sequence,
        target,
        clip: v.clip.0,
        layer: v.layer,
        fingerprint,
        text_stamp,
        layout_fingerprint,
    });
}

/// The end-of-pass check of the editor drawn in `ctx`'s viewport, after the pass's commands
/// ran. Returns whether the editor drawn there in this pass keeps the composer's descriptor.
/// Otherwise the composer turns the IME off: when the editor was drawn there, or, with `last`
/// (the main window's pass, which ends after every native window's), when it was drawn nowhere.
///
/// The composer's descriptor goes only with the editor's current target: a document, sequence,
/// target, text or layout replaced since the overlay drew turns it off for this pass instead of
/// pairing the old caret with the new model.
pub(crate) fn finish_graphics_ime(app: &mut FilmcraftApp, ctx: &egui::Context, last: bool) -> bool {
    let viewport = ctx.viewport_id();
    let drawn = match app.ime_drawn_editor {
        Some(drawn) if drawn.viewport != viewport => return false,
        None if !last => return false,
        drawn => drawn.filter(|drawn| drawn.pass_nr == ctx.cumulative_pass_nr_for(viewport)),
    };
    let eligible = drawn.is_some_and(|drawn| graphics_ime_eligible(app, ctx, &drawn));
    if !eligible {
        // The editor is gone or replaced: the marked text goes with it.
        let _ = app.ime_composer.update(ctx, false);
        return false;
    }
    app.note_ime_viewport(viewport);
    // In the main window the composer wrote its descriptor itself; a native window's goes out
    // after its last body.
    let owner = app.ime_composer_target.map(|target| target.owner);
    if let Some(output) = ctx.output(|o| o.ime.as_ref().filter(|ime| Some(ime.owner) == owner).map(crate::ime_reset::OutputContext::of)) {
        app.ime_reset.produced(output);
    }
    true
}

/// Whether the editor drawn in this pass is still the one being edited, unchanged since it drew.
fn graphics_ime_eligible(app: &mut FilmcraftApp, ctx: &egui::Context, drawn: &DrawnImeEditor) -> bool {
    // A picker or in-app modal opened after the overlay drew owns input for the rest of this
    // pass, so this editor cannot attach an already queued reset under the modal. Core may still
    // carry that un-taken request to a later stock descriptor (the accepted attachment
    // limitation); the next eligible pass rearms this guard explicitly.
    if app.ime_modal_blocks_input() {
        return false;
    }
    if drawn.document_epoch != app.session.document_epoch || drawn.sequence != app.session.state.active_sequence.map(|sequence| sequence.0) {
        return false;
    }
    if ctx.memory(|m| m.focused() != Some(egui::Id::unique(TEXT_EDIT_ID))) {
        return false;
    }
    let Some(ed) = app.ui.gfx_edit.clone() else { return false };
    if ed.clip != drawn.clip || ed.layer != drawn.layer {
        return false;
    }
    let (Some(sequence), Some(uid)) = (app.session.state.active_sequence, current_layer_uid(&app.session, ed.clip, ed.layer)) else {
        return false;
    };
    let target = graphics_ime_target(sequence.0, ed.clip, ed.layer, uid, app.ime_replacement_epoch);
    if target != drawn.target || current_layer_fingerprint(&app.session, ed.clip, ed.layer).is_none_or(|fingerprint| fingerprint != drawn.fingerprint) {
        // A control request replaced the edit target, reordered the layers, or changed the
        // document after the overlay drew.
        return false;
    }
    let Some(text) = current_layer_text(&app.session, ed.clip, ed.layer) else { return false };
    if ime_text_stamp(text) != drawn.text_stamp {
        // A same-pass control request changed the text after the overlay drew. The caret
        // belongs to the old text, and so does the composition: its next delete, preedit or
        // commit must not be inserted into the new text.
        app.invalidate_ime_composition();
        return false;
    }
    // A same-pass control request that changed a layout-affecting property (font, style,
    // alignment, spacing, box, transform, per-character style or pin) moved the caret: the next
    // pass draws and publishes the new one. A caret or selection move alone keeps the
    // descriptor, with a caret one pass old.
    current_layer_layout_fingerprint(&app.session, ed.clip, ed.layer).is_some_and(|fingerprint| fingerprint == drawn.layout_fingerprint)
}

/// Follow the edited layer through a real project edit (reorder, delete, undo/redo).
///
/// A stable uid wins. For a uid-less layer the compact fingerprint finds the same layer at its
/// new index; with no candidate the layer was deleted (or replaced), so the edit is ended rather
/// than silently retargeted to whatever now occupies the old index.
fn resolve_gfx_edit(session: &Session, ed: &GfxEdit) -> Option<(usize, u64, u64)> {
    let seq = session.active_sequence()?;
    let (_, it) = seq.find_item(ClipId(ed.clip))?;
    let indices = layer_indices(&it.effects);
    let resolved = if ed.uid != 0 {
        indices.iter().position(|&ei| it.effects.get(ei).and_then(|e| e.layer.as_ref()).is_some_and(|x| x.uid == ed.uid))
    } else {
        let at = |i: usize| it.effects.get(indices[i]).map(layer_fingerprint);
        let mut found = indices.get(ed.layer).and_then(|_| at(ed.layer)).filter(|fp| ed.fingerprint == 0 || *fp == ed.fingerprint).map(|_| ed.layer);
        if found.is_none() && ed.fingerprint != 0 {
            let mut best: Option<(usize, usize)> = None;
            for i in 0..indices.len() {
                if at(i) == Some(ed.fingerprint) {
                    let distance = i.abs_diff(ed.layer);
                    if best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, i));
                    }
                }
            }
            found = best.map(|(_, i)| i);
        }
        found
    }?;
    let effect = it.effects.get(*indices.get(resolved)?)?;
    let uid = effect.layer.as_ref().map_or(0, |x| x.uid);
    Some((resolved, uid, layer_fingerprint(effect)))
}

/// Keep `gfx_edit` on the layer it started on. Called before the overlay draws, so a command run
/// between two passes (arrange/delete/undo or a control edit) cannot shift the caret into another
/// layer.
pub(super) fn reconcile_gfx_edit(app: &mut FilmcraftApp) {
    let Some(ed) = app.ui.gfx_edit.clone() else { return };
    match resolve_gfx_edit(&app.session, &ed) {
        Some((layer, uid, fingerprint)) => {
            // A uid-less owner key includes the index, so an index change is a new context. A
            // stable uid layer keeps the same owner and its composition may continue.
            if layer != ed.layer && ed.uid == 0 {
                app.invalidate_ime_composition();
            }
            app.ui.gfx_edit = Some(GfxEdit { layer, uid, fingerprint, ..ed });
        }
        None => {
            app.ui.gfx_edit = None;
            app.invalidate_ime_composition();
        }
    }
}

/// Owner for the graphics text editor: sequence, clip and stable layer uid, falling back to
/// the numeric layer index only when `uid` is 0. The replacement epoch is explicit. Caret,
/// selection, text, revision, geometry, visual order and pointers are excluded so those
/// changes do not churn the IME owner.
pub(super) fn graphics_ime_target(sequence: u64, clip: u64, layer: usize, uid: u64, replacement_epoch: u64) -> ImeTarget {
    // The owner excludes caret, selection, text, revision, geometry, visual order and pointers.
    // `uid` is the stable layer component id; zero means "none", where the layer index is the
    // only component identity available.
    let component_uid = (uid != 0).then_some(uid);
    let component_index = (uid == 0).then_some(layer as u64);
    ImeTarget::new(egui::Id::unique(("filmcraft.graphics.ime", sequence, clip, component_uid, component_index)), replacement_epoch)
}

/// Current editor owner for the layer, using its live uid and the app's replacement epoch;
/// `None` when there is no active sequence or the layer is unavailable.
pub(super) fn current_graphics_ime_target(app: &FilmcraftApp, clip: u64, layer: usize) -> Option<ImeTarget> {
    let sequence = app.session.state.active_sequence?.0;
    let uid = current_layer_uid(&app.session, clip, layer)?;
    Some(graphics_ime_target(sequence, clip, layer, uid, app.ime_replacement_epoch))
}

/// Outside the keyboard reducer the actual engine/UI edit state is authoritative.
pub(crate) fn current_reset_destination(app: &FilmcraftApp) -> Option<crate::ime_reset::Destination> {
    let ed = app.ui.gfx_edit.as_ref()?;
    let target = current_graphics_ime_target(app, ed.clip, ed.layer)?;
    let text = current_layer_text(&app.session, ed.clip, ed.layer)?;
    Some(local_reset_destination(app, ed, text, target))
}

/// Build the admission destination from the local edit and current document epoch, stamping
/// the committed `text` while carrying the caret and anchor separately.
pub(super) fn local_reset_destination(app: &FilmcraftApp, ed: &GfxEdit, text: &str, target: ImeTarget) -> crate::ime_reset::Destination {
    crate::ime_reset::Destination { document: app.session.document_epoch, target, caret: ed.caret, anchor: ed.anchor, text: ime_text_stamp(text) }
}

/// Is this IME commit/delete left over from a composition whose target or caret/focus was
/// interrupted? A terminal-pending composition still owns its pending commit/delete after an empty
/// preedit. `None` (no preedit was seen) still accepts a direct commit.
pub(super) fn ime_event_is_stale(app: &FilmcraftApp, target: ImeTarget) -> bool {
    match app.ime_composition {
        Some(crate::ImeComposition::Stale) => true,
        Some(crate::ImeComposition::Active(active) | crate::ImeComposition::TerminalPending(active)) => active != target,
        None => false,
    }
}
