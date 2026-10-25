//! Ordered admission for the Program monitor's interrupted input method.
//!
//! A reset request is not an acknowledgement. Geometry/focus/output loss retires the local
//! token, and a later actual custom descriptor requests a fresh fence even for the same caret.

use std::sync::Arc;

use egui::{Context, IMEPurpose, Id, ImeResetId, ImeTarget, ViewportId, ViewportIdMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub document: u64,
    pub target: ImeTarget,
    pub caret: usize,
    pub anchor: usize,
    pub text: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputContext {
    pub owner: Id,
    pub generation: u64,
    pub purpose: IMEPurpose,
}

impl OutputContext {
    /// Snapshot the owner/generation/purpose triple that identifies one published custom IME
    /// output.
    pub(crate) fn of(ime: &egui::output::IMEOutput) -> Self {
        Self { owner: ime.owner, generation: ime.context_generation, purpose: ime.purpose }
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    NeedsRequest,
    Awaiting(ImeResetId),
    Suspended,
    AllocationFailed,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    // The retired composition is separate from the current destination (A -> B releases B).
    origin: ImeTarget,
    destination: Option<Destination>,
    output: Option<OutputContext>,
    phase: Phase,
}

#[derive(Default)]
pub(crate) struct State {
    pending: ViewportIdMap<Pending>,
    incoming: ViewportIdMap<Destination>,
    completed_origin: ViewportIdMap<ImeTarget>,
    pub viewport: ViewportId,
    custom_output: Option<OutputContext>,
}

impl State {
    /// Start a pass for `ctx`'s viewport and clear the per-pass published-output record, so
    /// `finish` cannot mistake a previous pass's descriptor for this pass's.
    pub(crate) fn begin_pass(&mut self, ctx: &Context) {
        self.viewport = ctx.viewport_id();
        self.custom_output = None;
    }

    /// Whether a reset fence still guards `viewport`. While blocked, IME input for the custom
    /// editor must not be admitted.
    pub(crate) fn blocked(&self, viewport: ViewportId) -> bool {
        self.pending.contains_key(&viewport)
    }

    /// The composition target being retired for `viewport`: the pending origin, or the last
    /// completed origin until `terminal` clears it.
    pub(crate) fn origin(&self, viewport: ViewportId) -> Option<ImeTarget> {
        self.pending.get(&viewport).map(|p| p.origin).or_else(|| self.completed_origin.get(&viewport).copied())
    }

    /// Whether `destination` differs from the last one remembered for `viewport`; false when
    /// nothing has been remembered yet.
    pub(crate) fn incoming_changed(&self, viewport: ViewportId, destination: Destination) -> bool {
        self.incoming.get(&viewport).is_some_and(|old| *old != destination)
    }

    /// Record the latest destination for `viewport`, so the next event can distinguish a real
    /// target change from repeated observation of the same target.
    pub(crate) fn remember(&mut self, viewport: ViewportId, destination: Destination) {
        self.incoming.insert(viewport, destination);
    }

    /// Acknowledge a terminal IME event for `viewport`: forget the incoming destination and
    /// completed origin so stale targets cannot be reused.
    pub(crate) fn terminal(&mut self, viewport: ViewportId) {
        self.incoming.remove(&viewport);
        self.completed_origin.remove(&viewport);
    }

    /// Context-less commands retire an obsolete ID synchronously. Repeated unchanged
    /// reconciliation is inert; the next live local destination flushes NeedsRequest.
    pub(crate) fn interrupt(&mut self, origin: ImeTarget, destination: Option<Destination>) {
        let viewport = self.viewport;
        if let Some(pending) = self.pending.get_mut(&viewport) {
            if pending.destination != destination {
                pending.destination = destination;
                // An unavailable route cannot be rearmed by reconciliation or a control.
                // Only the actual eligible final descriptor may leave Suspended.
                if !matches!(pending.phase, Phase::Suspended) {
                    pending.phase = Phase::NeedsRequest;
                }
            }
        } else {
            self.pending.insert(viewport, Pending { origin, destination, output: None, phase: Phase::NeedsRequest });
        }
    }

    /// Move a pending fence to `Suspended` and drop its old output binding. Only an actually
    /// eligible final descriptor in `finish` may rearm it; `sync`/`interrupt` reconciliation cannot.
    pub(crate) fn suspend(&mut self, viewport: ViewportId) {
        if let Some(pending) = self.pending.get_mut(&viewport) {
            pending.phase = Phase::Suspended;
            pending.output = None;
        }
    }

    /// Whether the custom output published in this pass is the one recorded by `produced`.
    /// False when no custom output was produced in this pass.
    pub(crate) fn owns_output(&self, ctx: &Context) -> bool {
        self.custom_output.is_some() && ctx.output(|out| out.ime.as_ref().map(OutputContext::of)) == self.custom_output
    }

    /// Allocate a reset fence for the current viewport after clearing the previous fence's
    /// output binding. Allocation failure leaves the phase `AllocationFailed`, which still
    /// blocks input; a later real destination change can request again.
    fn request(ctx: &Context, pending: &mut Pending) {
        // A new fence invalidates any output context bound to the previous fence. Clearing it
        // also lets `finish` recognize this request as already made in this pass, even when an
        // earlier pass had bound a different descriptor to the old destination.
        pending.output = None;
        pending.phase = ctx.request_ime_reset(ctx.viewport_id()).map_or(Phase::AllocationFailed, Phase::Awaiting);
    }

    /// Must run with the live local ed/text before EACH raw event, never a staged engine copy.
    pub(crate) fn sync(&mut self, ctx: &Context, destination: Destination) {
        let Some(pending) = self.pending.get_mut(&ctx.viewport_id()) else { return };
        let changed = pending.destination != Some(destination);
        pending.destination = Some(destination);
        // A suspended route cannot admit events until an actual eligible output rearms it.
        if !matches!(pending.phase, Phase::Suspended) && (changed || matches!(pending.phase, Phase::NeedsRequest)) {
            Self::request(ctx, pending);
        }
    }

    /// Validate a completion token and release the viewport's guard. The token must belong to
    /// this viewport and equal the current `Awaiting` id, and `destination` must equal the
    /// pending destination; `ctx.output` is deliberately not consulted because raw events run
    /// before this pass publishes its descriptor. Any mismatch returns false and keeps the
    /// guard. Success records the origin and destination for the next observation.
    pub(crate) fn complete(&mut self, ctx: &Context, id: ImeResetId, destination: Destination) -> bool {
        let viewport = ctx.viewport_id();
        let Some(pending) = self.pending.get(&viewport).copied() else { return false };
        // The opaque token is necessary but not sufficient: the destination carries the owner
        // and replacement epoch, Film publishes only `IMEPurpose::Normal`, and a generation,
        // purpose or focus transition supersedes or suspends this guard before it can be rearmed. Do not read
        // `ctx.output` here: raw events are processed before this pass has published its custom
        // descriptor, so that would compare against an empty or stale output. The reducer test
        // `changed_output_context_supersedes_and_allocation_failure_stays_blocked` binds a token
        // to one live owner/generation/purpose by rejecting it after each transition.
        if id.viewport_id() != viewport || pending.destination != Some(destination) || !matches!(pending.phase, Phase::Awaiting(current) if current == id) {
            return false;
        }
        self.pending.remove(&viewport);
        self.completed_origin.insert(viewport, pending.origin);
        self.remember(viewport, destination);
        true
    }

    /// Record the custom output descriptor published in this pass, for `finish` and
    /// `owns_output` to compare against the actual output.
    pub(crate) fn produced(&mut self, key: OutputContext) {
        self.custom_output = Some(key);
    }

    /// Actual custom eligibility and actual final app output are independent facts. Never
    /// overwrite a stock field or infer native completion from a descriptor/publication.
    pub(crate) fn finish(&mut self, ctx: &Context, destination: Option<Destination>, eligible: bool) {
        let viewport = ctx.viewport_id();
        let actual = ctx.output(|out| out.ime.as_ref().map(OutputContext::of));
        let custom = self.custom_output;
        let Some(pending) = self.pending.get_mut(&viewport) else { return };
        let Some(destination) = destination.filter(|_| eligible && custom.is_some() && actual == custom) else {
            self.suspend(viewport);
            return;
        };
        let changed = pending.destination != Some(destination) || pending.output != custom;
        let activate = matches!(pending.phase, Phase::Suspended | Phase::NeedsRequest);
        // `sync` may already have requested this pass, while `finish` is the first place the
        // actual custom output context is known. Bind that same request to the descriptor instead
        // of allocating a second, silently superseded ID for the same destination transition.
        let requested_by_sync = matches!(pending.phase, Phase::Awaiting(_)) && pending.output.is_none();
        pending.destination = Some(destination);
        if activate || (changed && !requested_by_sync) {
            Self::request(ctx, pending);
        }
        // A request clears the previous fence's binding. Bind the confirmed descriptor only
        // after requesting, so the next pass compares against this output rather than mistaking
        // an earlier pass's unbound request for a same-pass sync request.
        pending.output = custom;
    }

    /// Forget all reset state for a closed viewport: pending fence, incoming destination and
    /// completed origin.
    pub(crate) fn end_viewport(&mut self, viewport: ViewportId) {
        self.pending.remove(&viewport);
        self.incoming.remove(&viewport);
        self.completed_origin.remove(&viewport);
    }
}

/// Temp-data key marking whether `viewport` is inside a UI pass rather than a hidden logic
/// callback.
fn mode_id(viewport: ViewportId) -> Id {
    Id::unique(("filmcraft-ime-reset-ui-pass", viewport))
}

/// Whether the current callback is a UI pass (between the installed begin/end-pass hooks)
/// for this viewport, rather than `run_logic`. Defaults to false when no hook ran.
pub(crate) fn in_ui_pass(ctx: &Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(mode_id(ctx.viewport_id()))).unwrap_or(false)
}

/// Install before the first run_ui. The callbacks receive Ui and capture no Context/app.
pub(crate) fn install(ctx: &Context) {
    let installed = Id::unique("filmcraft-ime-reset-mode-installed");
    if ctx.data(|data| data.get_temp::<bool>(installed)).unwrap_or(false) {
        return;
    }
    ctx.on_begin_pass(
        "filmcraft IME reset UI entry",
        Arc::new(|ui| {
            let viewport = ui.ctx().viewport_id();
            ui.data_mut(|data| data.insert_temp(mode_id(viewport), true));
        }),
    );
    ctx.on_end_pass(
        "filmcraft IME reset UI exit",
        Arc::new(|ui| {
            let viewport = ui.ctx().viewport_id();
            ui.data_mut(|data| data.insert_temp(mode_id(viewport), false));
        }),
    );
    ctx.data_mut(|data| data.insert_temp(installed, true));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed destination for one editor owner with caret 3 and committed-text stamp 17.
    fn destination() -> Destination {
        Destination { document: 1, target: ImeTarget::new(Id::unique("film-model-editor"), 1), caret: 3, anchor: 3, text: 17 }
    }

    /// Build the IME output a custom producer publishes for `destination` with the given
    /// generation and purpose.
    fn output(destination: Destination, generation: u64, purpose: IMEPurpose) -> egui::output::IMEOutput {
        egui::output::IMEOutput::new(destination.target.owner, generation, purpose, egui::Rect::ZERO, egui::Rect::ZERO)
    }

    // These reducer tests use real Context requests/output, not native acknowledgements.
    /// Run one real UI pass that publishes a custom `output` and calls `finish` as eligible;
    /// return the reset request the Context attached, if any.
    fn custom_pass(ctx: &Context, state: &mut State, destination: Destination, generation: u64, purpose: IMEPurpose) -> Option<ImeResetId> {
        let mut result = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.begin_pass(ui.ctx());
            let ime = output(destination, generation, purpose);
            state.produced(OutputContext::of(&ime));
            ui.ctx().output_mut(|out| out.ime = Some(ime));
            state.finish(ui.ctx(), Some(destination), true);
        });
        let id = result.platform_output.ime.as_ref().and_then(|ime| ime.reset_request);
        result.textures_delta.clear();
        id
    }

    /// A generation or purpose change supersedes the old fence, old tokens cannot complete, an
    /// allocation failure stays blocked, and a later real destination change retries.
    #[test]
    fn changed_output_context_supersedes_and_allocation_failure_stays_blocked() {
        let ctx = Context::default();
        install(&ctx);
        let mut state = State::default();
        let destination = destination();
        state.interrupt(destination.target, Some(destination));
        let first = custom_pass(&ctx, &mut state, destination, 1, IMEPurpose::Normal).unwrap();
        assert!(custom_pass(&ctx, &mut state, destination, 1, IMEPurpose::Normal).is_none());
        let second = custom_pass(&ctx, &mut state, destination, 2, IMEPurpose::Normal).unwrap();
        let third = custom_pass(&ctx, &mut state, destination, 2, IMEPurpose::Password).unwrap();
        assert_ne!(first, second);
        assert_ne!(second, third);
        // The old opaque IDs are rejected even though document/target/caret/text are unchanged.
        assert!(!state.complete(&ctx, first, destination));
        assert!(!state.complete(&ctx, second, destination));
        let other_owner = Destination { target: ImeTarget::new(Id::unique("film-other-owner"), 1), ..destination };
        assert!(!state.complete(&ctx, third, other_owner), "the live destination owner is part of the admission boundary");
        assert!(state.blocked(ViewportId::ROOT));
        // Model token exhaustion explicitly; do not attempt to forge an opaque ID.
        state.pending.get_mut(&ViewportId::ROOT).unwrap().phase = Phase::AllocationFailed;
        for _ in 0..3 {
            assert!(custom_pass(&ctx, &mut state, destination, 2, IMEPurpose::Password).is_none());
            assert!(state.blocked(ViewportId::ROOT));
        }
        let changed = Destination { caret: 1, anchor: 1, ..destination };
        state.sync(&ctx, changed);
        let retry = custom_pass(&ctx, &mut state, changed, 2, IMEPurpose::Password).unwrap();
        assert!(state.complete(&ctx, retry, changed));
        assert!(!state.complete(&ctx, retry, changed));
    }

    /// `sync` followed by `finish` in the same pass allocates exactly one request and keeps it
    /// as the awaiting boundary.
    #[test]
    fn same_pass_sync_and_finish_keep_one_request() {
        let ctx = Context::default();
        install(&ctx);
        let mut state = State::default();
        let destination = destination();
        state.interrupt(destination.target, Some(destination));
        let mut requested = None;
        let mut result = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.begin_pass(ui.ctx());
            let ime = output(destination, 1, IMEPurpose::Normal);
            state.produced(OutputContext::of(&ime));
            ui.ctx().output_mut(|out| out.ime = Some(ime));
            state.sync(ui.ctx(), destination);
            if let Phase::Awaiting(id) = state.pending[&ViewportId::ROOT].phase {
                requested = Some(id);
            }
            state.finish(ui.ctx(), Some(destination), true);
            assert!(matches!(
                state.pending[&ViewportId::ROOT].phase,
                Phase::Awaiting(id) if Some(id) == requested
            ));
        });
        assert_eq!(result.platform_output.ime.as_ref().and_then(|ime| ime.reset_request), requested);
        result.textures_delta.clear();
    }

    /// `finish` reuses the replacement request allocated by `sync` instead of allocating a
    /// second one, and the stale token cannot release the guard.
    #[test]
    fn stale_output_binding_does_not_force_a_second_request_on_sync_then_finish() {
        let ctx = Context::default();
        install(&ctx);
        let mut state = State::default();
        let destination = destination();
        state.interrupt(destination.target, Some(destination));
        let stale = custom_pass(&ctx, &mut state, destination, 1, IMEPurpose::Normal).unwrap();
        assert!(matches!(
            state.pending[&ViewportId::ROOT].phase,
            Phase::Awaiting(id) if id == stale
        ));
        assert!(state.pending[&ViewportId::ROOT].output.is_some());

        // A destination transition retires the fence at the admission boundary but leaves the
        // previous output binding attached until the next actual request replaces it.
        let changed = Destination { caret: 1, anchor: 1, ..destination };
        state.interrupt(destination.target, Some(changed));
        assert!(state.pending[&ViewportId::ROOT].output.is_some(), "an old output context remains bound");

        let mut requested_by_sync = None;
        let mut result = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.begin_pass(ui.ctx());
            let ime = output(changed, 2, IMEPurpose::Normal);
            state.produced(OutputContext::of(&ime));
            ui.ctx().output_mut(|out| out.ime = Some(ime));
            state.sync(ui.ctx(), changed);
            if let Phase::Awaiting(id) = state.pending[&ViewportId::ROOT].phase {
                requested_by_sync = Some(id);
            }
            state.finish(ui.ctx(), Some(changed), true);
            assert!(
                matches!(
                    state.pending[&ViewportId::ROOT].phase,
                    Phase::Awaiting(id) if Some(id) == requested_by_sync
                ),
                "finish must reuse the request sync allocated for this destination transition"
            );
        });
        let replacement = requested_by_sync.expect("sync allocated the replacement request");
        assert_ne!(replacement, stale);
        assert_eq!(result.platform_output.ime.as_ref().and_then(|ime| ime.reset_request), Some(replacement));
        result.textures_delta.clear();

        // The retired R1 is no longer the awaiting boundary and cannot release the guard.
        assert!(!state.complete(&ctx, stale, changed));
        assert!(state.blocked(ViewportId::ROOT));
        assert!(state.complete(&ctx, replacement, changed));
        assert!(!state.blocked(ViewportId::ROOT));
    }

    /// If custom eligibility is lost after `sync`, stock IME may echo the queued token but it
    /// cannot release the suspended guard; only a later eligible descriptor can.
    #[test]
    fn queued_before_absence_token_may_reach_stock_but_cannot_release_suspended_guard() {
        let ctx = Context::default();
        install(&ctx);
        let mut state = State::default();
        let destination = destination();
        state.interrupt(destination.target, Some(destination));
        let mut queued = None;
        let mut absent = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.begin_pass(ui.ctx());
            state.sync(ui.ctx(), destination);
            if let Phase::Awaiting(id) = state.pending[&ViewportId::ROOT].phase {
                queued = Some(id); // actual Context request returned by the production reducer
            }
            // A later same-pass layout/control loss prevents any descriptor publication.
            state.finish(ui.ctx(), Some(destination), false);
        });
        assert!(absent.platform_output.ime.is_none());
        absent.textures_delta.clear();
        let queued = queued.unwrap();
        let stock_id = Id::unique("film-model-stock-field");
        let mut text = String::from("stock");
        let mut stock = ctx.run_ui(egui::RawInput::default(), |ui| {
            state.begin_pass(ui.ctx());
            ui.memory_mut(|memory| memory.request_focus(stock_id));
            ui.add(egui::TextEdit::singleline(&mut text).id(stock_id));
            // Destination changes on an ineligible route cannot allocate a replacement.
            state.interrupt(destination.target, Some(Destination { caret: 0, anchor: 0, ..destination }));
            state.sync(ui.ctx(), destination);
            assert!(!state.complete(ui.ctx(), queued, destination));
            state.finish(ui.ctx(), Some(destination), false);
        });
        assert_eq!(
            stock.platform_output.ime.as_ref().and_then(|ime| ime.reset_request),
            Some(queued),
            "core may attach ONLY the already queued R1; the public app API cannot cancel it"
        );
        assert!(matches!(state.pending[&ViewportId::ROOT].phase, Phase::Suspended));
        stock.textures_delta.clear();
        ctx.memory_mut(|memory| memory.surrender_focus(stock_id));
        let replacement = custom_pass(&ctx, &mut state, destination, 1, IMEPurpose::Normal).unwrap();
        assert_ne!(replacement, queued);
        assert!(!state.complete(&ctx, queued, destination));
        assert!(state.complete(&ctx, replacement, destination));
    }

    /// UI-pass mode is scoped per viewport, `install` is idempotent, and a child viewport's
    /// completion cannot release ROOT's guard.
    #[test]
    fn pass_mode_is_per_viewport_and_foreign_completion_cannot_release_a_real_guard() {
        let ctx = Context::default();
        install(&ctx);
        install(&ctx); // creator reuse must not register another pair of callbacks
        let mut state = State::default();
        let destination = destination();
        state.interrupt(destination.target, Some(destination));
        let root = custom_pass(&ctx, &mut state, destination, 1, IMEPurpose::Normal).unwrap();

        let foreign_viewport = ViewportId::from_hash_of("film-other-window");
        // Register an actual child viewport; a request for an unlisted viewport would be dropped
        // by `Memory::begin_pass` before it could exercise the viewport check at all.
        let mut raw = egui::RawInput { viewport_id: foreign_viewport, ..Default::default() };
        raw.viewports.insert(foreign_viewport, egui::ViewportInfo { parent: Some(ViewportId::ROOT), ..Default::default() });
        let mut foreign = None;
        let mut foreign_output = ctx.run_ui(raw, |ui| {
            assert_eq!(ui.ctx().viewport_id(), foreign_viewport);
            assert!(in_ui_pass(ui.ctx()));
            foreign = ui.ctx().request_ime_reset(foreign_viewport);
        });
        foreign_output.textures_delta.clear();
        let foreign = foreign.expect("the registered child viewport allocated a real request");

        let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(in_ui_pass(ui.ctx()));
            assert!(!ui.data(|data| data.get_temp::<bool>(mode_id(foreign_viewport))).unwrap_or(false));
            state.begin_pass(ui.ctx());
            assert!(!state.complete(ui.ctx(), foreign, destination), "a child viewport's completion cannot release ROOT's guard");
            assert!(state.blocked(ViewportId::ROOT));
            assert!(state.complete(ui.ctx(), root, destination));
            assert!(!state.complete(ui.ctx(), foreign, destination));
            assert!(!state.blocked(ViewportId::ROOT));
        });
        full.textures_delta.clear();
        assert!(!in_ui_pass(&ctx));
        let _logic = ctx.run_logic(&egui::RawInput::default(), |ctx| assert!(!in_ui_pass(ctx)));
    }
}
