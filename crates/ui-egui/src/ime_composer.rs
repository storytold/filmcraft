//! The input-method composer for text typed onto a canvas, outside egui's `TextEdit`.

use std::ops::Range;

use egui::{
    Context, Event, IMEPurpose, Id, ImeEvent, Key, Modifiers, Rect,
    ime_context::{ImeContextTracker, ImeObservation, ImeTarget},
    output::IMEOutput,
};

/// Seconds after a composition ends in which the IME's own confirm, cancel and edit keys
/// (Enter, Escape, Backspace, Delete, Space) are still taken as the IME's, and after a bare
/// commit in which the key that typed it is. Platforms that forward the key do so in the same
/// batch of events; a person's next Enter comes much later.
const GUARD_SECS: f64 = 0.05;

/// Seconds after the composer kept marked text itself (a click away, the canvas giving up the
/// keyboard) in which the IME's own commit of that same text is a duplicate.
const KEPT_SECS: f64 = 0.5;

/// One step for the canvas's text, in the order the platform sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The marked text changed. Empty text ends the composition; an [`Edit::Insert`] of the
    /// committed text, if any, follows. `active` is the segment being converted, in chars.
    Preedit {
        text: String,
        active: Option<Range<usize>>,
    },
    /// Text to insert at the caret: typed, committed by the IME, or marked text kept as typed.
    Insert(String),
    /// A key press for the editor (caret movement, Backspace, Enter, Escape, shortcuts). Keys the
    /// IME is using never appear here. The key's event also stays in the input for the app's
    /// other keyboard handling: an app handles each key from one of the two.
    Key {
        key: Key,
        modifiers: Modifiers,
        repeat: bool,
    },
    /// A line break sent as text or committed with nothing composing (an on-screen keyboard's
    /// Return, an IME's bare commit of `"\n"`). No Enter key press comes with it: the editor
    /// handles it as Enter. The line break a pressed Enter also types is dropped, so each Enter
    /// arrives once, as [`Edit::Key`] or as this.
    LineBreak {
        modifiers: Modifiers,
    },
    /// The clipboard events, in order with the text, held back while the IME composes. The app
    /// applies them; the event also stays in the input, so it handles each from one of the two.
    Paste(String),
    Copy,
    Cut,
    /// Delete chars around the caret (the web text agent's corrections, mobile Backspace). The app
    /// applies it; its event is removed from the input. Never sent while composing.
    DeleteSurrounding {
        before_chars: usize,
        after_chars: usize,
    },
    /// Delete the text an input method replaces (macOS accent menus and dictation, Wayland input
    /// methods), counted in UTF-8 bytes before and after the caret or selection, right before the
    /// commit that replaces it. While composing, the counts start at the caret too: the marked
    /// text isn't in the app's text. Delete only whole chars inside the counts;
    /// [`surrounding_chars`] counts them. The app applies it; its event is removed from the
    /// input.
    DeleteSurroundingBytes {
        before_bytes: usize,
        after_bytes: usize,
    },
}

/// For [`Edit::DeleteSurroundingBytes`]: how many chars of `before` (the text before the caret or
/// selection) end inside its last `before_bytes` bytes, and how many of `after` (the text after
/// it) lie inside its first `after_bytes` bytes. A char cut by a count stays, as in egui's
/// `TextEdit`.
pub fn surrounding_chars(before: &str, after: &str, before_bytes: usize, after_bytes: usize) -> (usize, usize) {
    let from = before.len().saturating_sub(before_bytes);
    let before_chars = before.char_indices().rev().take_while(|(i, _)| *i >= from).count();
    let after_chars = after.char_indices().take_while(|(i, c)| i + c.len_utf8() <= after_bytes).count();
    (before_chars, after_chars)
}

/// The marked text, for painting at the caret with an underline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preedit<'a> {
    pub text: &'a str,
    /// The segment being converted (Japanese, Chinese phrase input), in chars. Draw it with a
    /// thicker underline. `None` when the IME doesn't say.
    pub active: Option<&'a Range<usize>>,
}

