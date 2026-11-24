use filmcraft_ui_egui::FilmcraftApp;

/// Install the production headless framework before the first harness frame.
pub fn headless_creator<'a>(app: FilmcraftApp) -> impl FnOnce(&mut eframe::CreationContext<'a>) -> FilmcraftApp {
    move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    }
}
