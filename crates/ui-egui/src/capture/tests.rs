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
        surface: SurfaceGeometry { viewport: Some(egui::vec2(800.0, 600.0)), pixels_per_point: 1.0, max_texture_side: 8192, minimized: false, occluded: false },
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
    assert!(decoded.pixels().all(|pixel| *pixel == image::Rgba([255, 0, 0, 255])), "the crop must select the red region where the downscaled surface drew it");
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
        surface: SurfaceGeometry { viewport: Some(vec2(8000.0, 4000.0)), pixels_per_point: 1.0, max_texture_side: 16384, minimized: false, occluded: false },
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
        surface: SurfaceGeometry { viewport: Some(vec2(8000.0, 4000.0)), pixels_per_point: 1.0, max_texture_side: 16384, minimized: false, occluded: false },
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
