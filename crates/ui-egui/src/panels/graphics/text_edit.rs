use super::ime::{graphics_ime_target, ime_event_is_stale, local_reset_destination};
use super::overlay::{TEXT_EDIT_ID, end_edit};
use super::*;

pub(super) fn text_of(spec: &LayerSpec) -> Option<&filmcraft_project::graphic::TextProps> {
    match &spec.content {
        LayerContent::Text(t) => Some(t),
        _ => None,
    }
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i.min(s.len())].char_indices().next_back().map_or(0, |(b, _)| b)
}

fn next_boundary(s: &str, i: usize) -> usize {
    let i = i.min(s.len());
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

/// Whether an event was fully handled or must skip the per-event IME destination update, which
/// mirrors the original loop's `continue` paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventFlow {
    Applied,
    Skip,
}

/// Mutable state for one batch of text-editor events: the committed text and caret/selection.
struct TextEditState {
    ed: GfxEdit,
    text: String,
    changed: bool,
    stop: bool,
    custom_mutations: bool,
}

impl TextEditState {
    /// The caret/anchor pair sorted into a forward `(start, end)` selection range.
    fn selection(&self) -> (usize, usize) {
        (self.ed.caret.min(self.ed.anchor), self.ed.caret.max(self.ed.anchor))
    }

    /// Delete the sorted selection in place, collapse caret and anchor to its start and report
    /// whether anything was removed. This does not set the batch's changed flags; callers do.
    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.text.replace_range(a..b, "");
        self.ed.caret = a;
        self.ed.anchor = a;
        true
    }
}

/// The per-event prologue: sync the live destination, consume IME-reset completions, and reject
/// events while a reset blocks IME input. `None` is the original loop's `continue`.
fn begin_text_event(app: &mut FilmcraftApp, ui: &egui::Ui, state: &TextEditState, target: ImeTarget, event: &egui::Event) -> Option<(bool, bool)> {
    let destination = local_reset_destination(app, &state.ed, &state.text, target);
    if state.custom_mutations {
        app.sync_live_ime_destination(ui.ctx(), destination);
    }
    if let egui::Event::ImeResetCompleted { request_id } = event {
        if state.custom_mutations && app.ime_reset.complete(ui.ctx(), *request_id, destination) && app.ime_composition_viewport == Some(ui.ctx().viewport_id())
        {
            app.ime_composition = None;
            app.ime_composition_viewport = None;
        }
        return None;
    }
    if !state.custom_mutations || (app.ime_reset.blocked(ui.ctx().viewport_id()) && matches!(event, egui::Event::Ime(_))) {
        return None;
    }
    let from_ime = matches!(event, egui::Event::Ime(_));
    let terminal = matches!(event, egui::Event::Ime(egui::ImeEvent::Commit(_)));
    Some((from_ime, terminal))
}

/// The per-step epilogue: remember or terminal-acknowledge an IME step's destination, or keep
/// the live destination in sync for ordinary application edits.
fn finish_text_event(app: &mut FilmcraftApp, ui: &egui::Ui, state: &TextEditState, target: ImeTarget, from_ime: bool, terminal: bool) {
    let current = local_reset_destination(app, &state.ed, &state.text, target);
    if from_ime {
        if terminal {
            app.ime_reset.terminal(ui.ctx().viewport_id());
        } else {
            app.ime_reset.remember(ui.ctx().viewport_id(), current);
        }
    } else if state.custom_mutations {
        app.sync_live_ime_destination(ui.ctx(), current);
    }
}

/// Track this viewport's IME preedit target. Empty preedit becomes `TerminalPending` so a
/// following empty commit still belongs to the same composition; non-empty becomes `Active`.
fn apply_preedit(app: &mut FilmcraftApp, viewport: egui::ViewportId, target: ImeTarget, text: &str) {
    app.ime_composition_viewport = Some(viewport);
    if text.is_empty() {
        app.ime_composition = Some(crate::ImeComposition::TerminalPending(target));
    } else {
        app.ime_composition = Some(crate::ImeComposition::Active(target));
    }
}

