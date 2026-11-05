//! `format` contributions: a file with an extension FilmCraft does not know opens through a plug-in.
//!
//! The seam is `MediaPool::register_opener`. `Session::import` of an unknown file ends in
//! `MediaPool::open_bytes`, which tries the registered openers before the built-in readers. An
//! opener is a plain `fn`, so it cannot hold the host. It reads the books of every live
//! [`PluginHost`](crate::PluginHost) from [`BOOKS`] instead.

use std::sync::{Arc, RwLock, Weak};

use craft_plugin_abi::format::extension_of;
use craft_plugin_host::contrib::format::FormatBook;
use filmcraft_engine::Session;
use filmcraft_media::still::StillSource;
use filmcraft_media::{MediaError, SharedSource};

/// The format books of the hosts in this process.
static BOOKS: RwLock<Vec<Weak<FormatBook>>> = RwLock::new(Vec::new());

/// Make `session` open the files that `book` handles. Call once per host.
pub fn attach(session: &Session, book: &Arc<FormatBook>) {
    let mut books = BOOKS.write().unwrap_or_else(|e| e.into_inner());
    books.retain(|b| b.strong_count() > 0);
    books.push(Arc::downgrade(book));
    session.media.register_opener(opener);
}

fn opener(name: &str, bytes: Arc<[u8]>) -> Option<Result<SharedSource, MediaError>> {
    let ext = extension_of(name)?;
    let book = BOOKS.read().unwrap_or_else(|e| e.into_inner()).iter().filter_map(Weak::upgrade).find(|b| b.handles(&ext))?;
    Some(book.read(name, &bytes).map_err(|e| MediaError::Decode(format!("{name}: {e}"))).map(|doc| {
        let has_alpha = doc.rgba8.chunks_exact(4).any(|p| p[3] != 255);
        Arc::new(StillSource::from_rgba(name, doc.width, doc.height, doc.rgba8, has_alpha, bytes.len() as u64)) as SharedSource
    }))
}
