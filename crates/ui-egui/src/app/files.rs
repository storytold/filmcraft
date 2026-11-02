use super::{EXPORT_SAVE_DIALOGS, FileDialog, FilmcraftApp, Value, json, panels};

impl FilmcraftApp {
    // ---------------------------------------------------------------- files

    /// Destination of imports started from the currently shown Project panel view.
    pub fn import_bin(&self) -> filmcraft_project::BinId {
        panels::project::view_of(self, panels::project::shown_inst(self)).bin
    }

    pub fn file_dialog(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        match id {
            "file.import" => {
                let exts: Vec<&str> = filmcraft_media::VIDEO_EXTENSIONS
                    .iter()
                    .chain(filmcraft_media::AUDIO_EXTENSIONS)
                    .chain(filmcraft_media::STILL_EXTENSIONS)
                    .chain(&["srt", "vtt", "scc", "edl", "xml", "fcpxml", "otio", "aaf", "omf"])
                    .copied()
                    .collect();
                let bin = params.get("bin").cloned();
                self.pick(FileDialog::open_files(&exts), move |app, paths| {
                    if paths.is_empty() {
                        return Ok(Value::Null);
                    }
                    let bin = bin.unwrap_or_else(|| json!(app.import_bin().0));
                    let r = app.session.execute("file.import", json!({"paths": paths, "bin": bin})).map_err(|e| e.to_string());
                    if let Ok(v) = &r
                        && let Some(errs) = v.get("errors").and_then(Value::as_array)
                        && !errs.is_empty()
                    {
                        app.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                    }
                    r
                })
            }
            // File ▸ Import with Image Sequence: choose the first numbered still
            "file.importImageSequence" => self.pick(FileDialog::open_files(filmcraft_media::STILL_EXTENSIONS), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = app.session.execute("file.importImageSequence", json!({"path": path})).map_err(|e| e.to_string());
                if let Err(e) = &r {
                    app.ui.status = e.clone();
                }
                r
            }),
            "file.saveAs" | "file.save" | "file.saveCopy" => {
                let suggested =
                    if id == "file.saveCopy" { format!("{} copy.fcproj", self.session.project.name) } else { format!("{}.fcproj", self.session.project.name) };
                let cmd = if id == "file.saveCopy" { "file.saveCopy" } else { "file.saveAs" };
                self.pick(FileDialog::SaveProject { name: suggested }, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    let r = app.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string());
                    match &r {
                        Ok(_) => crate::menus::saved_status(app, cmd, &path),
                        Err(e) => app.ui.status = e.clone(),
                    }
                    r
                })
            }
            "file.open" => self.pick(FileDialog::OpenProject, |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                if let Some(asked) = crate::panels::clip_dialogs::ask_before_open(app, &path) {
                    return asked;
                }
                app.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())
            }),
            // Sequence ▸ Decupador Inteligente ▸ Align Script: the script next to the takes, else choose one
            "decupador.alignScript" => {
                let mut auto = params.clone();
                if !auto.is_object() {
                    auto = json!({});
                }
                match self.session.execute("decupador.alignScript", auto) {
                    Err(e) if e.to_string().contains("no script found") => self.pick(FileDialog::open_files(&["pdf", "md", "txt"]), |app, paths| {
                        let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                        let r = app.session.execute("decupador.alignScript", json!({"path": path})).map_err(|e| e.to_string());
                        if let Err(e) = &r {
                            app.ui.status = e.clone();
                        }
                        r
                    }),
                    Err(e) => {
                        self.ui.status = e.to_string();
                        Err(e.to_string())
                    }
                    Ok(v) => Ok(v),
                }
            }
            // Sequence ▸ Decupador Inteligente ▸ Apply Decoupage Plan: choose the premiere_cuts.json
            "decupador.applyPlan" => self.pick(FileDialog::open_files(&["json"]), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = app.session.execute("decupador.applyPlan", json!({"path": path})).map_err(|e| e.to_string());
                match &r {
                    Ok(v) => {
                        let missing = v.get("missingFiles").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                        if missing > 0 {
                            app.ui.status = format!("{missing} file(s) of the plan are missing");
                        }
                    }
                    Err(e) => app.ui.status = e.clone(),
                }
                r
            }),
            "graphics.newFromFile" => {
                let exts: Vec<&str> = filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).copied().collect();
                self.pick(FileDialog::open_files(&exts), |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    app.session.execute("graphics.newFromFile", json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "captions.import" => self.pick(FileDialog::open_files(&["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp", "xml"]), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute("captions.import", json!({"path": path})).map_err(|e| e.to_string())
            }),
            _ => match EXPORT_SAVE_DIALOGS.iter().find(|(c, ..)| *c == id) {
                Some(&(_, filter, ext)) => self.export_save_dialog(id, filter, ext, params),
                None => Err(format!("no dialog for {id}")),
            },
        }
    }

    /// File ▸ Export ▸ EDL…, Final Cut Pro XML… etc. from the menus (#382): ask where to save, then
    /// run the command with that `path` and the rest of `params`.
    fn export_save_dialog(&mut self, id: &str, filter: &str, ext: &str, params: &Value) -> Result<Value, String> {
        filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&self.session))?;
        if !FileDialog::save_as(filter, &[ext], "").available(&self.hooks) {
            return Err("no save dialog available: run the command with a `path`".into());
        }
        // Timelines are named after the sequence; a selection must not suggest the open project's file.
        let sequence = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone());
        let stem = match id {
            "file.exportAle" => self.session.project.name.clone(),
            "file.exportSelectionProject" => format!("{} selection", self.session.project.name),
            _ => sequence.unwrap_or_else(|| self.session.project.name.clone()),
        };
        let stem: String = stem.chars().map(|c| if matches!(c, '/' | '\\') || c.is_control() { '-' } else { c }).collect();
        let stem = if stem.trim().is_empty() { "Untitled".to_string() } else { stem };
        let mut p = params.as_object().cloned().unwrap_or_default();
        let id = id.to_string();
        self.pick(FileDialog::save_as(filter, &[ext], &format!("{stem}.{ext}")), move |app, paths| {
            let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
            p.insert("path".into(), json!(path));
            let r = app.session.execute(&id, Value::Object(p)).map_err(|e| e.to_string());
            app.ui.status = match &r {
                Ok(_) => tlf!("Exported {path}", path),
                Err(e) => e.clone(),
            };
            r
        })
    }
}
