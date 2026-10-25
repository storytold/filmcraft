//! Bounded, one-shot window screenshots for `ui.screenshot`.
//!
//! A request record owns the output path, crop, reply, deadline, document/focus identity and the
//! surface geometry it was admitted against. `craft_ui::capture::CaptureSlot` owns the strong
//! `egui::Context`; its callback holds only a weak reference and never writes a file. Files are
//! encoded and written only from the logic drain, after the deadline and every identity/geometry
//! check has passed.
//!
//! Web qualification note: the pinned eframe web runner does not implement
//! `ViewportCommand::RequestPaintWhileHidden` (the native runner does). A hidden web capture that
//! is still settling therefore reaches its timeout. That fork gap is recorded as a prerequisite;
//! this module does not claim a hidden web capture works.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use craft_ui::capture::{CaptureError, CaptureSlot};
use egui::{ColorImage, ViewportCommand};
use serde_json::{Value, json};
use web_time::{Duration, Instant};

use crate::FilmcraftApp;

/// At most this many queued, in-flight or completed-but-not-yet-drained requests.
pub(crate) const MAX_REQUESTS: usize = 4;
/// Per-image bound from the suite contract.
pub(crate) const MAX_IMAGE_BYTES: usize = 128 * 1024 * 1024;
/// Aggregate bound, counting completed images that have not been drained yet.
pub(crate) const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// The raw RGBA image is not the only copy alive while a capture is drained: the callback's
/// `ColorImage`, the cropped RGBA copy and the encoded PNG exist together, and on the web the PNG
/// is base64-encoded into the reply while all of those are still retained. Reserve this multiple
/// of the raw pixel bytes so admission actually bounds peak memory, not just the first buffer.
pub(crate) const CAPTURE_BUFFER_FACTOR: usize = 5;

/// Bytes reserved for one `width`×`height` RGBA capture, including the transient encode copies.
pub(crate) fn admitted_bytes(expected: [usize; 2]) -> Option<usize> {
    expected[0].checked_mul(expected[1])?.checked_mul(std::mem::size_of::<egui::Color32>())?.checked_mul(CAPTURE_BUFFER_FACTOR)
}

const READY_DELAY: Duration = Duration::from_millis(250);
/// Accepted first UI passes (never discarded passes or hidden logic ticks).
const SETTLE_PASSES: u32 = 3;
const DEADLINE: Duration = Duration::from_secs(10);
/// Give up waiting for the exact monitor frame/timeline rest and capture what is shown (5 s).
const SETTLE_MAX: Duration = Duration::from_secs(5);
/// Accepted passes to wait for a named crop target that is not drawn yet (e.g. a panel shown by
/// the preceding control request). After this the request fails instead of capturing the window.
const CROP_SETTLE_PASSES: u32 = 3;
const REPAINT_INTERVAL: Duration = Duration::from_millis(16);

/// The host surface as reported by one `RawInput`. Raw input is fresh on hidden logic-only ticks;
/// `Context::input`/`viewport_rect` describe the last UI pass and are stale there.
///
/// Equality covers only what changes the captured pixels (viewport size, pixels per point and the
/// GPU texture limit). Minimized, occluded and native window focus are presentation flags: a
/// hidden or unfocused window still captures the forced paint that was already issued, so they
/// must neither invalidate an in-flight screenshot nor bump `capture_epoch`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceGeometry {
    /// Logical viewport size in points. A minimized window keeps its last known size so a forced
    /// hidden paint can still be requested; `None` only before any size was ever reported.
    viewport: Option<egui::Vec2>,
    /// Native pixels per point times egui's interface zoom.
    pixels_per_point: f32,
    max_texture_side: usize,
    minimized: bool,
    occluded: bool,
}

impl PartialEq for SurfaceGeometry {
    /// Compares only the fields that change captured pixels: viewport size, pixels per point and
    /// the GPU texture limit. The presentation-only minimized/occluded flags are deliberately
    /// ignored, so minimizing, occluding or losing window focus does not invalidate an issued
    /// capture.
    fn eq(&self, other: &Self) -> bool {
        self.viewport == other.viewport && self.pixels_per_point == other.pixels_per_point && self.max_texture_side == other.max_texture_side
    }
}