/// What [`Composer::process`] made of a pass's events.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Processed {
    pub edits: Vec<Edit>,
    /// The input events with those the IME used removed, for the app's other keyboard handling.
    pub events: Vec<Event>,
}

/// One composer for text typed onto a canvas, outside egui's `TextEdit`.
///
/// It turns a pass's input events into ordered [`Edit`]s, keeps the marked (preedit) text, and
/// tells the platform where the caret is so the candidate window opens there.
///
/// A clone is a new IME context: it gets its own context generation when it next writes output.
#[derive(Debug)]
pub struct Composer {
    preedit: String,
    active: Option<Range<usize>>,
    /// The keys the platform may still forward, and since when, for [`GUARD_SECS`].
    guard: Option<(Guard, f64)>,
    /// Marked text the composer kept itself, and when, for [`KEPT_SECS`].
    kept: Option<(String, f64)>,
    /// The commit whose echo may still come in this pass, for [`Self::is_echo`].
    last_commit: Option<String>,
    modifiers: Modifiers,
    caret: Option<Rect>,
    purpose: IMEPurpose,
    interrupt: bool,
    /// The composer ended a composition the IME may still hold, for [`Self::interrupted`].
    interrupted: bool,
    /// The pass [`Self::interrupted`] is for, and the viewport and pass of the composer's last
    /// IME output.
    pass: Option<u64>,
    output_pass: Option<(egui::ViewportId, u64)>,
    on: bool,
    /// The IME context's owner, as egui's `IMEOutput::owner`.
    owner: Id,
    /// The context generation egui's `IMEOutput` carries: new whenever the composer turns the
    /// IME on again, changes purpose, or skipped a pass.
    context: ImeContextTracker,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            preedit: String::new(),
            active: None,
            guard: None,
            kept: None,
            last_commit: None,
            modifiers: Modifiers::default(),
            caret: None,
            purpose: IMEPurpose::default(),
            interrupt: false,
            interrupted: false,
            pass: None,
            output_pass: None,
            on: false,
            owner: Id::unique("filmcraft::ime_composer::Composer"),
            context: ImeContextTracker::new(),
        }
    }
}

impl Clone for Composer {
    fn clone(&self) -> Self {
        Self {
            preedit: self.preedit.clone(),
            active: self.active.clone(),
            guard: self.guard,
            kept: self.kept.clone(),
            last_commit: self.last_commit.clone(),
            modifiers: self.modifiers,
            caret: self.caret,
            purpose: self.purpose,
            interrupt: self.interrupt,
            interrupted: self.interrupted,
            pass: self.pass,
            output_pass: self.output_pass,
            on: self.on,
            owner: self.owner,
            context: ImeContextTracker::new(),
        }
    }
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The IME owner egui and the platform integration know this canvas's text by. Give each
    /// canvas that types text its own (the default is one id shared by every composer).
    pub fn with_owner(mut self, owner: Id) -> Self {
        self.owner = owner;
        self
    }

