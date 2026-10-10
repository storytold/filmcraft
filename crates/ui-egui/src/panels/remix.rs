//! Clip ▸ Remix ▸ Remix Properties…: Target Duration, Segments and Variations of the selected
//! remixed clip. OK runs `clip.remix.properties` (one undo step); the Remix tool in the timeline
//! sets the target by dragging instead.
//!
//! Automation ids: `remixProperties.duration` (seconds), `remixProperties.segments`,
//! `remixProperties.variations` (sliders 0 … 100), `remixProperties.info` (tempo and the duration
//! the remix will have), `remixProperties.ok`, `remixProperties.cancel`.

use serde_json::{Value, json};

use crate::FilmcraftApp;

#[derive(Clone, Debug, PartialEq)]
struct Draft {
    clip: u64,
    seconds: f64,
    segments: f64,
    variations: f64,
    info: String,
}

fn draft_id() -> egui::Id {
    egui::Id::new("remix-properties-draft")
}

/// Open the dialog for the selected remixed clip (the menu entry without parameters).
pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let v = app.session.execute("clip.remix.properties", json!({})).map_err(|e| e.to_string())?;
    let d = Draft {
        clip: v["clip"].as_u64().unwrap_or(0),
        seconds: v["targetSeconds"].as_f64().unwrap_or_else(|| v["seconds"].as_f64().unwrap_or(0.0)),
        segments: v["segments"].as_f64().unwrap_or(50.0),
        variations: v["variations"].as_f64().unwrap_or(50.0),
        info: tlf!("Current duration {seconds} s", seconds = format!("{:.2}", v["seconds"].as_f64().unwrap_or(0.0))),
    };
    ctx.data_mut(|m| m.insert_temp(draft_id(), Some(d)));
    Ok(json!({"dialog": "remixProperties"}))
}

/// Whether the dialog is open.
pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())).flatten().is_some()
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    let mut close = false;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    crate::dialog_style::Window::new(tl!("Remix Properties"))
        .id(egui::Id::new("Remix Properties"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            egui::Grid::new("remix-props").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label(tl!("Target Duration:"));
                let r = ui.add(egui::DragValue::new(&mut d.seconds).speed(0.1).range(1.0..=36_000.0).suffix(" s").max_decimals(2));
                elems.push(("remixProperties.duration".into(), r.rect, format!("{:.2} s", d.seconds)));
                ui.end_row();
                ui.label(tl!("Segments:"));
                let r = ui.add(egui::Slider::new(&mut d.segments, 0.0..=100.0).step_by(1.0).text(tl!("Fewer · More")));
                elems.push(("remixProperties.segments".into(), r.rect, format!("{:.0}", d.segments)));
                ui.end_row();
                ui.label(tl!("Variations:"));
                let r = ui.add(egui::Slider::new(&mut d.variations, 0.0..=100.0).step_by(1.0).text(tl!("Fewer · More")));
                elems.push(("remixProperties.variations".into(), r.rect, format!("{:.0}", d.variations)));
                ui.end_row();
            });
            ui.add_space(6.0);
            let i = ui.weak(&d.info);
            elems.push(("remixProperties.info".into(), i.rect, d.info.clone()));
            ui.add_space(8.0);
            crate::dialog_style::actions(ui, |ui| {
                let c = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
                elems.push(("remixProperties.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    close = true;
                }
                let o = ui.add(crate::dialog_style::primary(tl!("OK")));
                elems.push(("remixProperties.ok".into(), o.rect, "OK".into()));
                if o.clicked() {
                    apply = true;
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    if apply {
        let p = json!({"clip": d.clip, "seconds": d.seconds, "segments": d.segments, "variations": d.variations});
        match app.session.execute("clip.remix.properties", p) {
            Ok(v) => app.ui.status = tlf!("Remixed to {seconds} s", seconds = format!("{:.2}", v["seconds"].as_f64().unwrap_or(0.0))),
            Err(e) => app.ui.status = e.to_string(),
        }
        close = true;
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), if close { None } else { Some(d) }));
}