impl SurfaceGeometry {
    /// Snapshot the surface from a raw-input callback, reading the fresh `viewports`/`screen_rect`
    /// geometry rather than the stale `InputState`. Missing or hostile fields fall back to
    /// `previous` where possible; `None` means no usable zoom and pixels-per-point pair could be
    /// established.
    fn from_raw(raw: &egui::RawInput, ctx: &egui::Context, previous: Option<Self>) -> Option<Self> {
        let info = raw.viewports.get(&raw.viewport_id).cloned().unwrap_or_default();
        let rect = info.inner_rect.or(raw.screen_rect);
        let size = rect.map(|rect| rect.size()).filter(|size| size.x.is_finite() && size.y.is_finite() && size.x > 0.0 && size.y > 0.0);
        let viewport = size.or(previous.and_then(|previous| previous.viewport));
        let zoom = ctx.zoom_factor();
        if !zoom.is_finite() || zoom <= 0.0 {
            return None;
        }
        let pixels_per_point = info
            .native_pixels_per_point
            .map(|native| native * zoom)
            .filter(|pixels| pixels.is_finite() && *pixels > 0.0)
            .or_else(|| previous.map(|previous| previous.pixels_per_point))
            .unwrap_or_else(|| ctx.pixels_per_point());
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return None;
        }
        let max_texture_side = raw.max_texture_side.filter(|side| *side > 0).unwrap_or_else(|| ctx.input(|input| input.max_texture_side));
        let minimized = info.minimized.unwrap_or_else(|| previous.is_some_and(|previous| previous.minimized));
        let occluded = info.occluded.unwrap_or_else(|| previous.is_some_and(|previous| previous.occluded));
        Some(Self { viewport, pixels_per_point, max_texture_side, minimized, occluded })
    }

    /// Pixel size the backend will present: viewport points scaled by pixels per point and
    /// rounded. If either axis exceeds the texture limit, both axes shrink by the same factor to
    /// a positive limit, matching wgpu's `surface_fit`; per-axis clamping would predict the wrong
    /// image. A zero/unknown texture limit disables shrinking.
    /// `None` when the viewport is missing, non-finite, empty or the size overflows `usize`.
    fn expected_size(self) -> Option<[usize; 2]> {
        let viewport = self.viewport?;
        if !viewport.x.is_finite() || !viewport.y.is_finite() || viewport.x <= 0.0 || viewport.y <= 0.0 {
            return None;
        }
        let axis = |points: f32| -> Option<usize> {
            let pixels = (f64::from(points) * f64::from(self.pixels_per_point)).round();
            if !pixels.is_finite() || pixels <= 0.0 || pixels > usize::MAX as f64 {
                return None;
            }
            Some(pixels as usize)
        };
        let [width, height] = [axis(viewport.x)?, axis(viewport.y)?];
        // Match the wgpu backend's `surface_fit`: when either axis exceeds the device limit, both
        // axes shrink by the same factor and the longer side becomes exactly the limit. Clamping
        // each axis independently predicted a wider/taller image than the surface actually has.
        let max = if self.max_texture_side == 0 { usize::MAX } else { self.max_texture_side };
        if width <= max && height <= max {
            return Some([width, height]);
        }
        let longest = width.max(height);
        let scale = max as f64 / longest as f64;
        let fit = |side: usize| -> usize { if side == longest { max } else { ((side as f64 * scale).floor() as usize).clamp(1, max) } };
        Some([fit(width), fit(height)])
    }

    /// Pixels per point of the delivered screenshot texture.
    ///
    /// egui-wgpu's `surface_fit` shrinks an oversized window's surface by one factor and draws it
    /// at `pixels_per_point * render_scale`; a crop expressed in points must use that rendered
    /// scale, not the native one. A window within the limit keeps the native factor exactly, so a
    /// one-pixel rounding difference between `viewport * ppp` and the integer window size never
    /// becomes a bogus sub-1 scale.
    fn captured_pixels_per_point(self) -> f32 {
        let native = self.pixels_per_point;
        if !native.is_finite() || native <= 0.0 {
            return 1.0;
        }
        let Some(viewport) = self.viewport else { return native };
        if !viewport.x.is_finite() || !viewport.y.is_finite() || viewport.x <= 0.0 || viewport.y <= 0.0 || self.max_texture_side == 0 {
            return native;
        }
        let axis = |points: f32| (f64::from(points) * f64::from(native)).round();
        let (width, height) = (axis(viewport.x), axis(viewport.y));
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return native;
        }
        let longest = width.max(height);
        let limit = self.max_texture_side as f64;
        if longest <= limit {
            return native;
        }
        // Match egui-wgpu exactly: it casts `surface_fit`'s scale to f32 and then multiplies the
        // f32 pixels-per-point by it.
        let render_scale = (limit / longest) as f32;
        let effective = native * render_scale;
        if effective.is_finite() && effective > 0.0 { effective } else { native }
    }

    /// Raw RGBA bytes of [`Self::expected_size`], checked for multiplication overflow; `None`
    /// when the surface has no capturable size.
    fn expected_bytes(self) -> Option<usize> {
        let [width, height] = self.expected_size()?;
        width.checked_mul(height)?.checked_mul(std::mem::size_of::<egui::Color32>())
    }
}

/// Which sequence and project revision the request belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentId {
    sequence: Option<u64>,
    revision: u64,
}

/// Viewport geometry plus the immutable document and widget-focus identity a capture is bound to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CaptureGeometry {
    surface: SurfaceGeometry,
    document: DocumentId,
    focus: Option<egui::Id>,
}

impl CaptureGeometry {
    /// Pixel size implied by the bound surface geometry; `None` when it is not capturable.
    fn expected_size(self) -> Option<[usize; 2]> {
        self.surface.expected_size()
    }

    /// Raw RGBA byte count implied by the bound surface, with the same overflow rule as
    /// [`SurfaceGeometry::expected_bytes`].
    fn expected_bytes(self) -> Option<usize> {
        self.surface.expected_bytes()
    }
}

/// A screenshot crop named by a widget id. Resolved at issue time from the pass that is actually
/// captured, never from the control callback that admitted the request (that pass may be two
/// layouts old or may not have drawn the panel at all).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CropTarget {
    Element(String),
}

impl CropTarget {
    /// Resolve the named element against the accepted pass's automation registry. Only an element
    /// drawn in the current pass with a finite, non-empty rectangle is accepted; a stale
    /// previous-pass rectangle or an unknown id is an error, never a fallback to the whole window.
    fn resolve(&self, registry: &crate::automation::Registry) -> Result<[f32; 4], String> {
        let Self::Element(id) = self;
        let element = registry
            .find_current(id)
            .ok_or_else(|| format!("screenshot failed: `{id}` was not drawn in the captured pass (the panel may be closed or hidden; see ui.elements)"))?;
        let [x, y, width, height] = element.rect;
        if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0 {
            return Err(format!("screenshot failed: `{id}` has an empty or non-finite rectangle"));
        }
        Ok(element.rect)
    }
}

struct Shot {
    token: u64,
    path: Option<String>,
    crop_target: Option<CropTarget>,
    /// The crop resolved from the accepted pass that was captured (set at issue time).
    crop: Option<[f32; 4]>,
    /// Accepted passes the named crop target was missing, while a just-shown panel lays out.
    crop_misses: u32,
    reply: Sender<Value>,
    deadline: Instant,
    ready_at: Instant,
    frames: u32,
    issued: bool,
    document: DocumentId,
    focus: Option<egui::Id>,
    geometry: CaptureGeometry,
    geometry_epoch: u64,
    expected: [usize; 2],
    reserved: usize,
    slot: Option<CaptureSlot>,
    callback: Option<egui::ScreenshotCallback>,
}

/// All admitted screenshots. `requests` includes queued, issued and completed-but-undrained
/// captures; nothing is removed before it has been replied to or timed out.
#[derive(Default)]
pub(crate) struct Shots {
    token: u64,
    requests: Vec<Shot>,
}

impl Shots {
    /// True when no request is queued, in flight or completed-but-not-yet-drained.
    pub(crate) fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// True while at least one admitted request has not been handed to a `Screenshot` callback.
    fn has_unissued(&self) -> bool {
        self.requests.iter().any(|shot| !shot.issued)
    }

