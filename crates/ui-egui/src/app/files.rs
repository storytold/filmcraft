use super::{EXPORT_SAVE_DIALOGS, FileDialog, FilmcraftApp, Value, json, panels};

impl FilmcraftApp {
    // ---------------------------------------------------------------- files

    /// Destination of imports started from the currently shown Project panel view.
    pub fn import_bin(&self) -> filmcraft_project::BinId {
        panels::project::view_of(self, panels::project::shown_inst(self)).bin
    }

    /// Open the project at `path`: through [`crate::HostHooks::open_project`] when the host has one
    /// (it opens the project itself, later), else with the `file.open` command. Every open-by-path
    /// goes through here (picker result, Open Recent, Home, Media Browser).
    pub fn open_project_path(&mut self, path: &str) -> Result<Value, String> {
        if let Some(open) = self.hooks.open_project.as_mut() {
            open(path);
            return Ok(json!({"path": path, "pending": true}));
        }
        let r = self.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string());
        if let Err(e) = &r {
            self.ui.status = e.clone();
        }
        r
    }

    /// Offer an import command to [`HostHooks::import_command`]; `Some` when the host took it.
    pub(crate) fn host_import(&mut self, id: &str, params: &Value) -> Option<Value> {
        let mut resolved = params.clone();
        if resolved.get("paths").is_none() {
            let paths = match params.get("path").and_then(Value::as_str) {
                Some(p) => vec![p.to_string()],
                None => self.session.browser.selection.clone(),
            };
            resolved["paths"] = json!(paths);
        }
        let taken = self.hooks.import_command.as_mut().is_some_and(|hook| hook(id, &resolved));
        taken.then(|| json!({"pending": true}))
    }

    /// Import the files a picker (local or remote) returned into `bin` (default: the shown bin).
    pub(crate) fn import_picked(&mut self, bin: Option<Value>, paths: Vec<String>) -> Result<Value, String> {
        if paths.is_empty() {
            return Ok(Value::Null);
        }
        let bin = bin.unwrap_or_else(|| json!(self.import_bin().0));
        if let Some(r) = self.host_import("file.import", &json!({"paths": paths, "bin": bin})) {
            return Ok(r);
        }
        let r = self.session.execute("file.import", json!({"paths": paths, "bin": bin})).map_err(|e| e.to_string());
        if let Ok(v) = &r
            && let Some(errs) = v.get("errors").and_then(Value::as_array)
            && !errs.is_empty()
        {
            self.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
        }
        r
    }

    /// Open the file dialog `id` with the given parameters, or report why it cannot open.
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
                self.pick(FileDialog::open_files(&exts), move |app, paths| app.import_picked(bin, paths))
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
                    app.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "file.open" => self.pick(FileDialog::OpenProject, |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.open_project_path(&path)
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
            "captions.export" => {
                let name = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
                let suggested = format!("{}.srt", name.replace(' ', "_"));
                let dialog = FileDialog::save_as(
                    "Captions (SRT, WebVTT, SCC, MCC, EBU STL, TTML, DFXP)",
                    &["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp"],
                    &suggested,
                );
                let mut p = params.clone();
                self.pick(dialog, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    p["path"] = json!(path);
                    app.session.execute("captions.export", p).map_err(|e| e.to_string())
                })
            }
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