/// Apply a UTF-8 byte-count surrounding deletion after the staleness check. Only whole
/// characters fully inside the reported byte ranges are removed, the selection is preserved
/// and a stale event skips.
fn apply_delete_surrounding_bytes(app: &FilmcraftApp, state: &mut TextEditState, target: ImeTarget, before_bytes: usize, after_bytes: usize) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    let (a, b) = state.selection();
    let (before, after) = surrounding_byte_deletion_ranges(&state.text, a, b, before_bytes, after_bytes);
    if !before.is_empty() || !after.is_empty() {
        apply_surrounding_deletion(&mut state.text, &mut state.ed, before, after);
        state.changed = true;
    }
    EventFlow::Applied
}

/// Apply a character-count surrounding deletion after the staleness check. The selection is
/// preserved, deletion stops at either text end, and a stale event skips.
fn apply_delete_surrounding_chars(app: &FilmcraftApp, state: &mut TextEditState, target: ImeTarget, before_chars: usize, after_chars: usize) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    let (a, b) = state.selection();
    let before = prev_n_chars(&state.text, a, before_chars)..a;
    let after = b..next_n_chars(&state.text, b, after_chars);
    if !before.is_empty() || !after.is_empty() {
        apply_surrounding_deletion(&mut state.text, &mut state.ed, before, after);
        state.changed = true;
    }
    EventFlow::Applied
}

/// Insert application-origin typing or paste: strip control characters except newline,
/// replace the selection and move the caret to the end. Input that sanitizes to nothing is
/// skipped.
fn apply_text_input(state: &mut TextEditState, input: &str) -> EventFlow {
    let s: String = input.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    if s.is_empty() {
        return EventFlow::Skip;
    }
    state.delete_selection();
    let at = state.ed.caret;
    state.text.insert_str(at, &s);
    state.ed.caret += s.len();
    state.ed.anchor = state.ed.caret;
    state.changed = true;
    EventFlow::Applied
}

/// Apply an IME commit after the staleness check: clear the composition, insert sanitized
/// committed text, or delete the selection when the commit is empty; a stale commit skips.
fn apply_ime_commit(app: &mut FilmcraftApp, state: &mut TextEditState, target: ImeTarget, input: &str) -> EventFlow {
    if ime_event_is_stale(app, target) {
        return EventFlow::Skip;
    }
    app.ime_composition = None;
    app.ime_composition_viewport = None;
    let s: String = input.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    if s.is_empty() {
        // A commit replaces the selection even when the input method produced no text
        // (for example DeleteSurroundingBytes followed by an empty commit). An empty
        // commit with no selection stays a no-op.
        if state.delete_selection() {
            state.changed = true;
        }
    } else {
        state.delete_selection();
        let at = state.ed.caret;
        state.text.insert_str(at, &s);
        state.ed.caret += s.len();
        state.ed.anchor = state.ed.caret;
        state.changed = true;
    }
    EventFlow::Applied
}

/// Copy the sorted selection, and for Cut delete it. An empty selection is a no-op.
fn apply_copy_cut(ui: &egui::Ui, state: &mut TextEditState, cut: bool) -> EventFlow {
    let (a, b) = state.selection();
    if a < b {
        ui.ctx().copy_text(state.text[a..b].to_string());
        if cut && state.delete_selection() {
            state.changed = true;
        }
    }
    EventFlow::Applied
}

/// Caret destination for Left: with no Shift collapse a selection to its start, otherwise
/// move to the line start (Command), previous word (Alt) or previous character boundary.
fn left_target(state: &TextEditState, layout: &filmcraft_text::Layout, shift: bool, command: bool, alt: bool) -> usize {
    let (a, _) = state.selection();
    if !shift && state.ed.caret != state.ed.anchor {
        a
    } else if command {
        layout.lines[layout.caret(state.ed.caret).2].range.start
    } else if alt {
        word_left(&state.text, state.ed.caret)
    } else {
        prev_boundary(&state.text, state.ed.caret)
    }
}