    /// Sum of every request's byte reservation, including completed-but-undrained captures;
    /// saturates instead of overflowing.
    fn reserved(&self) -> usize {
        self.requests.iter().fold(0usize, |sum, shot| sum.saturating_add(shot.reserved))
    }
}

/// Allocate the next request token, wrapping after `u64::MAX` and skipping zero so a token is
/// always usable as a presence marker.
fn next_token(shots: &mut Shots) -> u64 {
    let mut token = shots.token.wrapping_add(1);
    if token == 0 {
        token = 1;
    }
    shots.token = token;
    token
}

/// Build the JSON error reply carrying `token`, so the client can match a rejection to the
/// request it sent.
fn error_reply(token: u64, message: impl std::fmt::Display) -> Value {
    json!({"ok": false, "error": message.to_string(), "request": token})
}

/// Send an error reply for `token`, ignoring a client that has already disconnected.
fn reject(reply: Sender<Value>, token: u64, message: impl std::fmt::Display) {
    let _ = reply.send(error_reply(token, message));
}

/// The active sequence id (none when no sequence is open) and project revision that a capture
/// is bound to.
fn active_document(app: &FilmcraftApp) -> DocumentId {
    DocumentId { sequence: app.session.state.active_sequence.map(|id| id.0), revision: app.session.revision }
}

/// The widget currently holding keyboard focus, part of the capture identity.
fn current_focus(ctx: &egui::Context) -> Option<egui::Id> {
    ctx.memory(|memory| memory.focused())
}

/// Refresh the fresh raw-input surface/identity snapshot. Called from `raw_input_hook`, so it runs
/// on both UI and hidden logic-only callbacks and never reads the stale `InputState` rectangle.
pub(crate) fn note_raw_geometry(app: &mut FilmcraftApp, ctx: &egui::Context, raw: &egui::RawInput) {
    let previous = app.capture_geometry;
    let Some(surface) = SurfaceGeometry::from_raw(raw, ctx, previous.map(|geometry| geometry.surface)) else {
        app.capture_geometry = None;
        app.capture_epoch = app.capture_epoch.wrapping_add(1);
        return;
    };
    let geometry = CaptureGeometry { surface, document: active_document(app), focus: current_focus(ctx) };
    let changed = app.capture_geometry != Some(geometry);
    // Keep the presentation flags fresh even when they did not change the capture geometry.
    app.capture_geometry = Some(geometry);
    if changed {
        app.capture_epoch = app.capture_epoch.wrapping_add(1);
    }
}

/// Pick up document/revision and keyboard-focus changes made since the raw-input snapshot (a
/// control request, or the UI pass that admitted a `ui.screenshot` at its end).
fn refresh_identity(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(geometry) = app.capture_geometry else { return };
    let refreshed = CaptureGeometry { document: active_document(app), focus: current_focus(ctx), ..geometry };
    let changed = refreshed != geometry;
    app.capture_geometry = Some(refreshed);
    if changed {
        app.capture_epoch = app.capture_epoch.wrapping_add(1);
    }
}

/// Admit one `ui.screenshot` request without issuing it. Counts queued, in-flight and
/// completed-but-undrained requests and reserves the raw image plus its encode/encode-reply buffers
/// before capture.
pub(crate) fn admit(app: &mut FilmcraftApp, ctx: &egui::Context, path: Option<String>, crop: Option<CropTarget>, reply: Sender<Value>) {
    refresh_identity(app, ctx);
    let token = next_token(&mut app.shots);
    let Some(geometry) = app.capture_geometry else {
        return reject(reply, token, "screenshot failed: the window geometry is not known yet");
    };
    let Some(expected) = geometry.expected_size() else {
        return reject(reply, token, "screenshot failed: the window has no capturable size yet");
    };
    let Some(raw) = geometry.expected_bytes() else {
        return reject(reply, token, "screenshot failed: the window size overflows the screenshot budget");
    };
    if raw > MAX_IMAGE_BYTES {
        return reject(
            reply,
            token,
            format!(
                "screenshot budget exceeded: {raw} raw bytes for one {}×{} image, over the {MAX_IMAGE_BYTES}-byte per-image limit",
                expected[0], expected[1]
            ),
        );
    }
    let Some(reserved) = admitted_bytes(expected) else {
        return reject(reply, token, "screenshot failed: the window size overflows the screenshot budget");
    };
    if reserved > MAX_TOTAL_BYTES {
        return reject(
            reply,
            token,
            format!(
                "screenshot budget exceeded: one {}×{} capture needs up to {reserved} bytes including the crop/encode reply buffers, over the {MAX_TOTAL_BYTES}-byte limit",
                expected[0], expected[1]
            ),
        );
    }
    if app.shots.requests.len() >= MAX_REQUESTS {
        return reject(reply, token, format!("screenshot budget exceeded: {MAX_REQUESTS} requests are already outstanding"));
    }
    let total = app.shots.reserved().saturating_add(reserved);
    if total > MAX_TOTAL_BYTES {
        return reject(
            reply,
            token,
            format!("screenshot budget exceeded: {total} bytes reserved by outstanding requests, over the {MAX_TOTAL_BYTES}-byte limit"),
        );
    }
    let (slot, callback) = CaptureSlot::new(ctx, ctx.viewport_id(), MAX_IMAGE_BYTES);
    let now = Instant::now();
    app.shots.requests.push(Shot {
        token,
        path,
        crop_target: crop,
        crop: None,
        crop_misses: 0,
        reply,
        deadline: now + DEADLINE,
        ready_at: now + READY_DELAY,
        frames: 0,
        issued: false,
        document: geometry.document,
        focus: geometry.focus,
        geometry,
        geometry_epoch: app.capture_epoch,
        expected,
        reserved,
        slot: Some(slot),
        callback: Some(callback),
    });
    ctx.request_repaint();
}

/// Why the request's bound document/revision or keyboard focus no longer matches. `None`
/// means identity is unchanged; a mismatch must reject rather than save pixels from another
/// document or focus.
fn identity_reason(app: &FilmcraftApp, ctx: &egui::Context, shot: &Shot) -> Option<&'static str> {
    if active_document(app) != shot.document {
        Some("the document or project revision changed while the capture was queued")
    } else if current_focus(ctx) != shot.focus {
        Some("the keyboard focus changed while the capture was queued")
    } else {
        None
    }
}

