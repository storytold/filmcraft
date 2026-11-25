use filmcraft_ui_egui::FilmcraftApp;

/// Install the production headless framework before the first harness frame.
pub fn headless_creator<'a>(app: FilmcraftApp) -> impl FnOnce(&mut eframe::CreationContext<'a>) -> FilmcraftApp {
    move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    }
}

/// Runs `n` frames, moving queued synthetic input into each frame's `RawInput` through the app's own
/// `raw_input_hook`, as eframe does.
#[allow(dead_code)]
pub fn frames(harness: &mut egui_kittest::Harness<'static, FilmcraftApp>, n: usize) {
    for _ in 0..n {
        let ctx = harness.ctx.clone();
        let mut raw = std::mem::take(harness.input_mut());
        eframe::App::raw_input_hook(harness.state_mut(), &ctx, &mut raw);
        *harness.input_mut() = raw;
        harness.step();
    }
}

/// Sends a control request and runs frames (as [`frames`]) until it is answered.
#[allow(dead_code)]
pub fn call(
    harness: &mut egui_kittest::Harness<'static, FilmcraftApp>,
    tx: &std::sync::mpsc::Sender<filmcraft_ui_egui::control::ControlRequest>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let (req, reply) = filmcraft_ui_egui::control::ControlRequest::new(method, params.clone());
    tx.send(req).unwrap();
    for _ in 0..600 {
        frames(harness, 1);
        if let Ok(v) = reply.try_recv() {
            return v;
        }
    }
    panic!("no reply to {method} {params}");
}