/// Caret destination for Right: with no Shift collapse a selection to its end, otherwise
/// move to the line end (Command), next word (Alt) or next character boundary.
fn right_target(state: &TextEditState, layout: &filmcraft_text::Layout, shift: bool, command: bool, alt: bool) -> usize {
    let (_, b) = state.selection();
    if !shift && state.ed.caret != state.ed.anchor {
        b
    } else if command {
        layout.lines[layout.caret(state.ed.caret).2].range.end
    } else if alt {
        word_right(&state.text, state.ed.caret)
    } else {
        next_boundary(&state.text, state.ed.caret)
    }
}

/// Caret destination for Up/Down: hit-test the same x on the adjacent line's baseline,
/// clamping to the text start/end when there is no line in that direction.
fn vertical_target(state: &TextEditState, layout: &filmcraft_text::Layout, up: bool) -> usize {
    let (x, _, line) = layout.caret(state.ed.caret);
    let target = if up { line.checked_sub(1) } else { Some(line + 1).filter(|next| *next < layout.lines.len()) };
    match target {
        Some(next) => layout.hit(x, layout.lines[next].baseline),
        None if up => 0,
        None => state.text.len(),
    }
}

/// Apply a layout-derived caret destination after re-clamping it to a UTF-8 boundary in the
/// current (possibly already edited) text. Without Shift the anchor collapses to the caret;
/// any actual selection change invalidates the IME composition.
fn apply_moved_caret(app: &mut FilmcraftApp, state: &mut TextEditState, moved: Option<usize>, shift: bool) {
    if let Some(m) = moved {
        let previous_selection = (state.ed.caret, state.ed.anchor);
        // `m` comes from the layout of the text at the start of this batch, but an
        // earlier Text/Paste/IME event may already have changed `text`; clamp to a
        // UTF-8 boundary in the current text before a later event slices or inserts.
        state.ed.caret = char_boundary_floor(&state.text, m);
        if !shift {
            state.ed.anchor = state.ed.caret;
        }
        if previous_selection != (state.ed.caret, state.ed.anchor) {
            app.invalidate_ime_composition();
        }
    }
}

/// Apply one non-IME key press to the local edit: Enter/Backspace/Delete mutate text,
/// Command+A selects all and arrows/Home/End move the caret. Text edits mark an application
/// mutation; Escape stops editing and suspends IME input. Only Escape returns `Skip`.
fn apply_key_event(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    state: &mut TextEditState,
    layout: &filmcraft_text::Layout,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> EventFlow {
    let shift = modifiers.shift;
    let mut moved = None;
    match key {
        egui::Key::Escape => {
            // Escape cancels the edit now: a commit later in the same raw batch must
            // not still be inserted after the user cancelled.
            state.stop = true;
            state.custom_mutations = false;
            app.suspend_ime_input(ui.ctx());
            return EventFlow::Skip;
        }
        egui::Key::Enter => {
            state.delete_selection();
            let at = state.ed.caret;
            state.text.insert(at, '\n');
            state.ed.caret += 1;
            state.ed.anchor = state.ed.caret;
            state.changed = true;
        }
        egui::Key::Backspace => {
            let mut did = state.delete_selection();
            if !did && state.ed.caret > 0 {
                let p = if modifiers.alt || modifiers.command { word_left(&state.text, state.ed.caret) } else { prev_boundary(&state.text, state.ed.caret) };
                let end = state.ed.caret;
                state.text.replace_range(p..end, "");
                state.ed.caret = p;
                state.ed.anchor = p;
                did = true;
            }
            if did {
                state.changed = true;
            }
        }
        egui::Key::Delete => {
            let mut did = state.delete_selection();
            if !did && state.ed.caret < state.text.len() {
                let n = next_boundary(&state.text, state.ed.caret);
                let at = state.ed.caret;
                state.text.replace_range(at..n, "");
                did = true;
            }
            if did {
                state.changed = true;
            }
        }
        egui::Key::A if modifiers.command => {
            let moved = (state.ed.anchor, state.ed.caret) != (0, state.text.len());
            state.ed.anchor = 0;
            state.ed.caret = state.text.len();
            if moved {
                app.invalidate_ime_composition();
            }
        }
        egui::Key::ArrowLeft => {
            moved = Some(left_target(state, layout, shift, modifiers.command, modifiers.alt));
        }
        egui::Key::ArrowRight => {
            moved = Some(right_target(state, layout, shift, modifiers.command, modifiers.alt));
        }
        egui::Key::ArrowUp | egui::Key::ArrowDown => {
            moved = Some(vertical_target(state, layout, key == egui::Key::ArrowUp));
        }
        egui::Key::Home => moved = Some(layout.lines[layout.caret(state.ed.caret).2].range.start),
        egui::Key::End => moved = Some(layout.lines[layout.caret(state.ed.caret).2].range.end),
        _ => {}
    }
    apply_moved_caret(app, state, moved, shift);
    EventFlow::Applied
}

/// One step for the editor, in the order the platform sent its events: an edit from
/// [`craft_ui::ime::Composer`], or an empty input-method commit with nothing composing, which
/// the composer drops but which replaces the selection here.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TextStep {
    Edit(craft_ui::ime::Edit),
    EmptyCommit,
}