/// Why the live surface snapshot or capture epoch differs from the one this request was
/// admitted or issued against. `None` means the geometry is still the one it reserved for.
fn geometry_reason(app: &FilmcraftApp, shot: &Shot) -> Option<&'static str> {
    if app.capture_geometry != Some(shot.geometry) || app.capture_epoch != shot.geometry_epoch {
        Some("the window geometry changed while the capture was queued")
    } else {
        None
    }
}

/// Re-admit an unissued request against the current geometry: cancel its old slot, check the
/// per-image and aggregate budgets including every other request, and reset its settled
/// frames, crop misses and ready time. On failure nothing is changed and the caller rejects
/// it; on success the request is unissued again and must collect fresh settling passes.
fn retry_geometry(app: &mut FilmcraftApp, ctx: &egui::Context, index: usize) -> Result<(), String> {
    let now = Instant::now();
    let current = app.capture_geometry.ok_or_else(|| "the window geometry is no longer known".to_string())?;
    let expected = current.expected_size().ok_or_else(|| "the window has no capturable size".to_string())?;
    let raw = current.expected_bytes().ok_or_else(|| "the window size overflows the screenshot budget".to_string())?;
    if raw > MAX_IMAGE_BYTES {
        return Err(format!("the window now needs {raw} raw bytes, over the {MAX_IMAGE_BYTES}-byte per-image limit"));
    }
    let reserved = admitted_bytes(expected).ok_or_else(|| "the window size overflows the screenshot budget".to_string())?;
    let others = app.shots.requests.iter().enumerate().filter(|(i, _)| *i != index).fold(0usize, |sum, (_, shot)| sum.saturating_add(shot.reserved));
    if others.saturating_add(reserved) > MAX_TOTAL_BYTES {
        return Err(format!("the window now needs {reserved} bytes, over the {MAX_TOTAL_BYTES}-byte aggregate limit"));
    }
    let Some(current_geometry) = app.capture_geometry else { return Err("the window geometry changed repeatedly".into()) };
    let (slot, callback) = CaptureSlot::new(ctx, ctx.viewport_id(), MAX_IMAGE_BYTES);
    let shot = app.shots.requests.get_mut(index).ok_or_else(|| "the screenshot request disappeared".to_string())?;
    if let Some(old) = shot.slot.take() {
        old.cancel();
    }
    shot.callback = Some(callback);
    shot.slot = Some(slot);
    shot.geometry = current_geometry;
    shot.geometry_epoch = app.capture_epoch;
    shot.expected = expected;
    shot.reserved = reserved;
    shot.frames = 0;
    shot.crop_misses = 0;
    shot.ready_at = now + READY_DELAY;
    shot.issued = false;
    Ok(())
}

/// Whether an unissued request has waited for enough accepted passes, including the bounded
/// maximum when the monitor is still showing an inexact frame or the timeline is animating.
fn shot_is_settled(now: Instant, monitor_inexact: bool, timeline_still: u32, shot: &Shot) -> bool {
    if now < shot.ready_at || shot.frames < SETTLE_PASSES {
        return false;
    }
    !((monitor_inexact || timeline_still < 2) && now < shot.ready_at + SETTLE_MAX)
}

/// The result of trying to issue one settled request. Every variant keeps the original admission
/// order: identity failures reject, geometry changes retry, a missing crop target waits a bounded
/// number of passes, and everything else issues in the current accepted UI pass.
enum IssueOutcome {
    Identity(String),
    GeometryRetry,
    CropDeferred,
    Failed(String),
    Issued,
}

/// Decide one settled request: reject on identity change, ask for a geometry retry, defer a
/// not-yet-drawn crop target for a bounded number of accepted passes, or issue the screenshot
/// with the crop resolved from this pass. The callback is taken at most once and `issued` is
/// set only when the viewport command was sent.
fn try_issue_shot(app: &mut FilmcraftApp, ctx: &egui::Context, index: usize) -> IssueOutcome {
    let shot = &app.shots.requests[index];
    if let Some(reason) = identity_reason(app, ctx, shot) {
        return IssueOutcome::Identity(reason.to_string());
    }
    if geometry_reason(app, shot).is_some() {
        return IssueOutcome::GeometryRetry;
    }
    let crop_result = match shot.crop_target.as_ref() {
        Some(target) => target.resolve(&app.auto).map(Some),
        None => Ok(None),
    };
    let resolved_crop = match crop_result {
        Ok(crop) => crop,
        Err(message) => {
            let shot = &mut app.shots.requests[index];
            if shot.crop_misses < CROP_SETTLE_PASSES {
                // The panel may be shown by a request processed just before this admission;
                // let the next accepted pass draw it before giving up.
                shot.crop_misses = shot.crop_misses.saturating_add(1);
                return IssueOutcome::CropDeferred;
            }
            return IssueOutcome::Failed(message);
        }
    };
    let shot = &mut app.shots.requests[index];
    let Some(callback) = shot.callback.take() else {
        return IssueOutcome::Failed("screenshot failed: the capture callback was already consumed".into());
    };
    shot.crop = resolved_crop;
    ctx.send_viewport_cmd_to(ctx.viewport_id(), ViewportCommand::Screenshot(callback));
    shot.issued = true;
    IssueOutcome::Issued
}

/// Reject failed requests from the highest index down, so an earlier removal cannot shift a later
/// index and remove the wrong shot.
fn reject_failures(app: &mut FilmcraftApp, mut failures: Vec<(usize, String)>) {
    failures.sort_unstable_by_key(|(i, _)| std::cmp::Reverse(*i));
    for (i, message) in failures {
        if i >= app.shots.requests.len() {
            continue;
        }
        let shot = app.shots.requests.remove(i);
        if let Some(slot) = shot.slot {
            slot.cancel();
        }
        reject(shot.reply, shot.token, message);
    }
}