    /// `IMEPurpose::Password` or `Terminal` for canvases that need them.
    pub fn with_purpose(mut self, purpose: IMEPurpose) -> Self {
        self.purpose = purpose;
        self
    }

    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    pub fn preedit(&self) -> Option<Preedit<'_>> {
        self.is_composing().then(|| Preedit { text: &self.preedit, active: self.active.as_ref() })
    }

    /// Call once per pass, before the app's other keyboard handling, with whether the canvas
    /// has a text caret. Returns the pass's edits and removes the events the IME used from
    /// `ctx`'s input, so shortcuts don't see the Space that converted a word or the Enter that
    /// confirmed it.
    ///
    /// When `editing` is false, or an egui text field has the keyboard, marked text is kept as
    /// typed (an empty [`Edit::Preedit`], then an [`Edit::Insert`]) and the IME turns off.
    /// When the editing ends later in a pass (a command closes the editor), call it again with
    /// `editing` false: the IME turns off in that pass, not the next.
    pub fn update(&mut self, ctx: &Context, editing: bool) -> Vec<Edit> {
        let pass = self.start_pass(ctx);
        if !editing || ctx.text_edit_focused() {
            self.on = false;
            self.context.observe(ctx.viewport_id(), None);
            if self.output_pass == Some((ctx.viewport_id(), pass)) && !ctx.text_edit_focused() {
                ctx.output_mut(|o| o.ime = None);
            }
            let time = ctx.input(|i| i.time);
            let kept = self.end_at(time);
            // Turning the IME off drops its marked text; there is nothing to interrupt.
            self.interrupt = false;
            return kept.map(|text| vec![Edit::Preedit { text: String::new(), active: None }, Edit::Insert(text)]).unwrap_or_default();
        }
        self.on = true;
        let (events, time, modifiers) = ctx.input(|i| (i.events.clone(), i.time, i.modifiers));
        // Modifiers can change while the composer isn't updated (another view has the keyboard).
        self.modifiers = modifiers;
        let processed = self.process_pass(&events, time);
        if processed.events.len() != events.len() {
            ctx.input_mut(|i| i.events = processed.events);
        }
        self.write_output(ctx);
        processed.edits
    }

    /// The caret in screen points (`ctx.layer_transform_to_global(layer) * rect` for a canvas
    /// on a transformed layer), after this pass's edits moved it. Put it at the start of the
    /// marked text, or of its active segment, so candidates don't cover what is being typed.
    ///
    /// Call it after [`Self::update`] in every pass that edits; until the first call the
    /// candidate window has no position.
    pub fn set_caret(&mut self, ctx: &Context, caret: Rect) {
        self.caret = Some(caret_area(caret, ctx.content_rect()));
        if self.on {
            self.write_output(ctx);
        }
    }

    /// Whether the composer ended a composition the platform IME may still hold, in this pass
    /// (since the last [`Self::process`] for callers that call it directly): it kept the marked
    /// text as typed (a click, the editing ending, [`Self::end`], [`Self::discard`]) or a bare
    /// line break confirmed it. A host that talks to the platform IME itself (macOS
    /// `discardMarkedText`) tells it to drop its marked text then; egui's
    /// `should_interrupt_composition` is already set while the IME is on.
    pub fn interrupted(&self) -> bool {
        self.interrupted
    }

    /// Ends the composition from the app, keeping the marked text as typed, for an app ending
    /// its text editing itself (a tool change, a command). Returns that text for the app to
    /// insert before the editor closes. The IME is told to drop its copy, and its late commit
    /// of the same text is dropped.
    #[must_use = "insert the marked text, or call `discard` when the editor already keeps it"]
    pub fn end(&mut self, ctx: &Context) -> Option<String> {
        self.start_pass(ctx);
        let time = ctx.input(|i| i.time);
        let kept = self.end_at(time);
        if kept.is_some() && self.on {
            self.write_output(ctx);
        }
        kept
    }

    /// Ends the composition from the app without handing back the marked text: the edit was
    /// cancelled, or the editor shows marked text inline and kept it when it stopped composing
    /// on its own. Otherwise as [`Self::end`].
    pub fn discard(&mut self, ctx: &Context) {
        let _ = self.end(ctx);
    }

    /// The state machine behind [`Self::update`], for callers that read events elsewhere and
    /// for tests. `time` is the pass's input time in seconds.
    pub fn process(&mut self, events: &[Event], time: f64) -> Processed {
        self.interrupted = false;
        self.process_pass(events, time)
    }

    /// [`Self::process`] for [`Self::update`], which starts [`Self::interrupted`] per pass, so
    /// an [`Self::end`] before it in the same pass still counts.
    fn process_pass(&mut self, events: &[Event], time: f64) -> Processed {
        let mut out = Processed::default();
        self.last_commit = None;
        if self.kept.as_ref().is_some_and(|(_, at)| time - at > KEPT_SECS) {
            self.kept = None;
        }
        // The previous event was a pressed Enter: a line break right after it is that key's text.
        let mut after_enter = false;
        for event in events {
            let follows_enter = std::mem::take(&mut after_enter);
            let keep = match event {
                Event::Ime(ime) => {
                    self.ime(ime, time, follows_enter, &mut out.edits);
                    false
                }
                Event::Key { key, pressed, repeat, modifiers, .. } => {
                    self.modifiers = *modifiers;
                    if !*pressed {
                        after_enter = follows_enter;
                        true
                    } else {
                        after_enter = *key == Key::Enter;
                        if self.is_composing() || self.take_guard(*key, time) {
                            false
                        } else {
                            self.kept = None;
                            // Text after this key is the key's own.
                            self.last_commit = None;
                            out.edits.push(Edit::Key { key: *key, modifiers: *modifiers, repeat: *repeat });
                            true
                        }
                    }
                }
                Event::Text(text) => {
                    let echo = self.is_echo(text);
                    if self.is_composing() || echo || is_shortcut_chord(self.modifiers) {
                        false
                    } else if is_line_break(text) {
                        self.line_break(time, follows_enter, &mut out.edits);
                        true
                    } else {
                        // Keys after typed text are the user's.
                        self.guard = None;
                        self.kept = None;
                        out.edits.push(Edit::Insert(text.clone()));
                        true
                    }
                }
                Event::Paste(_) | Event::Copy | Event::Cut if self.is_composing() => false,
                Event::Paste(text) => {
                    out.edits.push(Edit::Paste(text.clone()));
                    true
                }
                Event::Copy => {
                    out.edits.push(Edit::Copy);
                    true
                }
                Event::Cut => {
                    out.edits.push(Edit::Cut);
                    true
                }
                Event::ModifiersChanged(modifiers) => {
                    self.modifiers = *modifiers;
                    after_enter = follows_enter;
                    true
                }
                Event::PointerButton { pressed: true, .. } => {
                    // A click while composing keeps the marked text where it is, then the app
                    // handles the click (moving the caret or ending the edit).
                    if let Some(text) = self.end_at(time) {
                        out.edits.push(Edit::Preedit { text: String::new(), active: None });
                        out.edits.push(Edit::Insert(text));
                    }
                    true
                }
                _ => true,
            };
            if keep {
                out.events.push(event.clone());
            }
        }
        out
    }

    fn ime(&mut self, ime: &ImeEvent, time: f64, after_enter: bool, edits: &mut Vec<Edit>) {
        match ime {
            ImeEvent::Preedit { text, .. } if text.is_empty() => {
                // Wayland input methods send empty preedits on every state change and after
                // each commit; only one that ends a composition means anything.
                self.take_preedit(time, edits);
            }
            ImeEvent::Preedit { text, active_range_chars } => {
                self.kept = None;
                self.last_commit = None;
                self.preedit.clone_from(text);
                self.active = active_range_chars.clone().filter(|r| r.end <= text.chars().count());
                edits.push(Edit::Preedit { text: text.clone(), active: self.active.clone() });
            }
            // A bare line break confirms the composition (Enter with some IMEs): the marked text
            // stays as typed, not a new line. The platform's input context can still hold the
            // marked text (macOS), so it is kept as for a click.
            ImeEvent::Commit(text) if is_line_break(text) => {
                if let Some(kept) = self.take_preedit(time, edits) {
                    self.guard = Some((Guard::Ended { committed: true }, time));
                    edits.push(Edit::Insert(self.keep(kept, time)));
                } else if self.awaits_commit(time) {
                    self.guard = Some((Guard::Ended { committed: true }, time));
                } else {
                    self.last_commit = Some(text.clone());
                    self.line_break(time, after_enter, edits);
                }
            }
            ImeEvent::Commit(text) => {
                let was_composing = self.take_preedit(time, edits).is_some();
                if self.kept.take().is_some_and(|(kept, _)| kept == *text) {
                    return;
                }
                if was_composing || self.awaits_commit(time) {
                    self.guard = Some((Guard::Ended { committed: true }, time));
                } else if !text.is_empty() {
                    // A commit with no composition (IBus and fcitx typing plain letters, the
                    // emoji picker, dictation, Apple Korean's Space with no syllable) is typed
                    // text. Only Space can follow it as the key that typed it.
                    self.guard = (text == " ").then_some((Guard::Key(Key::Space), time));
                }
                if text.is_empty() {
                    return;
                }
                self.last_commit = Some(text.clone());
                edits.push(Edit::Insert(text.clone()));
            }
            ImeEvent::DeleteSurrounding { before_chars, after_chars } => {
                if !self.is_composing() {
                    edits.push(Edit::DeleteSurrounding { before_chars: *before_chars, after_chars: *after_chars });
                }
            }
            ImeEvent::DeleteSurroundingBytes { before_bytes, after_bytes } => {
                edits.push(Edit::DeleteSurroundingBytes { before_bytes: *before_bytes, after_bytes: *after_bytes });
            }
            #[expect(deprecated)]
            ImeEvent::Enabled | ImeEvent::Disabled => {}
        }
    }

    /// Whether `text` repeats the commit just made, as the text of the key that typed it.
    ///
    /// macOS on winit 0.30 without the fix for rust-windowing/winit#4478 does this for Apple
    /// Korean's Space with no syllable: `Commit(" ")`, then that Space as `KeyboardInput`, which
    /// egui-winit turns into `Key(Space)`, `Text(" ")`, all in one pass. The key is guarded as
    /// the IME's, so only the first text after the commit, with no key handed to the editor in
    /// between, can be its echo. Text in a later pass is typed.
    fn is_echo(&mut self, text: &str) -> bool {
        self.last_commit.take().is_some_and(|c| c == text)
    }

    /// Whether `key` is the one the platform forwards after a composition ended or a bare
    /// commit. One key per guard: a second Enter is the user's.
    fn take_guard(&mut self, key: Key, time: f64) -> bool {
        let guarded = self.guard.is_some_and(|(guard, at)| guard.covers(key) && time - at <= GUARD_SECS);
        if guarded {
            self.guard = None;
        }
        guarded
    }

    /// The composition just ended with an empty preedit and its commit hasn't arrived yet
    /// (Wayland and Windows end it before committing).
    fn awaits_commit(&self, time: f64) -> bool {
        matches!(self.guard, Some((Guard::Ended { committed: false }, at)) if time - at <= GUARD_SECS)
    }

    /// A line break typed or committed with nothing composing is the Enter key, unless the Enter
    /// that typed it came just before.
    fn line_break(&mut self, time: f64, after_enter: bool, edits: &mut Vec<Edit>) {
        if after_enter {
            return;
        }
        self.kept = None;
        self.guard = Some((Guard::Key(Key::Enter), time));
        edits.push(Edit::LineBreak { modifiers: self.modifiers });
    }

    fn take_preedit(&mut self, time: f64, edits: &mut Vec<Edit>) -> Option<String> {
        if !self.is_composing() {
            return None;
        }
        self.active = None;
        self.guard = Some((Guard::Ended { committed: false }, time));
        edits.push(Edit::Preedit { text: String::new(), active: None });
        Some(std::mem::take(&mut self.preedit))
    }

    /// Keeps marked text as typed, remembering it so the IME's late commit of it is dropped.
    fn end_at(&mut self, time: f64) -> Option<String> {
        if !self.is_composing() {
            return None;
        }
        let text = std::mem::take(&mut self.preedit);
        self.active = None;
        Some(self.keep(text, time))
    }

    /// Starts [`Self::interrupted`] afresh in a new pass. Returns the pass number.
    fn start_pass(&mut self, ctx: &Context) -> u64 {
        let pass = ctx.cumulative_pass_nr();
        if self.pass.replace(pass) != Some(pass) {
            self.interrupted = false;
        }
        pass
    }

    /// Marked text the composer ended itself: the IME is interrupted, and a late commit of the
    /// same text is dropped.
    fn keep(&mut self, text: String, time: f64) -> String {
        self.interrupt = true;
        self.interrupted = true;
        self.kept = Some((text.clone(), time));
        text
    }

    fn write_output(&mut self, ctx: &Context) {
        let interrupt = std::mem::take(&mut self.interrupt);
        let viewport = ctx.viewport_id();
        self.output_pass = Some((viewport, ctx.cumulative_pass_nr()));
        let rect = self.caret.unwrap_or(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)));
        let observation = ImeObservation::new(ctx.cumulative_frame_nr_for(viewport), ctx.cumulative_pass_nr_for(viewport), ImeTarget::new(self.owner, 0))
            .purpose(self.purpose);
        // `None` only once egui's process-wide generation counter has run out.
        let Some(context) = self.context.observe(viewport, Some(observation)) else {
            return;
        };
        let mut ime = IMEOutput::new(context.owner, context.context_generation, context.purpose, rect, rect);
        ime.should_interrupt_composition = interrupt;
        ime.change_cause = context.change_cause;
        ime.surrounding_text = context.surrounding_text;
        // egui-winit turns the IME on only in passes that set `ime`.
        ctx.output_mut(|o| {
            // A later write in the same pass (the caret after edits) keeps the interrupt.
            ime.should_interrupt_composition |= o.ime.as_ref().is_some_and(|ime| ime.should_interrupt_composition);
            o.ime = Some(ime);
        });
    }
}

