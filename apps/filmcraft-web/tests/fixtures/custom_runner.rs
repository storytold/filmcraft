use filmcraft_engine::{Services, Session};
use filmcraft_ui_egui::FilmcraftApp;
use std::sync::Arc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn probe_exclusive_writes() -> Result<bool, JsValue> {
    let services = filmcraft_web::fs::WebServices;
    filmcraft_web::fs::reserve_blob_name("reserved-probe.wav");
    match services.write_file_new("/files/reserved-probe.wav", b"replacement") {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        _ => return Err("exclusive writes must reject reserved recovery media".into()),
    }
    if filmcraft_web::fs::mem_bytes("/files/reserved-probe.wav").is_some() {
        return Err("reserved recovery media must remain absent from the file table".into());
    }
    services.write_file_new("/exports/exclusive-probe.bin", b"original").map_err(|error| JsValue::from_str(&error.to_string()))?;
    match services.write_file_new("/exports/exclusive-probe.bin", b"replacement") {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        _ => return Err("exclusive writes must reject occupied paths".into()),
    }
    if filmcraft_web::fs::mem_bytes("/exports/exclusive-probe.bin").as_deref() != Some(b"original".as_slice()) {
        return Err("an exclusive-write rejection must preserve existing bytes".into());
    }
    Ok(true)
}

#[wasm_bindgen]
pub async fn start_custom() -> Result<(), JsValue> {
    let canvas = web_sys::window().ok_or("no window")?.document().ok_or("no document")?.get_element_by_id("filmcraft_canvas").ok_or("no canvas")?.dyn_into()?;
    filmcraft_web::api::install()?;
    let mut options = eframe::WebOptions::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(config) = &mut options.wgpu_options.wgpu_setup {
        config.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    eframe::WebRunner::new()
        .start(
            canvas,
            options,
            Box::new(|cc| {
                filmcraft_web::set_context(cc.egui_ctx.clone());
                let context = cc.egui_ctx.clone();
                filmcraft_web::fs::set_on_data(move || context.request_repaint());
                let session = Session::new(Arc::new(filmcraft_web::fs::WebServices));
                let (sender, receiver) = std::sync::mpsc::channel();
                filmcraft_web::api::set_sender(sender);
                Ok(Box::new(filmcraft_web::WebApp::new(FilmcraftApp::new(session).with_control(receiver))))
            }),
        )
        .await
}