/// Issue settled requests. Called from every real `App::ui` pass (never from a hidden
/// `run_logic` tick), including the accepted final pass of a multipass layout. `monitor_inexact`
/// describes the pass that just drew the monitor, so consume it here: it must not stay sticky and
/// delay every later screenshot until the 5 s settle maximum.
pub(crate) fn issue(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let monitor_inexact = std::mem::take(&mut app.monitor_inexact);
    if ctx.will_discard() {
        return;
    }
    let now = Instant::now();
    let mut failures: Vec<(usize, String)> = Vec::new();
    let mut retries: Vec<usize> = Vec::new();
    for i in 0..app.shots.requests.len() {
        if app.shots.requests[i].issued {
            continue;
        }
        app.shots.requests[i].frames = app.shots.requests[i].frames.saturating_add(1);
        let shot = &app.shots.requests[i];
        if !shot_is_settled(now, monitor_inexact, app.timeline_still, shot) {
            continue;
        }
        match try_issue_shot(app, ctx, i) {
            IssueOutcome::Identity(reason) => failures.push((i, reason)),
            IssueOutcome::GeometryRetry => retries.push(i),
            IssueOutcome::CropDeferred => ctx.request_repaint(),
            IssueOutcome::Failed(message) => failures.push((i, message)),
            IssueOutcome::Issued => {}
        }
    }
    for i in retries.into_iter().rev() {
        if let Err(message) = retry_geometry(app, ctx, i) {
            failures.push((i, message));
        }
    }
    reject_failures(app, failures);
    if !app.shots.is_empty() {
        ctx.request_repaint_after(REPAINT_INTERVAL);
    }
}

/// The next step for one drain scan position. `Keep` advances to the next request; every removal
/// variant leaves the index unchanged so it sees the shot shifted into place.
enum DrainStep {
    Keep,
    Timeout,
    Identity(String),
    Geometry,
    Complete(Result<Arc<ColorImage>, CaptureError>),
}

/// Classify one drain position before removing anything: check the deadline first, then
/// identity, then geometry (an unissued request keeps waiting for a retry, while in-flight
/// stale pixels are rejected), and finally take a completed slot. `Keep` leaves the request
/// in place and advances the scan.
fn drain_step(app: &FilmcraftApp, ctx: &egui::Context, index: usize, now: Instant) -> DrainStep {
    let shot = &app.shots.requests[index];
    if now >= shot.deadline {
        return DrainStep::Timeout;
    }
    if let Some(reason) = identity_reason(app, ctx, shot) {
        return DrainStep::Identity(reason.to_string());
    }
    if geometry_reason(app, shot).is_some() {
        if !shot.issued {
            // The next accepted UI pass re-creates the slot against the fresh geometry.
            return DrainStep::Keep;
        }
        // Geometry changed while the callback was already issued: reject stale pixels rather
        // than pairing them with the new surface.
        return DrainStep::Geometry;
    }
    if !shot.issued {
        return DrainStep::Keep;
    }
    let Some(result) = shot.slot.as_ref().and_then(CaptureSlot::take) else {
        return DrainStep::Keep;
    };
    DrainStep::Complete(result)
}

/// Remove one request, cancel its slot and send its error reply; later requests shift down by
/// one index.
fn remove_and_reject(app: &mut FilmcraftApp, index: usize, message: impl std::fmt::Display) {
    let shot = app.shots.requests.remove(index);
    if let Some(slot) = shot.slot {
        slot.cancel();
    }
    reject(shot.reply, shot.token, message);
}

/// Build the saved-image reply, using the rendered scale of the delivered texture for crops.
fn image_reply(shot: &Shot, image: &ColorImage) -> Value {
    if image.size != shot.expected {
        return error_reply(
            shot.token,
            format!(
                "screenshot is {}×{} pixels, expected {}×{}: the window changed size while it was captured",
                image.size[0], image.size[1], shot.expected[0], shot.expected[1]
            ),
        );
    }
    let captured_ppp = shot.geometry.surface.captured_pixels_per_point();
    let mut reply = crate::control::save_screenshot_with_ppp(image, shot.path.as_deref(), shot.crop, captured_ppp);
    if let Some(object) = reply.as_object_mut() {
        object.insert("request".into(), json!(shot.token));
    }
    reply
}

/// Cancel any remaining callback, turn the captured image or error into the reply and send it
/// exactly once. A pixel size that differs from the reserved size becomes an error reply
/// instead of writing an image for the wrong surface.
fn complete_shot(mut shot: Shot, result: Result<Arc<ColorImage>, CaptureError>) {
    if let Some(slot) = shot.slot.take() {
        slot.cancel();
    }
    let reply = match result {
        Ok(image) => image_reply(&shot, &image),
        Err(error) => error_reply(shot.token, error),
    };
    let _ = shot.reply.send(reply);
}

/// Drain completed slots and expire timed-out requests. Call from every logic callback (including
/// hidden ones). The deadline is checked before taking or writing an image.
pub(crate) fn drain(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let mut i = 0;
    while i < app.shots.requests.len() {
        let now = Instant::now();
        match drain_step(app, ctx, i, now) {
            DrainStep::Keep => i += 1,
            DrainStep::Timeout => remove_and_reject(app, i, "no frame was presented before the screenshot timeout"),
            DrainStep::Identity(reason) => remove_and_reject(app, i, format!("screenshot failed: {reason}")),
            DrainStep::Geometry => remove_and_reject(app, i, "screenshot failed: the window geometry changed while the capture was in flight"),
            DrainStep::Complete(result) => {
                let shot = app.shots.requests.remove(i);
                complete_shot(shot, result);
            }
        }
    }
    if !app.shots.is_empty() {
        ctx.request_repaint_after(REPAINT_INTERVAL);
    }
}

/// Cancel every queued/in-flight capture (app exit). Late callbacks see a dropped slot and write
/// nothing; clients receive one error reply instead of hanging past the transport timeout.
pub(crate) fn cancel_all(app: &mut FilmcraftApp) {
    for shot in app.shots.requests.drain(..) {
        if let Some(slot) = shot.slot {
            slot.cancel();
        }
        let _ = shot.reply.send(error_reply(shot.token, "screenshot cancelled: the app is closing"));
    }
}

