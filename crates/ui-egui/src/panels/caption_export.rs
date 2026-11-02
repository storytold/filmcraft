//! File ▸ Export ▸ Captions… and the Text panel's Export captions… button: the caption export
//! settings dialog (#3365). The File Format menu lists every format `captions.export` writes; OK
//! asks for the file name with only that format's filter and extension (the host's save panel) and
//! runs `captions.export` with it. A name typed with another caption format's extension is written
//! in that format, since that is the file the save dialog asked about; any other name gets the chosen
//! format's extension ([`destination`]). If a file with that new name already exists, nothing is
//! written: the save dialog only asked about replacing the name as typed (with WebVTT chosen, typing
//! `keep` when `keep.vtt` exists leaves `keep.vtt` alone). The format written last
//! (`caption_export_format` in the UI state) is offered the next time. With a `path` the command runs
//! directly (agents).
//!
//! Automation ids: `captionExport.format`, `captionExport.format.option.<ext>` (`srt`, `vtt`, `scc`,
//! `mcc`, `stl`, `ttml`, `dfxp`), `captionExport.ok`, `captionExport.cancel`.

use crate::widgets::CenteredDialog as _;
use filmcraft_captions::Format;
use serde_json::{Map, Value, json};

use crate::FilmcraftApp;
use crate::native_dialogs::FileDialog;

#[derive(Clone, Debug)]
struct Draft {
    format: Format,
    /// The command's other parameters (`track`, `dropFrame` …), passed on unchanged.
    params: Map<String, Value>,
}

fn draft_id() -> egui::Id {
    egui::Id::unique("caption-export-draft")
}

/// Open the dialog with `captions.export`'s parameters (no `path`). A `format` among them is
/// selected; otherwise the format chosen last, or SRT.
pub fn open(app: &FilmcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    let given = params.get("format").and_then(Value::as_str).and_then(Format::from_name);
    let format = given.or_else(|| app.ui.caption_export_format.as_deref().and_then(Format::from_name)).unwrap_or(Format::Srt);
    let params = params.as_object().cloned().unwrap_or_default();
    ctx.data_mut(|m| m.insert_temp(draft_id(), Some(Draft { format, params })));
    Ok(json!({"dialog": "captionExport"}))
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(Some(mut d)) = ctx.data(|m| m.get_temp::<Option<Draft>>(draft_id())) else { return };
    let mut close = false;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let shown = tl!("Caption Export Settings");
    egui::Window::new(shown).id(egui::Id::unique("Caption Export Settings")).collapsible(false).resizable(false).centered(ctx).show(ctx, |ui| {
        ui.set_min_width(340.0);
        egui::Grid::new("caption-export-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("File Format"));
            let r = craft_ui::ComboBox::from_id_salt("caption-export-format").selected_text(d.format.name()).width(240.0).show_ui(ui, |ui| {
                for f in Format::ALL {
                    let o = ui.selectable_value(&mut d.format, f, f.name());
                    elems.push((format!("captionExport.format.option.{}", f.extension()), o.rect, f.name().into()));
                }
            });
            elems.push(("captionExport.format".into(), r.response.rect, d.format.name().into()));
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let c = ui.button(tl!("Cancel"));
            elems.push(("captionExport.cancel".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                close = true;
            }
            let o = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("captionExport.ok".into(), o.rect, "OK".into()));
            if o.clicked() {
                apply = true;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        close = true;
    }
    if apply {
        close = true;
        let ext = d.format.extension();
        app.ui.caption_export_format = Some(ext.to_string());
        let name = app.session.state.active_sequence.and_then(|s| app.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
        let stem = if name.trim().is_empty() { "Captions".to_string() } else { name.replace([' ', '/', '\\', ':'], "_") };
        let dialog = FileDialog::save_as_named(d.format.name(), &[ext], &format!("{stem}.{ext}"));
        if dialog.available(&app.hooks) {
            let chosen = d.format;
            let mut p = d.params.clone();
            app.pick_ui(dialog, move |app, paths| {
                let Some(answered) = paths.into_iter().next() else { return };
                let (path, format) = destination(&answered, chosen);
                // the save dialog asked before replacing what was typed, not the name with the
                // extension added: `keep` typed when `keep.vtt` exists must not overwrite keep.vtt
                if path != answered {
                    match app.session.services.file_size(&path) {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Ok(_) => {
                            let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                            app.ui.status = tlf!("{name} already exists, so nothing was exported. To replace it, choose it in the save dialog.", name);
                            return;
                        }
                        Err(e) => {
                            app.ui.status = format!("Cannot check export destination: {e}");
                            return;
                        }
                    }
                }
                app.ui.caption_export_format = Some(format.extension().to_string());
                p.insert("format".into(), json!(format.extension()));
                p.insert("path".into(), json!(path));
                match app.session.execute("captions.export", Value::Object(p)) {
                    Ok(v) => {
                        let path = v["path"].as_str().unwrap_or_default();
                        app.ui.status = if format == chosen {
                            tlf!("Exported {path}", path)
                        } else {
                            tlf!("Exported {path} as {format}, from its file extension", path, format = format.name())
                        };
                    }
                    Err(e) => app.ui.status = e.to_string(),
                }
            });
        } else {
            app.ui.status = "no save dialog available: run the command with a `path`".into();
        }
    }
    ctx.data_mut(|m| m.insert_temp(draft_id(), if close { None } else { Some(d) }));
}

/// Where to write, and in which format, from the save dialog's answer and the chosen format. The
/// GTK file chooser behind the Linux portal returns a name exactly as typed, having asked before
/// replacing that name only, and Windows can keep an extension the user typed, so:
/// - a caption format's extension, in any case (`subs.srt`, `SUBS.VTT`), is the user's choice: that
///   file, in that format;
/// - any other name gets the chosen format's extension: `subs` and `subs.` give `subs.vtt`, and
///   `subs.final` gives `subs.final.vtt`.
fn destination(path: &str, chosen: Format) -> (String, Format) {
    let typed = std::path::Path::new(path).extension().and_then(|e| e.to_str());
    match typed.and_then(|e| Format::ALL.into_iter().find(|f| f.extension().eq_ignore_ascii_case(e))) {
        Some(format) => (path.to_string(), format),
        None => (format!("{}.{}", path.trim_end_matches('.'), chosen.extension()), chosen),
    }
}

#[cfg(test)]
mod tests {
    use super::destination;
    use filmcraft_captions::Format;

    #[test]
    fn a_saved_name_gives_the_file_and_its_format() {
        for (typed, saved, format) in [
            ("subs", "subs.vtt", Format::WebVtt),
            ("subs.", "subs.vtt", Format::WebVtt),
            ("subs.vtt", "subs.vtt", Format::WebVtt),
            ("SUBS.VTT", "SUBS.VTT", Format::WebVtt),
            ("subs.srt", "subs.srt", Format::Srt),
            ("Subs.SCC", "Subs.SCC", Format::Scc),
            ("subs.final", "subs.final.vtt", Format::WebVtt),
            ("/home/a.b/subs", "/home/a.b/subs.vtt", Format::WebVtt),
            ("/home/a.b/Main Edit.ttml", "/home/a.b/Main Edit.ttml", Format::Ttml),
        ] {
            assert_eq!(destination(typed, Format::WebVtt), (saved.to_string(), format), "{typed}");
        }
    }
}