/// Run the admitted events through the composer. Returns the steps and whether the composer
/// ended a composition the platform input method may still hold.
fn compose(composer: &mut craft_ui::ime::Composer, events: &[egui::Event], time: f64) -> (Vec<TextStep>, bool) {
    let mut steps = Vec::new();
    let mut interrupted = false;
    let mut start = 0;
    for (i, event) in events.iter().enumerate() {
        if !matches!(event, egui::Event::Ime(egui::ImeEvent::Commit(text)) if text.is_empty()) {
            continue;
        }
        steps.extend(composer.process(events.get(start..i).unwrap_or_default(), time).edits.into_iter().map(TextStep::Edit));
        interrupted |= composer.interrupted();
        if composer.is_composing() {
            start = i;
        } else {
            let _ = composer.process(std::slice::from_ref(event), time);
            steps.push(TextStep::EmptyCommit);
            start = i.saturating_add(1);
        }
    }
    steps.extend(composer.process(events.get(start..).unwrap_or_default(), time).edits.into_iter().map(TextStep::Edit));
    (steps, interrupted || composer.interrupted())
}

/// Whether a step is the input method's (`from_ime`) and ends its composition (`terminal`). An
/// insert is the input method's when it is one of the batch's committed texts, which it takes.
fn step_origin(step: &TextStep, commits: &mut Vec<String>) -> (bool, bool) {
    use craft_ui::ime::Edit;
    match step {
        TextStep::EmptyCommit => (true, true),
        TextStep::Edit(Edit::Insert(text)) => match commits.iter().position(|commit| commit == text) {
            Some(i) => {
                commits.remove(i);
                (true, true)
            }
            None => (false, false),
        },
        TextStep::Edit(Edit::Preedit { .. } | Edit::DeleteSurrounding { .. } | Edit::DeleteSurroundingBytes { .. }) => (true, false),
        TextStep::Edit(_) => (false, false),
    }
}