/// Ask for a painted frame while a request is still queued, and on hidden logic ticks when a
/// capture is waiting for settlement. The screenshot command itself forces one paint once issued.
pub(crate) fn request_hidden_paint(app: &FilmcraftApp, ctx: &egui::Context) {
    if app.shots.has_unissued() {
        ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, channel};

    use egui::{Color32, ColorImage, Rect, ViewportId, ViewportInfo, pos2, vec2};

    use super::*;
    use crate::automation::Registry;
    use filmcraft_engine::Session;

    /// App with a default session, for capture-state unit tests.
    fn app() -> FilmcraftApp {
        FilmcraftApp::new(Session::default())
    }

    /// A fixed 800×600, 1 px/pt capture identity for the app's current document with no focus.
    fn geometry(app: &FilmcraftApp) -> CaptureGeometry {
        CaptureGeometry {
            surface: SurfaceGeometry {
                viewport: Some(egui::vec2(800.0, 600.0)),
                pixels_per_point: 1.0,
                max_texture_side: 8192,
                minimized: false,
                occluded: false,
            },
            document: active_document(app),
            focus: None,
        }
    }

    /// Raw input reporting an 800×600 root viewport.
    fn raw() -> egui::RawInput {
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() }
    }

    /// Admit one uncropped in-memory capture at the default test surface; return its reply
    /// receiver and token.
    fn admit_one(app: &mut FilmcraftApp, ctx: &egui::Context) -> (Receiver<Value>, u64) {
        admit_with_surface(app, ctx, geometry(app).surface)
    }

    /// Bind `surface` as the current capture geometry, admit one uncropped in-memory capture and
    /// return its reply receiver and token.
    fn admit_with_surface(app: &mut FilmcraftApp, ctx: &egui::Context, surface: SurfaceGeometry) -> (Receiver<Value>, u64) {
        app.capture_geometry = Some(CaptureGeometry { surface, document: active_document(app), focus: None });
        let (tx, rx) = channel();
        admit(app, ctx, None, None, tx);
        let token = app.shots.requests.last().map(|shot| shot.token).unwrap_or_default();
        (rx, token)
    }

    /// `note_raw_geometry` uses the fresh raw 2x viewport (1600×1200) rather than the previous
    /// pass's input and bumps the capture epoch.
    #[test]
    fn raw_geometry_uses_the_fresh_surface_not_the_previous_pass() {
        let ctx = egui::Context::default();
        let mut app = app();
        let mut raw = raw();
        let _ = raw.viewports.insert(ViewportId::ROOT, ViewportInfo { native_pixels_per_point: Some(2.0), ..Default::default() });
        note_raw_geometry(&mut app, &ctx, &raw);
        let geometry = app.capture_geometry.expect("geometry from raw input");
        assert_eq!(geometry.surface.expected_size(), Some([1600, 1200]));
        assert!(app.capture_epoch > 0);
    }

    /// Oversized surfaces shrink both axes by one factor to the texture limit and crop at the
    /// matching rendered scale; a surface within the limit keeps the exact native scale.
    #[test]
    fn oversized_windows_predict_the_backends_uniformly_scaled_surface() {
        let app = app();
        let mut surface = geometry(&app).surface;
        surface.viewport = Some(vec2(16_384.0, 4_000.0));
        surface.max_texture_side = 8192;
        assert_eq!(surface.expected_size(), Some([8192, 2000]), "both axes scale by the same factor, like egui-wgpu's surface_fit");
        assert_eq!(surface.captured_pixels_per_point(), 0.5, "the fitted texture is drawn at the same factor");
        surface.viewport = Some(vec2(4_000.0, 16_384.0));
        assert_eq!(surface.expected_size(), Some([2000, 8192]));
        assert_eq!(surface.captured_pixels_per_point(), 0.5);
        surface.viewport = Some(vec2(1920.0, 1080.0));
        assert_eq!(surface.expected_size(), Some([1920, 1080]), "within the limit nothing shrinks");
        assert_eq!(surface.captured_pixels_per_point(), 1.0, "an uncapped surface keeps the native scale exactly");
    }

    /// Unusable viewport data preserves a valid native scale, while an invalid native scale falls
    /// back to 1.0 so a crop can never divide by zero.
    #[test]
    fn captured_scale_falls_back_when_geometry_is_unusable() {
        let app = app();
        let mut surface = geometry(&app).surface;
        surface.pixels_per_point = 2.0;
        surface.viewport = None;
        assert_eq!(surface.captured_pixels_per_point(), 2.0, "no viewport preserves the native scale");
        surface.viewport = Some(vec2(0.0, 600.0));
        assert_eq!(surface.captured_pixels_per_point(), 2.0);
        surface.viewport = Some(vec2(f32::NAN, 600.0));
        assert_eq!(surface.captured_pixels_per_point(), 2.0);
        surface.viewport = Some(vec2(800.0, 600.0));
        surface.pixels_per_point = f32::INFINITY;
        assert_eq!(surface.captured_pixels_per_point(), 1.0, "an invalid native scale falls back to 1, like save_screenshot_with_ppp");
        surface.pixels_per_point = 0.0;
        assert_eq!(surface.captured_pixels_per_point(), 1.0);
    }

    /// A crop in points selects the region where a uniformly downscaled surface actually drew it,
    /// using the rendered rather than the native scale.
    #[test]
    fn downscaled_capture_crops_with_the_rendered_scale() {
        let ctx = egui::Context::default();
        let mut app = app();
        let surface = SurfaceGeometry {
            viewport: Some(vec2(16_384.0, 2_000.0)),
            // A 2x native surface fits at 0.25 render scale: crop at 0.5 pixels per point.
            // This also catches accidentally returning render_scale without the native factor.
            pixels_per_point: 2.0,
            max_texture_side: 8192,
            minimized: false,
            occluded: false,
        };
        assert_eq!(surface.expected_size(), Some([8192, 1_000]));
        assert_eq!(surface.captured_pixels_per_point(), 0.5);
        let (rx, token) = admit_with_surface(&mut app, &ctx, surface);
        let dir = tempfile::tempdir().expect("a temporary screenshot directory");
        let path = dir.path().join("downscaled-crop.png");
        // The crop is 400x200 points, which the fitted surface draws as 200x100 pixels. Paint a
        // red rectangle at exactly that rendered position so a wrong (native) scale would crop
        // black pixels or the wrong dimensions instead of this region.
        let mut image = ColorImage::filled([8192, 1_000], Color32::BLACK);
        for y in 25..125 {
            for x in 50..250 {
                image.pixels[y * 8192 + x] = Color32::RED;
            }
        }
        let shot = &mut app.shots.requests[0];
        shot.path = Some(path.to_string_lossy().into_owned());
        shot.crop = Some([100.0, 50.0, 400.0, 200.0]);
        shot.issued = true;
        let callback = shot.callback.take().expect("a callback");
        callback.complete(Arc::new(image));
        drain(&mut app, &ctx);
        let reply = rx.try_recv().expect("a screenshot reply");
        assert_eq!(reply["request"], token);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!([reply["result"]["width"].as_u64(), reply["result"]["height"].as_u64()], [Some(200), Some(100)]);
        let decoded = image::load_from_memory(&std::fs::read(&path).expect("the saved crop")).expect("decode the saved crop").to_rgba8();
        assert_eq!(decoded.dimensions(), (200, 100));
        assert!(
            decoded.pixels().all(|pixel| *pixel == image::Rgba([255, 0, 0, 255])),
            "the crop must select the red region where the downscaled surface drew it"
        );
    }

    /// A 1.5x surface whose point size rounds up by one window pixel is still uncapped, so its
    /// crop keeps the exact 1.5 scale instead of a slightly shrunk one.
    #[test]
    fn uncapped_hidpi_capture_keeps_the_native_scale() {
        let ctx = egui::Context::default();
        let mut app = app();
        let surface = SurfaceGeometry {
            // 801 points at 1.5x is 1201.5 window pixels, which rounds to 1202. The fitted scale
            // must still be exactly 1.0 rather than 1202/1201.5 from that one-pixel difference.
            viewport: Some(vec2(801.0, 600.0)),
            pixels_per_point: 1.5,
            max_texture_side: 8192,
            minimized: false,
            occluded: false,
        };
        assert_eq!(surface.expected_size(), Some([1202, 900]));
        assert_eq!(surface.captured_pixels_per_point(), 1.5);
        let (rx, token) = admit_with_surface(&mut app, &ctx, surface);
        let dir = tempfile::tempdir().expect("a temporary screenshot directory");
        let path = dir.path().join("hidpi-crop.png");
        let mut image = ColorImage::filled([1202, 900], Color32::BLACK);
        for y in 75..375 {
            for x in 150..750 {
                image.pixels[y * 1202 + x] = Color32::GREEN;
            }
        }
        let shot = &mut app.shots.requests[0];
        shot.path = Some(path.to_string_lossy().into_owned());
        shot.crop = Some([100.0, 50.0, 400.0, 200.0]);
        shot.issued = true;
        let callback = shot.callback.take().expect("a callback");
        callback.complete(Arc::new(image));
        drain(&mut app, &ctx);
        let reply = rx.try_recv().expect("a screenshot reply");
        assert_eq!(reply["request"], token);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!([reply["result"]["width"].as_u64(), reply["result"]["height"].as_u64()], [Some(600), Some(300)]);
        let decoded = image::load_from_memory(&std::fs::read(&path).expect("the saved crop")).expect("decode the saved crop").to_rgba8();
        assert_eq!(decoded.dimensions(), (600, 300));
        assert!(decoded.pixels().all(|pixel| *pixel == image::Rgba([0, 255, 0, 255])), "the HiDPI crop must select the green region at the native scale");
    }

    /// Minimized, occluded and window-focus flags do not bump the epoch or reject an already
    /// issued capture; its pixels are still delivered once.
    #[test]
    fn presentation_flags_do_not_invalidate_an_issued_capture() {
        let ctx = egui::Context::default();
        let mut app = app();
        let (rx, token) = admit_one(&mut app, &ctx);
        let callback = app.shots.requests[0].callback.take().expect("a callback");
        app.shots.requests[0].issued = true;
        // The window is minimized or another application took focus while the callback was in
        // flight. Those flags do not change captured pixels and must not reject the image.
        let epoch = app.capture_epoch;
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            max_texture_side: Some(8192),
            focused: false,
            ..Default::default()
        };
        let viewport = raw.viewports.entry(ViewportId::ROOT).or_default();
        viewport.native_pixels_per_point = Some(1.0);
        viewport.minimized = Some(true);
        viewport.occluded = Some(true);
        note_raw_geometry(&mut app, &ctx, &raw);
        assert_eq!(app.capture_epoch, epoch, "presentation-only raw input retains the issued capture");
        callback.complete(Arc::new(ColorImage::filled([800, 600], Color32::RED)));
        drain(&mut app, &ctx);
        let reply = rx.try_recv().expect("the issued capture completes instead of being rejected");
        assert_eq!(reply["request"], token);
        assert_eq!(reply["ok"], true, "{reply}");
    }

    /// Admission counts completed-but-undrained requests, so a new request over `MAX_REQUESTS`
    /// is rejected instead of replacing a slot that still owes a reply.
    #[test]
    fn admission_counts_outstanding_and_completed_but_undrained_requests() {
        let ctx = egui::Context::default();
        let mut app = app();
        let _receivers: Vec<_> = (0..MAX_REQUESTS).map(|_| admit_one(&mut app, &ctx)).collect();
        assert_eq!(app.shots.requests.len(), MAX_REQUESTS);
        let image = Arc::new(ColorImage::filled([800, 600], Color32::RED));
        app.shots.requests[0].callback.take().expect("a callback").complete(image);
        let (tx, rx) = channel();
        admit(&mut app, &ctx, None, None, tx);
        assert_eq!(app.shots.requests.len(), MAX_REQUESTS, "the fifth request is rejected, not admitted");
        let reply = rx.recv().expect("a rejection reply");
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap_or_default().contains("outstanding"), "{reply}");
    }

    /// A timed-out request replies once; its late callback cannot produce a second reply.
    #[test]
    fn a_late_completion_after_timeout_does_not_write_or_reply_twice() {
        let ctx = egui::Context::default();
        let mut app = app();
        let (rx, token) = admit_one(&mut app, &ctx);
        let callback = app.shots.requests[0].callback.take().expect("a callback");
        app.shots.requests[0].issued = true;
        app.shots.requests[0].deadline = Instant::now() - Duration::from_millis(1);
        drain(&mut app, &ctx);
        let reply = rx.recv().expect("timeout reply");
        assert_eq!(reply["request"], token);
        assert_eq!(reply["ok"], false);
        callback.complete(Arc::new(ColorImage::filled([800, 600], Color32::RED)));
        assert!(rx.try_recv().is_err(), "a late image must not produce another reply");
    }

    /// A near-per-image-limit surface is rejected before any slot exists because its crop/encode
    /// reservation alone exceeds the aggregate bound.
    #[test]
    fn near_limit_images_are_rejected_for_encode_buffer_memory() {
        let ctx = egui::Context::default();
        let mut app = app();
        // 8000×4000 RGBA is exactly 128,000,000 raw bytes, under the per-image bound, but the
        // crop/PNG/base64 copies make its real peak several times larger than the aggregate bound.
        app.capture_geometry = Some(CaptureGeometry {
            surface: SurfaceGeometry {
                viewport: Some(vec2(8000.0, 4000.0)),
                pixels_per_point: 1.0,
                max_texture_side: 16384,
                minimized: false,
                occluded: false,
            },
            document: active_document(&app),
            focus: None,
        });
        let (tx, rx) = channel();
        admit(&mut app, &ctx, None, None, tx);
        assert!(app.shots.requests.is_empty(), "the image is rejected before a slot or buffer is created");
        let reply = rx.recv().expect("a rejection reply");
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap_or_default().contains("budget"), "{reply}");
        assert!(admitted_bytes([8000, 4000]).is_some_and(|bytes| bytes > MAX_TOTAL_BYTES));
    }

    /// A crop resolves only from the current pass's registry; a stale rect and a missing id both
    /// error rather than using old coordinates.
    #[test]
    fn a_crop_is_resolved_from_the_current_pass_not_the_previous_one() {
        let mut registry = Registry::default();
        let old = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
        let fresh = Rect::from_min_size(pos2(300.0, 200.0), vec2(400.0, 250.0));
        registry.add("panel.project", old, "Project");
        registry.begin_frame(); // the previous pass keeps the old rect; this pass has not drawn yet
        let target = CropTarget::Element("panel.project".into());
        assert!(target.resolve(&registry).is_err(), "a stale rect must not be used");
        registry.add("panel.project", fresh, "Project");
        assert_eq!(target.resolve(&registry).expect("fresh rect"), [300.0, 200.0, 400.0, 250.0]);
        assert!(CropTarget::Element("panel.missing".into()).resolve(&registry).is_err());
    }

    /// Issuing waits for the accepted final pass of a multipass layout, not the discarded first
    /// pass.
    #[test]
    fn issue_runs_on_the_accepted_final_pass_of_a_multipass_layout() {
        let ctx = egui::Context::default();
        let mut app = app();
        let (_rx, _token) = admit_one(&mut app, &ctx);
        app.shots.requests[0].ready_at = Instant::now() - Duration::from_millis(1);
        app.shots.requests[0].frames = SETTLE_PASSES;
        app.timeline_still = 2;
        let mut passes = 0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            passes += 1;
            if ui.ctx().current_pass_index() == 0 {
                ui.ctx().request_discard("capture pass test");
            }
            issue(&mut app, ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(passes, 2, "the first pass was discarded and the second accepted");
        assert!(app.shots.requests[0].issued, "the accepted pass must issue the capture");
    }

    /// `issue` consumes `monitor_inexact`, so the signal cannot stay sticky and delay later
    /// screenshots until the five-second settle maximum.
    #[test]
    fn monitor_inexact_is_consumed_by_each_issue_attempt() {
        let ctx = egui::Context::default();
        let mut app = app();
        app.monitor_inexact = true;
        issue(&mut app, &ctx);
        assert!(!app.monitor_inexact, "a sticky flag would delay every later screenshot by up to five seconds");
    }

    /// An unissued request follows a geometry change: its expected size and budget are recomputed
    /// and it waits for fresh settling passes before issuing.
    #[test]
    fn a_geometry_change_retries_an_unissued_capture_with_new_budget() {
        let ctx = egui::Context::default();
        let mut app = app();
        let (_rx, _) = admit_one(&mut app, &ctx);
        app.shots.requests[0].ready_at = Instant::now() - Duration::from_millis(1);
        app.shots.requests[0].frames = SETTLE_PASSES;
        app.timeline_still = 2;
        let mut changed = geometry(&app).surface;
        changed.viewport = Some(egui::vec2(1024.0, 768.0));
        app.capture_geometry = Some(CaptureGeometry { surface: changed, ..geometry(&app) });
        app.capture_epoch += 1;
        issue(&mut app, &ctx);
        assert_eq!(app.shots.requests.len(), 1);
        assert_eq!(app.shots.requests[0].expected, [1024, 768]);
        assert!(!app.shots.requests[0].issued, "the retry waits for its own settling passes");
    }

    /// When a resize pushes both queued shots over the aggregate budget, each is rejected with
    /// its own token and removal of the first cannot shift the second's index.
    #[test]
    fn mixed_order_geometry_failures_reject_every_shot_without_shifting_indices() {
        let ctx = egui::Context::default();
        let mut app = app();
        let (rx0, token0) = admit_one(&mut app, &ctx);
        let (rx1, token1) = admit_one(&mut app, &ctx);
        for shot in &mut app.shots.requests {
            shot.ready_at = Instant::now() - Duration::from_millis(1);
            shot.frames = SETTLE_PASSES;
        }
        app.timeline_still = 2;
        // A resize makes both queued shots need more crop/encode buffers than the aggregate
        // budget, so the geometry retry fails for the last shot first.
        app.capture_geometry = Some(CaptureGeometry {
            surface: SurfaceGeometry {
                viewport: Some(vec2(8000.0, 4000.0)),
                pixels_per_point: 1.0,
                max_texture_side: 16384,
                minimized: false,
                occluded: false,
            },
            document: active_document(&app),
            focus: None,
        });
        app.capture_epoch += 1;
        issue(&mut app, &ctx);
        assert!(app.shots.requests.is_empty(), "both failed shots were removed");
        for (rx, token) in [(rx0, token0), (rx1, token1)] {
            let reply = rx.try_recv().expect("each failed shot gets its own reply");
            assert_eq!(reply["request"], token);
            assert_eq!(reply["ok"], false, "{reply}");
            assert!(reply["error"].as_str().unwrap_or_default().contains("aggregate"), "{reply}");
        }
    }
}
