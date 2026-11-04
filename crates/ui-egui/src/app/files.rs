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
            "clip.export" => self.export_clip_dialog(params),
            _ => match EXPORT_SAVE_DIALOGS.iter().find(|(c, ..)| *c == id) {
                Some(&(_, filter, ext)) => self.export_save_dialog(id, filter, ext, params),
                None => Err(format!("no dialog for {id}")),
            },
        }
    }

    /// The Timeline's clip menu ▸ Export Clip…: ask where to save the selected clips, suggesting
    /// the clip's name (an MP4 in its source's quality, or a WAV when only sound is selected), then
    /// export them in the background. Without a save dialog (the web app) the file goes to the
    /// default export folder.
    fn export_clip_dialog(&mut self, params: &Value) -> Result<Value, String> {
        filmcraft_engine::find_command("clip.export").map_or(Ok(()), |c| (c.enabled)(&self.session))?;
        let seq = self.session.active_sequence().ok_or("no sequence is open")?;
        // the clips named in `clip` / `clips` (an agent's call), else the selection
        let named: Vec<filmcraft_project::ClipId> = match (params.get("clip"), params.get("clips")) {
            (Some(c), _) => c.as_u64().map(filmcraft_project::ClipId).into_iter().collect(),
            (None, Some(Value::Array(a))) => a.iter().filter_map(Value::as_u64).map(filmcraft_project::ClipId).collect(),
            _ => self.session.state.selection.clone(),
        };
        let picked: Vec<(bool, String)> =
            named.iter().filter_map(|c| seq.find_item(*c)).map(|(track, it)| (seq.video_tracks.iter().any(|t| t.id == track), it.name.clone())).collect();
        if picked.is_empty() {
            return Err(tl!("Select a clip to export").into());
        }
        let video = picked.iter().any(|(v, _)| *v);
        let name = picked.iter().find(|(v, _)| *v == video).map(|(_, n)| n.clone()).unwrap_or_default();
        let (filter, ext) = if video { ("MPEG-4 video", "mp4") } else { ("WAV audio", "wav") };
        let stem = std::path::Path::new(&name).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stem: String = stem.chars().map(|c| if matches!(c, '/' | '\\') || c.is_control() { '-' } else { c }).collect();
        let stem = if stem.trim().is_empty() { "Clip".to_string() } else { stem };
        let mut p = params.as_object().cloned().unwrap_or_default();
        let dialog = FileDialog::save_as(filter, &[ext], &format!("{stem}.{ext}"));
        if !dialog.available(&self.hooks) {
            return export_clip_now(self, Value::Object(p));
        }
        self.pick(dialog, move |app, paths| {
            let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
            p.insert("path".into(), json!(path));
            export_clip_now(app, Value::Object(p))
        })
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

/// Run `clip.export` with `params` and say in the status line where the file is going.
fn export_clip_now(app: &mut FilmcraftApp, params: Value) -> Result<Value, String> {
    let r = app.session.execute("clip.export", params).map_err(|e| e.to_string());
    app.ui.status = match &r {
        Ok(v) => {
            let path = v["path"].as_str().unwrap_or_default().to_string();
            tlf!("Exporting clip to {path}", path)
        }
        Err(e) => e.clone(),
    };
    r
}