/// Apply one step. `Skip` is a stale or empty input-method or text step, which leaves the IME
/// destination as it was.
fn apply_text_step(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    state: &mut TextEditState,
    target: ImeTarget,
    layout: &filmcraft_text::Layout,
    step: TextStep,
    from_ime: bool,
) -> EventFlow {
    use craft_ui::ime::Edit;
    let viewport = ui.ctx().viewport_id();
    let edit = match step {
        TextStep::EmptyCommit => return apply_ime_commit(app, state, target, ""),
        TextStep::Edit(edit) => edit,
    };
    match edit {
        Edit::Preedit { text, .. } => {
            apply_preedit(app, viewport, target, &text);
            EventFlow::Applied
        }
        // text-input-v3/Wayland delete-surrounding arrives in UTF-8 bytes. Only whole
        // characters fully inside the reported range are deleted: a byte count that ends
        // inside a character leaves that character (the fork's contract), so a combining
        // accent or the letter next to an emoji is preserved.
        Edit::DeleteSurroundingBytes { before_bytes, after_bytes } => apply_delete_surrounding_bytes(app, state, target, before_bytes, after_bytes),
        Edit::DeleteSurrounding { before_chars, after_chars } => apply_delete_surrounding_chars(app, state, target, before_chars, after_chars),
        // an input method (Chinese, Japanese, Korean) hands over its finished text as a commit (#1083)
        Edit::Insert(text) if from_ime => apply_ime_commit(app, state, target, &text),
        // ordinary typing, paste and marked text the composer kept as typed are application-origin
        Edit::Insert(text) | Edit::Paste(text) => apply_text_input(state, &text),
        Edit::Copy => apply_copy_cut(ui, state, false),
        Edit::Cut => apply_copy_cut(ui, state, true),
        Edit::Key { key, modifiers, .. } => apply_key_event(app, ui, state, layout, key, modifiers),
        Edit::LineBreak { modifiers } => apply_key_event(app, ui, state, layout, egui::Key::Enter, modifiers),
    }
}

