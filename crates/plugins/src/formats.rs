//! `format` contributions: a file with an extension FilmCraft does not know opens through a plug-in.
//!
//! The seam is `MediaPool::register_opener`. `Session::import` of an unknown file ends in
//! `MediaPool::open_bytes`, which tries the registered openers before the built-in readers. The
//! opener of a session holds the [`FormatBook`] of that session's
//! [`PluginHost`](crate::PluginHost), so two hosts in one process never open each other's files.

use std::sync::{Arc, Weak};

use craft_plugin_abi::format::extension_of;
use craft_plugin_host::contrib::format::FormatBook;
use filmcraft_engine::Session;
use filmcraft_media::still::StillSource;
use filmcraft_media::{MediaError, SharedSource};

/// Make `session` open the files that `book` handles. Call once per host.
pub fn attach(session: &Session, book: &Arc<FormatBook>) {
    // A weak handle: the host owns the book, and a dropped host leaves a pool that opens nothing here.
    let book = Arc::downgrade(book);
    session.media.register_opener(Arc::new(move |name, bytes| open(&book, name, &bytes)));
}

fn open(book: &Weak<FormatBook>, name: &str, bytes: &Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    let ext = extension_of(name)?;
    let book = book.upgrade().filter(|b| b.handles(&ext))?;
    Some(book.read(name, bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}"))).map(|doc| {
        let has_alpha = doc.rgba8.chunks_exact(4).any(|p| p[3] != 255);
        Arc::new(StillSource::from_rgba(name, doc.width, doc.height, doc.rgba8, has_alpha, bytes.len() as u64)) as SharedSource
    }))
}
