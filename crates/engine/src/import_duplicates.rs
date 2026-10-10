//! `file.import` of a file the project already has (#356): no second item.
//!
//! Dragging a folder into a project twice, or the same file under two spellings of its path, used
//! to add every file again. Each path of an import is now looked up in three tiers, cheapest
//! first; the first tier that knows the file decides.
//!
//! 1. **Path.** Both paths are normalised by the host ([`Services::canonical_path`]: absolute,
//!    `.` and `..` resolved, symlinks followed, the letter case of the volume) and compared. Equal
//!    paths are the same file, with nothing more to check: this alone is #356 as reported. A host
//!    that cannot normalise (the web) compares the paths as written.
//! 2. **File identity, confirmed by the fingerprint.** Two paths that normalise differently can
//!    still be one file: a hard link. The host says which file a path is
//!    ([`Services::file_identity`]: device + inode), but network and FUSE mounts hand out numbers
//!    that are not unique, so a match only counts when the *media identity* of the two paths
//!    ([`MediaIdentity`]: size + head/tail fingerprint, [`crate::relink::identity_of`]) agrees
//!    too. Two reads of at most 2 MiB, and only when the numbers already matched.
//! 3. **Media identity.** The content, wherever the file is:
//!    - it is the media of an item that is offline or missing: nothing is imported, the match is
//!      reported (`relink`) and the Link Media dialog is asked for, so the user reconnects the
//!      clip they have instead of getting a second one;
//!    - Project Settings ▸ Ingest copies files, and the copy this file would be ingested to is
//!      already in the project with the same content: a duplicate (the original imported again).
//!
//! A **copy** of a file is not a duplicate: another path, another file identity, and tier 3 only
//! speaks for offline items and ingested copies. Proxy and versioning workflows keep copies on
//! purpose, and each is its own item.
//!
//! An **image sequence** is the frames beside its first frame, so it is that frame's name in its
//! directory: only tier 1 applies, on the normalised directory. The same still imported as a
//! single file is a different item.
//!
//! Nothing here is saved in the project: paths and file identities are asked of the host once per
//! command ([`Known::of`]).

use std::collections::HashMap;

use filmcraft_media::MediaKind;
use filmcraft_project::{ItemId, MediaIdentity, MediaRef};

use crate::{FileIdentity, Services, Session};

/// What the project already knows about a path being imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// The file is `item`. `tier`: `path` (tier 1), `file` (tier 2) or `ingested` (tier 3).
    Duplicate { item: ItemId, tier: &'static str },
    /// The file is the media of these offline or missing items (tier 3).
    Relink(Vec<ItemId>),
}

/// The media files of the project as the three tiers look them up, built once per `file.import`.
/// Of several items of one file (projects from before #356) the oldest is the one reported.
#[derive(Default)]
pub(crate) struct Known {
    /// Tier 1: online items by normalised path, apart for image sequences.
    by_path: HashMap<(String, bool), ItemId>,
    /// Tier 2: online single files by file identity, with their path for the confirmation.
    by_file: HashMap<FileIdentity, Vec<(ItemId, String)>>,
    /// Tier 3: offline and missing single files with the identity saved at import.
    offline: Vec<(ItemId, MediaIdentity)>,
}

/// The path the tiers compare. An image sequence keeps the name of its first frame as written
/// (a link to the frame under another name starts another sequence) in its normalised directory.
fn normalised(services: &dyn Services, path: &str, sequence: bool) -> String {
    if !sequence {
        return services.canonical_path(path).unwrap_or_else(|| path.to_string());
    }
    let (dir, name) = match path.rfind(['/', '\\']) {
        // (the separator stays with the directory: `/a.0001.png` is in `/`)
        Some(i) => (path.get(..=i).unwrap_or(""), path.get(i + 1..).unwrap_or(path)),
        None => (".", path),
    };
    match services.canonical_path(dir) {
        Some(d) => format!("{}/{name}", d.trim_end_matches(['/', '\\'])),
        None => path.to_string(),
    }
}

impl Known {
    pub(crate) fn of(s: &Session) -> Known {
        let mut k = Known::default();
        for it in s.project.items.values() {
            let Some(m) = it.as_media() else { continue };
            let MediaRef::File { path } = &m.media else { continue };
            let sequence = m.info.kind == MediaKind::ImageSequence;
            // made offline on purpose, or gone: not a file to be a duplicate of
            if m.offline || s.services.file_size(path).is_err() {
                if let (false, Some(id)) = (sequence, m.identity) {
                    k.offline.push((it.id, id));
                }
                continue;
            }
            k.add(&*s.services, path, sequence, it.id);
        }
        k
    }

    /// Remember a file `file.import` just added: the same file later in `paths` is a duplicate.
    pub(crate) fn add(&mut self, services: &dyn Services, path: &str, sequence: bool, item: ItemId) {
        self.by_path.entry((normalised(services, path, sequence), sequence)).or_insert(item);
        if !sequence && let Some(f) = services.file_identity(path) {
            self.by_file.entry(f).or_default().push((item, path.to_string()));
        }
    }

    /// What importing `path` (as an image sequence, or as a single file) would repeat. `None`:
    /// new media, or no such file (importing it says why).
    pub(crate) fn find(&self, s: &Session, path: &str, sequence: bool) -> Option<Found> {
        let services = &*s.services;
        let size = services.file_size(path).ok()?;
        // 1: the path
        if let Some(&item) = self.by_path.get(&(normalised(services, path, sequence), sequence)) {
            return Some(Found::Duplicate { item, tier: "path" });
        }
        if sequence {
            return None;
        }
        // the media identity of `path`, read when a tier first needs it
        let mut read = None;
        let mut identity = || *read.get_or_insert_with(|| crate::relink::identity_of(services, path).ok());
        // 2: the file identity, if the content agrees
        if let Some(same) = services.file_identity(path).and_then(|f| self.by_file.get(&f)) {
            for (item, other) in same {
                if let Some(id) = identity()
                    && crate::relink::identity_of(services, other).is_ok_and(|o| o == id)
                {
                    return Some(Found::Duplicate { item: *item, tier: "file" });
                }
            }
        }
        // 3: the content. The copy ingest would make of this file is already in the project…
        if let Some(out) = crate::proxies::ingest_copy_path(&s.project.settings.ingest, path) {
            let out = out.to_string_lossy();
            if let Some(&item) = self.by_path.get(&(normalised(services, &out, false), false))
                && let Some(id) = identity()
                && crate::relink::identity_of(services, &out).is_ok_and(|o| o == id)
            {
                return Some(Found::Duplicate { item, tier: "ingested" });
            }
        }
        // …or it is what an offline item lost (the saved size first: no read for other files)
        if self.offline.iter().any(|(_, o)| o.size == size)
            && let Some(id) = identity()
        {
            let items: Vec<ItemId> = self.offline.iter().filter(|(_, o)| *o == id).map(|(i, _)| *i).collect();
            if !items.is_empty() {
                return Some(Found::Relink(items));
            }
        }
        None
    }
}