/// Keyboard handling while a text layer is being edited.
pub(super) fn text_edit(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &LayerView, ed: GfxEdit, resp: &egui::Response) {
    let _ = resp;
    let window_focused = ui.input(|input| input.viewport().focused.unwrap_or(input.focused));
    if !window_focused || app.ime_modal_blocks_input() || !ui.is_enabled() {
        ime_off(app, ui);
        return;
    }
    // moving to another panel ends the edit; Properties and Essential Graphics style the text
    // being edited, so working there keeps it (#683). Only a move counts: an edit started with
    // another panel focused (an agent's `ui.set`) goes on until the focus moves.
    use crate::dock::PanelKind;
    let panel_id = egui::Id::unique(TEXT_EDIT_ID).with("panel");
    let before = ui.data(|d| d.get_temp::<PanelKind>(panel_id));
    ui.data_mut(|d| d.insert_temp(panel_id, app.ui.focused));
    if before.is_some_and(|b| b != app.ui.focused) && !matches!(app.ui.focused, PanelKind::Program | PanelKind::Properties | PanelKind::EssentialGraphics) {
        end_edit(app, ui);
        ime_off(app, ui);
        return;
    }
    let eid = egui::Id::unique(TEXT_EDIT_ID);
    // a real (non-clickable) widget holds keyboard focus so shortcuts pause while typing
    let fr = ui.interact(Rect::from_points(&v.quad()), eid, Sense::focusable_noninteractive());
    fr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Text layer"));
    // while another field has the keyboard (a value typed in Properties) the keys are its own;
    // the text takes the keyboard back once nothing has it (#683)
    if ui.memory(|m| m.focused().is_some_and(|f| f != eid)) {
        ime_off(app, ui);
        return;
    }
    ui.memory_mut(|m| {
        m.request_focus(eid);
        m.set_focus_lock_filter(eid, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
    });
    let Some(tp) = text_of(&v.spec) else {
        ime_off(app, ui);
        return;
    };
    let mut state = TextEditState { ed, text: tp.text.clone(), changed: false, stop: false, custom_mutations: true };
    // `ui.set.gfxEdit` accepts arbitrary byte offsets. Clamp both ends to UTF-8 boundaries here,
    // before any slice/replace_range/insert_str, so hostile offsets can never panic the editor.
    clamp_edit_offsets(&state.text, &mut state.ed);
    let l = text_layout(tp);
    let (events, time) = ui.input(|i| (i.events.clone(), i.time));
    let sequence = app.session.state.active_sequence.map_or(0, |sequence| sequence.0);
    let composition_target = graphics_ime_target(sequence, state.ed.clip, state.ed.layer, v.spec.uid, app.ime_replacement_epoch);
    let viewport = ui.ctx().viewport_id();
    if app.ime_composer_target != Some(composition_target) {
        app.ime_composer.discard(ui.ctx());
        app.ime_composer = craft_ui::ime::Composer::new().with_owner(composition_target.owner);
        app.ime_composer_target = Some(composition_target);
    }
    // A command, a control request or a caret move ended the composition: the composer drops
    // its copy of the marked text, and a late commit of it.
    if app.ime_composer.is_composing()
        && !(app.ime_composition == Some(crate::ImeComposition::Active(composition_target)) && app.ime_composition_viewport == Some(viewport))
    {
        app.ime_composer.discard(ui.ctx());
    }
    // The composer turns the IME on and publishes its descriptor; the editor runs the events
    // through it below, between the fence's per-event checks.
    let held = ui.ctx().input_mut(|i| std::mem::take(&mut i.events));
    let _ = app.ime_composer.update(ui.ctx(), true);
    ui.ctx().input_mut(|i| i.events = held);
    // Reset completions and the fence's admission run per raw event. A pointer press is
    // handled later in this pass (caret placement, another layer, or ending the edit): the
    // composer sees it, so a composition keeps its marked text, and the events after it
    // belong to the new target.
    let mut admitted = Vec::new();
    let mut pressed = false;
    for ev in events {
        if begin_text_event(app, ui, &state, composition_target, &ev).is_none() {
            continue;
        }
        pressed = matches!(ev, egui::Event::PointerButton { pressed: true, .. });
        admitted.push(ev);
        if pressed {
            break;
        }
    }
    let mut commits: Vec<String> = admitted
        .iter()
        .filter_map(|ev| match ev {
            egui::Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        })
        .collect();
    let (steps, interrupted) = compose(&mut app.ime_composer, &admitted, time);
    for step in steps {
        if !state.custom_mutations {
            break;
        }
        let (from_ime, terminal) = step_origin(&step, &mut commits);
        app.sync_live_ime_destination(ui.ctx(), local_reset_destination(app, &state.ed, &state.text, composition_target));
        if from_ime && app.ime_reset.blocked(viewport) {
            continue;
        }
        if apply_text_step(app, ui, &mut state, composition_target, &l, step, from_ime) == EventFlow::Skip {
            continue;
        }
        finish_text_event(app, ui, &state, composition_target, from_ime, terminal);
    }
    if pressed {
        state.custom_mutations = false;
        app.suspend_ime_input(ui.ctx());
    } else if interrupted && matches!(app.ime_composition, Some(crate::ImeComposition::Active(_) | crate::ImeComposition::TerminalPending(_))) {
        // The composer kept the marked text as typed (a bare line break confirmed it): the
        // platform input method drops its copy.
        app.invalidate_ime_composition();
    }
    // Written now, not queued with the pass's other actions: an edit that ends later in this pass
    // (Escape, a click elsewhere, another layer) types the marked text into this text.
    if state.changed && state.text != tp.text {
        let text = std::mem::take(&mut state.text);
        if let Err(e) = app.session.execute("graphics.setText", json!({"clip": state.ed.clip, "layer": state.ed.layer, "text": text, "merge": true})) {
            app.ui.status = e.to_string();
        }
    }
    app.ui.gfx_edit = Some(state.ed);
    if state.stop {
        end_edit(app, ui);
    }
}

/// The editor doesn't take the keyboard in this pass: the composer turns the IME off, and the
/// fence's controls stay inert.
fn ime_off(app: &mut FilmcraftApp, ui: &egui::Ui) {
    let _ = app.ime_composer.update(ui.ctx(), false);
    app.suspended_ime_controls(ui.ctx());
}

/// Clamp an edit caret/anchor to valid UTF-8 boundaries (hostile `ui.set.gfxEdit` offsets).
pub(super) fn clamp_edit_offsets(text: &str, ed: &mut GfxEdit) {
    ed.caret = char_boundary_floor(text, ed.caret);
    ed.anchor = char_boundary_floor(text, ed.anchor);
}

/// The two ranges `DeleteSurroundingBytes` deletes: whole characters fully contained in the
/// bytes before and after the sorted selection. The selection itself and any pre-edit (absent
/// from FilmCraft's committed model) are excluded. A byte count that ends inside a character
/// leaves that character whole.
pub(super) fn surrounding_byte_deletion_ranges(
    text: &str,
    a: usize,
    b: usize,
    before_bytes: usize,
    after_bytes: usize,
) -> (core::ops::Range<usize>, core::ops::Range<usize>) {
    let a = char_boundary_floor(text, a);
    let b = char_boundary_ceil(text, b).max(a);
    let from = a.saturating_sub(before_bytes);
    let before_chars = text[..a].char_indices().rev().take_while(|(index, _)| *index >= from).count();
    let after_chars = text[b..].char_indices().take_while(|(index, c)| index + c.len_utf8() <= after_bytes).count();
    (prev_n_chars(text, a, before_chars)..a, b..next_n_chars(text, b, after_chars))
}

/// Apply the two delete-surrounding ranges while preserving the selection's direction.
///
/// Delete after the selection first, then before it, so the selection remains intact and both
/// endpoints shift only by the preceding deletion. This mirrors the stock `TextEdit`: a
/// surrounding deletion does not consume the selection, and the commit that usually follows
/// replaces it.
pub(super) fn apply_surrounding_deletion(text: &mut String, ed: &mut GfxEdit, before: core::ops::Range<usize>, after: core::ops::Range<usize>) {
    let before_end = before.end;
    let before_len = before.len();
    if !after.is_empty() {
        text.replace_range(after, "");
    }
    if !before.is_empty() {
        text.replace_range(before, "");
    }
    let shift = |offset: usize| {
        let offset = if offset >= before_end { offset - before_len } else { offset };
        offset.min(text.len())
    };
    ed.caret = shift(ed.caret);
    ed.anchor = shift(ed.anchor);
}

/// Largest char boundary not after `i`.
pub(super) fn char_boundary_floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary not before `i`.
pub(super) fn char_boundary_ceil(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Move `i` to a character boundary and then back up to `n` whole characters, stopping at
/// the start. Hostile offsets and counts are clamped instead of slicing inside a character.
pub(super) fn prev_n_chars(s: &str, mut i: usize, n: usize) -> usize {
    i = char_boundary_floor(s, i);
    for _ in 0..n {
        if i == 0 {
            break;
        }
        i = match s[..i].char_indices().next_back() {
            Some((j, _)) => j,
            None => 0,
        };
    }
    i
}

/// Move `i` to a character boundary and then forward up to `n` whole characters, stopping at
/// the end. Hostile offsets and counts are clamped instead of slicing inside a character.
pub(super) fn next_n_chars(s: &str, mut i: usize, n: usize) -> usize {
    i = char_boundary_ceil(s, i);
    for _ in 0..n {
        if i >= s.len() {
            break;
        }
        i = match s[i..].char_indices().nth(1) {
            Some((j, _)) => i + j,
            None => s.len(),
        };
    }
    i
}

fn word_left(s: &str, i: usize) -> usize {
    let mut j = i;
    let b: Vec<(usize, char)> = s[..i].char_indices().collect();
    let mut k = b.len();
    while k > 0 && b[k - 1].1.is_whitespace() {
        k -= 1;
    }
    while k > 0 && !b[k - 1].1.is_whitespace() {
        k -= 1;
    }
    if k < b.len() {
        j = b[k].0;
    }
    if k == 0 { 0 } else { j }
}

fn word_right(s: &str, i: usize) -> usize {
    let mut it = s[i..].char_indices().peekable();
    while it.peek().is_some_and(|(_, c)| c.is_whitespace()) {
        it.next();
    }
    while it.peek().is_some_and(|(_, c)| !c.is_whitespace()) {
        it.next();
    }
    it.peek().map_or(s.len(), |(b, _)| i + b)
}
