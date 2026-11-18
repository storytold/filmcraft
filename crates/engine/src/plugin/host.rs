//! The sandbox's view of the session.

use std::sync::Arc;

use craft_plugin::{Access, Manifest};
use filmcraft_project::Project;
use serde_json::Value;

use super::{MAX_NOTE_CHARS, MAX_NOTES, refusal, selection_json, set_setting};
use crate::{EditorState, Session};

/// The sandbox's view of the session: every call is checked by [`refusal`] and run in its JSON
/// form ([`Session::execute`]) on the project the run started in. After each call, [`Probe`]
/// puts back and refuses a call that changed the preferences, started background work or, from a
/// read-only plug-in, edited the project.
pub(super) struct EngineHost<'s> {
    s: &'s mut Session,
    manifest: &'s Manifest,
    /// The project the run started in (its identity survives edits and undo).
    project_instance: Arc<()>,
    notes: Vec<String>,
    /// A call replaced the project: the run fails.
    strayed: bool,
}

impl<'s> EngineHost<'s> {
    pub(super) fn new(s: &'s mut Session, manifest: &'s Manifest) -> Self {
        let project_instance = s.project_instance.clone();
        EngineHost { s, manifest, project_instance, notes: Vec::new(), strayed: false }
    }

    /// What the run asked to show the user, and whether a call left the project.
    pub(super) fn finish(self) -> (Vec<String>, bool) {
        (self.notes, self.strayed)
    }

    /// Whether the project the run started in is still the open one.
    fn on_project(&self) -> bool {
        Arc::ptr_eq(&self.s.project_instance, &self.project_instance)
    }
}

impl craft_plugin::Host for EngineHost<'_> {
    fn call(&mut self, command: &str, params: &str) -> Result<String, String> {
        if self.strayed || !self.on_project() {
            self.strayed = true;
            return Err("plug-ins work on the project they started in, which isn't open any more".into());
        }
        let command = command.trim();
        let params: Value = serde_json::from_str(params).map_err(|e| format!("the params for `{command}` aren't JSON: {e}"))?;
        if let Some(why) = refusal(self.manifest, command, &params) {
            return Err(why);
        }
        let probe = Probe::take(self.s);
        let r = self.s.execute(command, params);
        if let Some(why) = probe.violation(self.s, self.manifest, command) {
            probe.restore(self.s);
            return Err(why);
        }
        if !self.on_project() {
            self.strayed = true;
        }
        r.map(|v| v.to_string()).map_err(|e| e.to_string())
    }

    fn selection(&mut self) -> String {
        selection_json(self.s).to_string()
    }

    fn notify(&mut self, message: &str) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(message.chars().take(MAX_NOTE_CHARS).collect());
        }
    }

    fn log(&mut self, line: &str) {
        // Also in the run's result (`logs`)
        log::info!("plug-in {}: {line}", self.manifest.id);
    }

    fn setting(&mut self, key: &str) -> Option<String> {
        Some(self.s.prefs.plugin_data.settings.get(&self.manifest.id)?.get(key)?.to_string())
    }

    fn set_setting(&mut self, key: &str, value: Option<&str>) -> Result<(), String> {
        set_setting(&mut self.s.prefs.plugin_data.settings, &self.manifest.id, key, value)
    }
}

/// The session as it was before one call, to tell what the call changed and to put it back.
struct Probe {
    project: Arc<Project>,
    state: EditorState,
    undo: usize,
    redo: Vec<(String, Arc<Project>, Option<String>)>,
    merge_key: Option<String>,
    prefs: crate::autosave::Preferences,
    jobs: Vec<u64>,
    /// The project had no unsaved changes.
    clean: bool,
}

impl Probe {
    fn take(s: &Session) -> Probe {
        Probe {
            project: s.project.clone(),
            state: s.state.clone(),
            undo: s.history.undo.len(),
            redo: s.history.redo.clone(),
            merge_key: s.history.merge_key.clone(),
            prefs: s.prefs.clone(),
            jobs: s.jobs.iter().map(|j| j.id).collect(),
            clean: !s.is_dirty(),
        }
    }

    /// Why the call that ran since [`Probe::take`] isn't allowed, from what it changed: the
    /// preferences, background work it started (cancelled here), or, for a read-only plug-in, the
    /// project or its undo history.
    fn violation(&self, s: &mut Session, m: &Manifest, command: &str) -> Option<String> {
        if s.prefs != self.prefs {
            return Some(format!("`{command}` changes FilmCraft's settings, which plug-ins can't"));
        }
        let started: Vec<u64> = s.jobs.iter().map(|j| j.id).filter(|id| !self.jobs.contains(id)).collect();
        if !started.is_empty() {
            stop_jobs(s, &started);
            return Some(format!("`{command}` starts background work, which plug-ins can't"));
        }
        let edited = !Arc::ptr_eq(&s.project, &self.project) || s.history.undo.len() != self.undo;
        if edited && m.permissions.access != Access::Write {
            return Some(craft_plugin::check_read_only(m, command, true).err().unwrap_or_else(|| format!("`{command}` edits the project")));
        }
        None
    }

    /// Put the project, the editing state, the undo history and the preferences back.
    fn restore(self, s: &mut Session) {
        s.project = self.project;
        s.state = self.state;
        s.history.undo.truncate(self.undo);
        s.history.redo = self.redo;
        s.history.merge_key = self.merge_key;
        if s.prefs != self.prefs
            && let Err(e) = s.set_prefs(self.prefs)
        {
            log::warn!("putting the settings back after a plug-in call: {e}");
        }
        // a new revision refreshes what shows the project; a project without unsaved changes
        // stays without them
        s.bump();
        if self.clean {
            s.saved_revision = s.revision;
        }
    }
}

/// Cancel the background jobs `ids` and forget what they would apply when they finish.
fn stop_jobs(s: &mut Session, ids: &[u64]) {
    for j in s.jobs.iter().filter(|j| ids.contains(&j.id)) {
        j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    s.jobs.retain(|j| !ids.contains(&j.id));
    s.media_jobs.retain(|j| !ids.contains(&j.job));
    s.mask_jobs.retain(|j| !ids.contains(&j.job));
    s.scene_jobs.retain(|j| !ids.contains(&j.job));
    s.stepped.retain(|j| !ids.contains(&j.job));
}
