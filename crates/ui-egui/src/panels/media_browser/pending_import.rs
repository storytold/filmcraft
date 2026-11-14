use filmcraft_engine::Session;
use filmcraft_project::ItemId;
use serde_json::json;

/// A pending import belongs to the opened document of this App's Session. Its IDs and history
/// length must never be resolved against a replacement, even when those values are reused.
#[derive(Clone)]
pub(super) struct PendingImport {
    document_epoch: u64,
    items: Vec<u64>,
    history: usize,
}

impl PendingImport {
    pub(super) fn new(session: &Session, items: Vec<u64>, history: usize) -> Self {
        Self { document_epoch: session.document_epoch, items, history }
    }

    pub(super) fn belongs_to(&self, session: &Session) -> bool {
        self.document_epoch == session.document_epoch
    }

    pub(super) fn finish(self, session: &mut Session, landed: bool) {
        if !self.belongs_to(session) {
            return;
        }
        let used_elsewhere = session.history.undo.len() > self.history;
        if !landed && !used_elsewhere && self.items.iter().all(|i| session.project.item(ItemId(*i)).is_some()) {
            let _ = session.execute("edit.undo", json!({}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn wave(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fc-pending-import-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audio.wav");
        std::fs::write(&path, filmcraft_engine::previews::write_wav_f32(&[0.0; 9600], 48_000)).unwrap();
        (dir, path)
    }

    fn import(session: &mut Session, path: &std::path::Path) -> PendingImport {
        let before = session.history.undo.len();
        let result = session.execute("mediaBrowser.import", json!({"paths": [path]})).unwrap();
        let items: Vec<_> = result["items"].as_array().unwrap().iter().map(|id| id.as_u64().unwrap()).collect();
        assert!(!items.is_empty(), "the drag really imported media");
        PendingImport::new(session, items, session.history.undo.len().max(before))
    }

    #[test]
    fn stale_pending_import_cannot_undo_a_replacement_document_edit() {
        let (dir, path) = wave("replacement");
        let mut session = Session::default();
        let pending = import(&mut session, &path);
        let imported: Vec<_> = session.project.items.keys().copied().collect();
        let mut replacement = Session::default();
        replacement.project = session.project.clone();
        replacement.state = session.state.clone();
        let project_path = dir.join("B browser.fcproj");
        replacement.execute("file.saveAs", json!({"path": project_path})).unwrap();
        let epoch = session.document_epoch;
        session.execute("file.open", json!({"path": project_path})).unwrap();
        assert_ne!(session.document_epoch, epoch);
        session.execute("file.newBin", json!({"name": "B work must survive cleanup"})).unwrap();
        assert!(imported.iter().all(|id| session.project.item(*id).is_some()), "B reuses the imported IDs");
        let before = session.project.clone();
        let history = (session.history.undo.len(), session.history.redo.len());
        // A release over the Media Browser is not an accepted target, independently of the
        // shared cancellation latch. Run the same deferred cleanup that show() consumes.
        pending.finish(&mut session, false);
        assert_eq!(session.project, before, "stale MediaBrowser import cleanup undid B edit");
        assert_eq!((session.history.undo.len(), session.history.redo.len()), history);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn same_document_cancellation_undoes_only_its_own_import() {
        let (dir, path) = wave("same-document");
        let mut session = Session::default();
        session.execute("file.newBin", json!({"name": "earlier work"})).unwrap();
        let before = session.project.clone();
        let history = session.history.undo.len();
        let pending = import(&mut session, &path);
        assert!(session.project.items.len() > before.items.len());
        pending.finish(&mut session, false);
        assert_eq!(session.project, before, "the canceled import is removed but earlier work stays");
        assert_eq!(session.history.undo.len(), history);
        let _ = std::fs::remove_dir_all(dir);
    }
}