/// Keys the platform may forward after the text they produced, for [`GUARD_SECS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Guard {
    /// A composition ended: the key that confirmed, cancelled or emptied it is the IME's.
    /// `committed` once its commit, if any, has arrived.
    Ended { committed: bool },
    /// Text that arrived without a composition: only the key that typed it (Apple Korean's
    /// Space after a commit of " ", the Enter behind a line break) is a duplicate.
    Key(Key),
}

impl Guard {
    fn covers(self, key: Key) -> bool {
        match self {
            Self::Ended { .. } => is_ime_key(key),
            Self::Key(guarded) => key == guarded,
        }
    }
}

/// Keys an IME uses to confirm, cancel or edit a composition.
fn is_ime_key(key: Key) -> bool {
    matches!(key, Key::Enter | Key::Escape | Key::Backspace | Key::Delete | Key::Space)
}

fn is_line_break(text: &str) -> bool {
    text == "\n" || text == "\r" || text == "\r\n"
}

/// Cmd/Ctrl chords are shortcuts: a host that still sends their letter as text doesn't type it.
/// Alt is left alone: Option on macOS and AltGr (Ctrl+Alt) on Windows type characters.
fn is_shortcut_chord(m: Modifiers) -> bool {
    (m.command || m.ctrl || m.mac_cmd) && !m.alt
}

/// A caret IMEs can place a window at: at least a point wide and tall (some ignore an empty
/// rectangle), and inside the window, so a caret scrolled off the canvas doesn't put the
/// candidates off screen.
fn caret_area(caret: Rect, screen: Rect) -> Rect {
    if !caret.is_finite() || !screen.is_positive() {
        return caret;
    }
    let size = caret.size().max(egui::vec2(1.0, 1.0)).min(screen.size());
    // `screen.max - size` can round below `screen.min` (a screen from 0.1 to 0.5), and `clamp`
    // panics on bounds out of order.
    let min = caret.min.clamp(screen.min, (screen.max - size).max(screen.min));
    Rect::from_min_size(min, size)
}
