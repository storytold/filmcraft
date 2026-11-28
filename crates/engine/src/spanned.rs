//! Spanned multi-part recordings: files the camera split across several same-named
//! parts (`VID_<date>_<time>_<lens>_<seg>.insv`, `…_055.insv`, `…_056`, …) that play
//! back to back as one clip.
//!
//! Insta360 cameras write a long recording as a chain of such parts, each a complete
//! file on its own; the trailer of every part names the recording's first part. When
//! the pool opens any part, [`open_spanned`] looks for its siblings in the same folder
//! and joins them with [`filmcraft_codecs::SpannedSource`], so the timeline sees one
//! clip with the whole duration — importing any part imports the whole recording, as
//! the camera's own apps do. A part whose siblings are missing (or damaged) opens
//! standalone rather than failing: never less playable than before the join existed.
//!
//! Insta360 cameras also write a low-resolution proxy chain (`LRV_…_.lrv`) beside the
//! main files; its trailer names the recording's first *main* part, so the two chains
//! are matched by recording (date and time), not by file-name prefix. The engine
//! attaches the proxy chain to imported recordings ([`crate::proxies`]).

use std::sync::Arc;

use filmcraft_media::{MediaError, SharedSource};

use crate::Services;

/// Most parts one recording may have before we stop looking (the camera numbers them
/// `000`…`999`; a real session stays far below this).
const MAX_PARTS: usize = 1000;

/// The parts of the recording `path` belongs to, in play order, when it is a spanned
/// multi-part file: its own name parses as a part (`VID_…_<seg>.insv`), its trailer
/// names a first part, and at least two matching siblings exist next to it.
pub fn spanned_parts(path: &str, services: &dyn Services) -> Option<Vec<String>> {
    let (dir, name) = split_path(path)?;
    let (prefix, _) = filmcraft_codecs::insta360::recording_part(name)?;
    // the trailer marks spanned recordings by naming the recording's first part
    // (an LRV proxy part's trailer names the recording's first MAIN part, so the
    // two chains match by recording, not by name prefix)
    let meta = trailer_meta(path, services)?;
    let first = meta.first_part.as_deref()?;
    let (first_prefix, first_seg) = filmcraft_codecs::insta360::recording_part(first.rsplit(['/', '\\']).next()?)?;
    if filmcraft_codecs::insta360::recording_key(&first_prefix) != filmcraft_codecs::insta360::recording_key(&prefix) {
        return None; // trailer points at a different recording: treat this file standalone
    }
    // A chain is one file's own extension throughout: the camera writes every
    // part of a chain identically, and a same-named `.mp4` copy beside the real
    // `.insv` chain (an export, a transcode) is that recording's twin, not two
    // more parts. A renamed recording keeps its camera name and its trailer, so
    // the *named* first part is matched by segment number, not file name.
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let names = services.list_dir(dir)?.ok()?;
    let mut found: Vec<(u32, String)> = names
        .iter()
        .filter_map(|n| {
            let (p, seg) = filmcraft_codecs::insta360::recording_part(n)?;
            let same_ext = n.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case(&ext));
            (p == prefix && same_ext).then(|| (seg, join_path(dir, n)))
        })
        .collect();
    found.sort_by_key(|(seg, _)| *seg);
    // The parts have to be the unbroken segment run the camera recorded - the
    // duration SpannedSource reports is a sum, so a missing middle part would
    // silently shorten the clip, and the play would lose the cut points the
    // trailer's numbering implies.
    if found.len() < 2 || found.len() > MAX_PARTS || found[0].0 != first_seg || found.windows(2).any(|w| w[1].0 != w[0].0 + 1) {
        return None;
    }
    // And every part must actually be this recording's: its trailer names a
    // first part of the same date and time. A same-prefixed file whose trailer
    // disagrees (a copy of one part renamed into place, a half-restored card)
    // would otherwise join as a sibling and corrupt the clip.
    for (_, p) in &found {
        let ok = trailer_meta(p, services)
            .and_then(|m| m.first_part)
            .and_then(|f| {
                let (fp, _) = filmcraft_codecs::insta360::recording_part(f.rsplit(['/', '\\']).next()?)?;
                Some(filmcraft_codecs::insta360::recording_key(&fp) == filmcraft_codecs::insta360::recording_key(&prefix))
            })
            .unwrap_or(false);
        if !ok {
            log::debug!("spanned clip {path}: sibling {p} names another recording");
            return None;
        }
    }
    Some(found.into_iter().map(|(_, p)| p).collect())
}

/// Open `path` as the spanned clip it belongs to, opening each part with `open` (the
/// pool's single-file opener). None: not a spanned file, its parts are not all
/// openable, or the join is refused (mismatched parts) — the caller then opens `path`
/// alone, exactly as before.
pub fn open_spanned(path: &str, services: &dyn Services, open: impl Fn(&str) -> Result<SharedSource, MediaError>) -> Option<SharedSource> {
    let parts = spanned_parts(path, services)?;
    let mut opened = Vec::with_capacity(parts.len());
    for p in &parts {
        match open(p) {
            Ok(s) => opened.push(s),
            Err(e) => {
                log::debug!("spanned clip {path}: part {p} did not open ({e}); falling back to the single file");
                return None;
            }
        }
    }
    let (_, name) = split_path(path)?;
    match filmcraft_codecs::SpannedSource::new(name, opened) {
        Ok(s) => Some(Arc::new(s) as SharedSource),
        Err(e) => {
            log::debug!("spanned clip {path}: parts refused to join ({e})");
            None
        }
    }
}

/// The camera's own low-resolution proxy for `path` (an Insta360 main file, one part of
/// a spanned recording or a standalone one): the `LRV_…_.lrv` twin of the same segment
/// beside it, written by the camera with the same trailer. Opening the twin joins its
/// own chain through the same spanned machinery, so one path stands in for the whole
/// recording. None when the camera wrote no proxy, `path` is not a main file, or the
/// proxy chain does not cover exactly the main chain's segments (its missing or extra
/// parts mean its duration could not match).
pub fn camera_proxy(path: &str, services: &dyn Services) -> Option<String> {
    let (dir, name) = split_path(path)?;
    let (prefix, seg) = filmcraft_codecs::insta360::recording_part(name)?;
    // only main files have a proxy; the LRV chain is the proxy itself. The
    // recording's own extension is not restricted to .insv: renamed or
    // re-wrapped recordings keep the camera's name and trailer.
    if !prefix.get(..4).is_some_and(|f| f.eq_ignore_ascii_case("VID_")) {
        return None;
    }
    let key = filmcraft_codecs::insta360::recording_key(&prefix)?;
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let names = services.list_dir(dir)?.ok()?;
    // The segment numbers each chain holds. Counting files was not enough: an
    // LRV set of 054/056/057 beside a main 054/055/056 matches in count, yet
    // lacks 055's picture and brings a segment nobody asked for - a proxy must
    // cover the recording exactly to stand in for it (its duration could not
    // match, and its parts' joins could not agree with the main chain's).
    let mut main_segs = std::collections::BTreeSet::new();
    let mut proxy_segs = std::collections::BTreeSet::new();
    let mut twin = None;
    for n in &names {
        let Some((p, s)) = filmcraft_codecs::insta360::recording_part(n) else { continue };
        if filmcraft_codecs::insta360::recording_key(&p) != Some(key) {
            continue;
        }
        if p.eq_ignore_ascii_case(&prefix) {
            // the recording's own chain, whatever it is named on disk
            if n.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case(&ext)) {
                main_segs.insert(s);
            }
        } else if p.get(..4).is_some_and(|f| f.eq_ignore_ascii_case("LRV_")) {
            proxy_segs.insert(s);
            if s == seg {
                twin = Some(join_path(dir, n));
            }
        }
    }
    (!main_segs.is_empty() && proxy_segs == main_segs).then_some(twin?).filter(|t| services.file_size(t).is_ok_and(|sz| sz > 72))
}

/// The INSV trailer of `path`, read through the host's range access (never whole-file).
pub(crate) fn trailer_meta(path: &str, services: &dyn Services) -> Option<filmcraft_insta360::Meta> {
    let len = services.file_size(path).ok()?;
    if len < 72 {
        return None;
    }
    struct Svc<'a> {
        path: &'a str,
        len: u64,
        s: &'a dyn Services,
    }
    impl filmcraft_insta360::ByteSource for Svc<'_> {
        fn len(&self) -> u64 {
            self.len
        }
        fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
            let b = self.s.read_range(self.path, off, buf.len())?;
            if b.len() != buf.len() {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read"));
            }
            buf.copy_from_slice(&b);
            Ok(())
        }
    }
    filmcraft_insta360::parse(&Svc { path, len, s: services }).ok().flatten()
}

/// `(parent, file)` of a path, or None when it has no directory part.
fn split_path(path: &str) -> Option<(&str, &str)> {
    let i = path.rfind(['/', '\\'])?;
    Some((&path[..i], &path[i + 1..]))
}

/// Whether `path`'s name follows the camera's naming for parts of a spanned
/// recording (whatever extension, whatever siblings exist on the host). The
/// media pool uses this to tell a provably single-file recording's opened
/// success from a multi-part one whose join this pass may only have declined
/// because another part's bytes were still on their way (see
/// `crate::media_pool`'s resolve).
pub(crate) fn has_part_name(path: &str) -> bool {
    path.rsplit(['/', '\\']).next().is_some_and(|n| filmcraft_codecs::insta360::recording_part(n).is_some())
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with(['/', '\\']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// An in-memory card: files by path, listed by directory.
    struct Card {
        files: Mutex<BTreeMap<String, Arc<[u8]>>>,
    }

    impl Card {
        fn new() -> Self {
            Self { files: Mutex::new(BTreeMap::new()) }
        }
        fn add(&self, path: &str, bytes: Vec<u8>) {
            let mut f = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.insert(path.to_string(), bytes.into());
        }
    }

    impl Services for Card {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            let f = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.get(path).map(|b| b.to_vec()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
        }
        fn write_file(&self, _path: &str, _data: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            self.read_file(path).map(|b| b.len() as u64)
        }
        fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
            let prefix = format!("{dir}/");
            let f = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Some(Ok(f.keys().filter_map(|p| p.strip_prefix(&prefix).filter(|r| !r.is_empty() && !r.contains('/')).map(str::to_string)).collect()))
        }
    }

    const DIR: &str = "/card/DCIM/Camera01";

    fn part_path(seg: u32) -> String {
        format!("{DIR}/VID_20261010_205908_00_{seg:03}.insv")
    }

    fn lrv_path(seg: u32) -> String {
        format!("{DIR}/LRV_20261010_205908_01_{seg:03}.lrv")
    }

    fn first_part() -> String {
        format!("/DCIM/Camera01/{}", part_path(54).rsplit('/').next().unwrap_or_default())
    }

    /// Three parts on the card, each two frames at 25 fps, alternating which circle is
    /// bright, so a frame's left half says which part presented it.
    fn card() -> Card {
        let c = Card::new();
        for (i, seg) in [54u32, 55, 56].iter().enumerate() {
            let bytes = filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, i % 2 == 1).expect("build");
            c.add(&part_path(*seg), bytes);
        }
        c
    }

    /// An LRV chain with as many parts as the main one, but not the same
    /// segments (a lost 055, a leftover 057 from the previous recording's clock):
    /// counting files called it a proxy, and the import then attached a chain
    /// whose durations could not match. Coverage must be exact.
    #[test]
    fn an_lrv_chain_of_equal_count_but_different_parts_is_not_a_proxy() {
        let c = card();
        for (i, seg) in [54u32, 56, 57].iter().enumerate() {
            c.add(&lrv_path(*seg), filmcraft_codecs::insta360::synthetic_lrv(Some(&first_part()), 2, i % 2 == 1).expect("build lrv"));
        }
        for seg in [54u32, 55, 56] {
            assert_eq!(camera_proxy(&part_path(seg), &c), None, "main part {seg}: the chain misses or adds a part");
        }
        // a lone extra LRV part is refused the same way: exact coverage only
        // (a complete matching chain still pairs - every_main_part_finds_its_lrv_twin)
        let c2 = card_with_lrvs();
        c2.add(&lrv_path(57), filmcraft_codecs::insta360::synthetic_lrv(Some(&first_part()), 2, false).expect("build lrv"));
        assert_eq!(camera_proxy(&part_path(55), &c2), None, "a chain with a part the recording never had");
    }

    /// A host like the card but with a switch that makes every read of a part
    /// other than the first one of either chain (main 054 / proxy 054) come back
    /// "still fetching" (pending), the way a web import reads parts in the order
    /// they load.
    struct SiblingFetching {
        card: Card,
        fetching: std::sync::atomic::AtomicBool,
    }

    impl Services for SiblingFetching {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            if self.fetching.load(std::sync::atomic::Ordering::Relaxed) && !(path.contains("00_054") || path.contains("01_054")) {
                return Err(filmcraft_media::pending::would_block());
            }
            self.card.read_file(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            self.card.write_file(path, data)
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            self.card.file_size(path)
        }
        fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            if self.fetching.load(std::sync::atomic::Ordering::Relaxed) && !(path.contains("00_054") || path.contains("01_054")) {
                filmcraft_media::pending::mark();
                return Some(Err(filmcraft_media::pending::would_block()));
            }
            match self.card.read_file(path) {
                Ok(b) => Some(Ok(Arc::new(filmcraft_media::reader::MemReader(b.into())))),
                Err(e) => Some(Err(e)),
            }
        }
        fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
            self.card.list_dir(dir)
        }
    }

    /// The pool's cache of an opened part may not freeze the join away: while a
    /// sibling's bytes are on their way the recording plays as its one readable
    /// part, and the resolve after they land must be the whole clip - not the
    /// shortened single part the first pass opened.
    #[test]
    fn a_pending_sibling_keeps_the_shortened_open_out_of_the_cache() {
        use std::sync::atomic::Ordering;
        let card = card();
        let pool = crate::media_pool::MediaPool::default();
        let info = pool.open_file(&part_path(54), &card).expect("part 54 opens").info().clone();
        let mut p = filmcraft_project::Project::new("spanned pending");
        let item = p.add_item(
            "VID_20261010_205908_00_054.insv",
            filmcraft_project::Label::Lavender,
            filmcraft_project::ItemKind::Media(Box::new(filmcraft_project::MediaClip {
                media: filmcraft_project::MediaRef::File { path: part_path(54) },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        let host = SiblingFetching { card, fetching: std::sync::atomic::AtomicBool::new(true) };
        let alone = pool.full_res_source(&p, item, &host).expect("the readable part plays");
        assert_eq!(alone.info().duration, filmcraft_time::FrameRate::FPS_25.tick_of(2), "one part while its siblings fetch");
        assert!(!alone.info().container.contains("(spanned)"));
        assert!(filmcraft_media::pending::take(), "the declined join reported why");
        host.fetching.store(false, Ordering::Relaxed);
        let joined = pool.full_res_source(&p, item, &host).expect("resolved again");
        assert!(joined.info().container.ends_with("(spanned)"), "the parts joined once they were all readable");
        assert_eq!(joined.info().duration, filmcraft_time::FrameRate::FPS_25.tick_of(6), "3 parts x 2 frames");
    }

    /// The pool's media_key / proxy machinery needs a real Project around one
    /// part, imported with `info` (the opened source's).
    fn proj_with(path: &str, proxy: Option<String>, info: filmcraft_media::MediaInfo) -> (filmcraft_project::Project, filmcraft_project::ItemId) {
        let mut p = filmcraft_project::Project::new("spanned pool");
        let item = p.add_item(
            path.rsplit(['/', '\\']).next().unwrap_or(path),
            filmcraft_project::Label::Lavender,
            filmcraft_project::ItemKind::Media(Box::new(filmcraft_project::MediaClip {
                media: filmcraft_project::MediaRef::File { path: path.to_string() },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: proxy.map(|path| filmcraft_project::MediaRef::File { path }),
                identity: None,
            })),
            None,
        );
        (p, item)
    }

    /// A camera LRV chain stands in for a spanned recording - but one proxy
    /// part opened while its siblings were still fetching must not freeze that
    /// shortened chain in the proxy cache: the resolve after the bytes land has
    /// to be the whole proxy chain again.
    #[test]
    fn a_pending_proxy_part_is_not_cached_short() {
        use std::sync::atomic::Ordering;
        let pool = crate::media_pool::MediaPool::default();
        pool.set_use_proxies(true);
        let c = card_with_lrvs();
        let info = pool.open_file(&part_path(54), &c).expect("spanned open").info().clone();
        let (p, item) = proj_with(&part_path(54), Some(lrv_path(54)), info);
        let host = SiblingFetching { card: c, fetching: std::sync::atomic::AtomicBool::new(true) };
        let s1 = pool.source_for(&p, item, &host).expect("the readable proxy part");
        // the mean luma of the picture's left eighth says which part's flip
        // pattern is showing (see joining_sums_the_parts_and_maps_frames)
        let mean_left = |src: &dyn filmcraft_media::MediaSource, frame: i64| -> u32 {
            let f = src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_25.tick_of(frame))).expect("frame");
            let l = f.luma8();
            let w = f.width as usize;
            (0..f.height as usize).map(|r| u64::from(l[r * w + w / 8])).sum::<u64>() as u32 / f.height
        };
        // while LRV 055/056 fetch, the chain is the lone part: time 3 shows no new
        // content, only part 054's frames clamped back
        assert_eq!(mean_left(&*s1, 0), mean_left(&*s1, 3), "the readable proxy part stands in for the chain meanwhile");
        assert!(filmcraft_media::pending::take(), "the declined chain said why");
        host.fetching.store(false, Ordering::Relaxed);
        // the shortened chain must not have stuck: the next resolve retries the
        // join, and once joined, frame 3 belongs to LRV 055 - a different flip
        let s2 = pool.source_for(&p, item, &host).expect("resolved again");
        assert_ne!(mean_left(&*s1, 3), mean_left(&*s2, 3), "the proxy cache froze at the part that fetched first");
    }

    /// While a sibling part is still fetching, the job's own-source pass must
    /// not keep (or freeze in the job's cache) the shortened one-part copy the
    /// fallback opens: like the monitors, the job shares the pool's readable
    /// part meanwhile and gets the whole recording once every part arrived.
    #[test]
    fn a_job_retries_a_join_while_siblings_fetch() {
        use filmcraft_render::SourceProvider;
        use std::sync::atomic::Ordering;
        let pool = std::sync::Arc::new(crate::media_pool::MediaPool::default());
        let c = card();
        let info = pool.open_file(&part_path(54), &c).expect("spanned open").info().clone();
        let (p, item) = proj_with(&part_path(54), None, info);
        let p = std::sync::Arc::new(p);
        let host = std::sync::Arc::new(SiblingFetching { card: c, fetching: std::sync::atomic::AtomicBool::new(true) });
        let job = pool.full_res_provider(p.clone(), host.clone()).own_sources();
        let while_pending = job.source(item).expect("a frame source: the readable part");
        assert!(!while_pending.info().container.contains("(spanned)"), "only this part's bytes are here yet");
        assert!(filmcraft_media::pending::take(), "the declined join said why");
        host.fetching.store(false, Ordering::Relaxed);
        let joined = job.source(item).expect("resolved again");
        assert!(joined.info().container.ends_with("(spanned)"), "the job cached the shortened fallback: {}", joined.info().container);
        let t3 = filmcraft_media::FrameRequest::full(filmcraft_time::FrameRate::FPS_25.tick_of(3));
        let want = pool.open_file(&part_path(54), &*host).expect("joined");
        assert_eq!(joined.video_frame(t3).expect("frame").to_rgba8(), want.video_frame(t3).expect("frame").to_rgba8(), "the job renders the whole recording");
    }

    #[test]
    fn a_job_opens_the_whole_recording_not_just_its_part() {
        let pool = crate::media_pool::MediaPool::default();
        let host = CardReaders(card());
        let info = pool.open_file(&part_path(54), &host).expect("spanned open").info().clone();
        let (p, item) = proj_with(&part_path(54), None, info);
        let monitor = pool.full_res_source(&p, item, &host).expect("joined");
        assert!(monitor.info().container.ends_with("(spanned)"));
        let job = pool.own_source(&p, item, &host).expect("the job gets its own copy");
        assert!(job.info().container.ends_with("(spanned)"), "export must see the whole recording the monitors show");
        let t = filmcraft_time::FrameRate::FPS_25.tick_of(3);
        assert_eq!(
            job.video_frame(filmcraft_media::FrameRequest::full(t)).expect("a frame past the first part").to_rgba8(),
            monitor.video_frame(filmcraft_media::FrameRequest::full(t)).expect("same frame from the monitors").to_rgba8(),
            "the job and the monitors disagree about what is at this time"
        );
    }

    /// A Card that also offers streaming readers, the way the desktop host does.
    struct CardReaders(Card);

    impl Services for CardReaders {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.0.read_file(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            self.0.write_file(path, data)
        }
        fn file_size(&self, path: &str) -> std::io::Result<u64> {
            self.0.file_size(path)
        }
        fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
            self.0.list_dir(dir)
        }
        fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            match self.0.read_file(path) {
                Ok(b) => Some(Ok(Arc::new(filmcraft_media::reader::MemReader(b.into())))),
                Err(e) => Some(Err(e)),
            }
        }
    }

    #[test]
    fn any_part_finds_the_whole_recording_in_order() {
        let c = card();
        for seg in [54u32, 55, 56] {
            let parts = spanned_parts(&part_path(seg), &c).expect("spanned");
            assert_eq!(parts, [part_path(54), part_path(55), part_path(56)], "ordered by segment, from any part");
        }
    }

    /// A recording renamed (or re-wrapped) as `.mp4` keeps the camera's name and
    /// trailer, so its parts still join and it still finds its LRV proxy chain -
    /// and the naming of a sibling `.insv` copy of part 54 cannot fuse into it.
    #[test]
    fn a_renamed_recording_still_finds_its_parts_and_proxy() {
        let mp4 = |seg: u32| format!("{DIR}/VID_20261010_205908_00_{seg:03}.mp4");
        let c = Card::new();
        for (i, seg) in [54u32, 55, 56].iter().enumerate() {
            c.add(&mp4(*seg), filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, i % 2 == 1).expect("build"));
            c.add(&lrv_path(*seg), filmcraft_codecs::insta360::synthetic_lrv(Some(&first_part()), 2, i % 2 == 1).expect("build lrv"));
        }
        let parts = spanned_parts(&mp4(55), &c).expect("the .mp4 chain is spanned too");
        assert_eq!(parts, [mp4(54), mp4(55), mp4(56)], "joined by name whatever the extension");
        assert_eq!(camera_proxy(&mp4(54), &c).as_deref(), Some(lrv_path(54).as_str()));
        // An .insv copy of part 54 with its own trailer beside it is a second
        // copy of the recording, not two more chains' worth of parts.
        c.add(&part_path(54), filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, false).expect("build"));
        assert_eq!(spanned_parts(&mp4(55), &c).expect("still spanned"), [mp4(54), mp4(55), mp4(56)], "a different-extension copy does not fuse in");
        assert_eq!(camera_proxy(&mp4(54), &c).as_deref(), Some(lrv_path(54).as_str()), "and does not inflate the main part count");
    }

    /// The camera splits recordings in unbroken segment runs; a chain with a
    /// hole is a card that lost a file, and joining the rest would report a
    /// silently short clip.
    #[test]
    fn a_gapped_segment_run_is_not_spanned() {
        let c = Card::new();
        for seg in [54u32, 56].iter().copied() {
            c.add(&part_path(seg), filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, false).expect("build"));
        }
        assert!(spanned_parts(&part_path(54), &c).is_none(), "a missing middle part is not skipped over");
        // The run also has to start at the part the trailer names.
        let c = Card::new();
        for (i, seg) in [55u32, 56].iter().copied().enumerate() {
            c.add(&part_path(seg), filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, i % 2 == 1).expect("build"));
        }
        assert!(spanned_parts(&part_path(55), &c).is_none(), "a chain whose first part is absent is not a recording yet");
    }

    /// A same-prefixed file whose trailer names a different recording (a copy
    /// renamed into the chain) must not join: it would splice someone else's
    /// frames in and still report one clip.
    #[test]
    fn a_sibling_naming_another_recording_breaks_the_join() {
        let c = card();
        let foreign = "/DCIM/Camera01/VID_20270101_101010_00_000.insv";
        c.add(&part_path(57), filmcraft_codecs::insta360::synthetic_insv(Some(foreign), 2, false).expect("build"));
        assert!(spanned_parts(&part_path(54), &c).is_none(), "one dissenting sibling: the whole run is not trusted");
    }

    #[test]
    fn a_lone_part_is_not_spanned() {
        let c = Card::new();
        c.add(&part_path(54), filmcraft_codecs::insta360::synthetic_insv(Some(&first_part()), 2, false).expect("build"));
        assert!(spanned_parts(&part_path(54), &c).is_none(), "one file on the card: nothing to join");
    }

    #[test]
    fn a_standalone_insv_is_not_spanned() {
        let c = card();
        // a fourth file with no first-part field: its own recording
        c.add(&part_path(57), filmcraft_codecs::insta360::synthetic_insv(None, 2, false).expect("build"));
        assert!(spanned_parts(&part_path(57), &c).is_none());
    }

    /// The camera's LRV proxy chain beside a spanned recording: its parts join among
    /// themselves (their trailers name the recording's first MAIN part, so the match is
    /// by recording, not by name prefix), and every main part finds its LRV twin.
    fn card_with_lrvs() -> Card {
        let c = card();
        for (i, seg) in [54u32, 55, 56].iter().enumerate() {
            c.add(&lrv_path(*seg), filmcraft_codecs::insta360::synthetic_lrv(Some(&first_part()), 2, i % 2 == 1).expect("build lrv"));
        }
        c
    }

    #[test]
    fn lrv_parts_join_their_own_chain() {
        let c = card_with_lrvs();
        let parts = spanned_parts(&lrv_path(55), &c).expect("the LRV chain is spanned too");
        assert_eq!(parts, [lrv_path(54), lrv_path(55), lrv_path(56)], "ordered by segment, LRVs only");
        let src = open_spanned(&lrv_path(55), &c, |p| {
            let b = c.read_file(p).expect("part bytes");
            filmcraft_codecs::insta360::lrv_opener(p.rsplit('/').next().unwrap_or(p), b.into()).expect("claimed")
        })
        .expect("joined");
        assert!(src.info().container.ends_with("(spanned)"), "the container says so: {}", src.info().container);
        assert_eq!(src.info().duration, filmcraft_time::FrameRate::FPS_25.tick_of(6), "3 parts x 2 frames");
    }

    #[test]
    fn every_main_part_finds_its_lrv_twin() {
        let c = card_with_lrvs();
        for seg in [54u32, 55, 56] {
            assert_eq!(camera_proxy(&part_path(seg), &c).as_deref(), Some(lrv_path(seg).as_str()), "main part {seg}");
        }
        // the proxy is its own recording's stand-in, and has no proxy of its own
        assert_eq!(camera_proxy(&lrv_path(55), &c), None, "an LRV is already the proxy");
        // a recording with no LRV chain beside it
        assert_eq!(camera_proxy(&part_path(54), &card()), None, "no proxy written");
        // a lone part (not spanned) still finds its twin
        let c = Card::new();
        c.add(&part_path(42), filmcraft_codecs::insta360::synthetic_insv(None, 2, false).expect("build"));
        c.add(&format!("{DIR}/LRV_20261010_205908_01_042.lrv"), filmcraft_codecs::insta360::synthetic_lrv(None, 2, false).expect("build"));
        assert_eq!(
            camera_proxy(&part_path(42), &c).as_deref(),
            Some(format!("{DIR}/LRV_20261010_205908_01_042.lrv").as_str()),
            "a standalone recording has a twin too"
        );
    }

    #[test]
    fn a_partial_lrv_chain_is_not_a_proxy() {
        // the camera wrote LRVs for only some parts: their duration cannot match, so
        // the import must not attach one
        let c = card();
        c.add(&lrv_path(54), filmcraft_codecs::insta360::synthetic_lrv(Some(&first_part()), 2, false).expect("build"));
        assert_eq!(camera_proxy(&part_path(54), &c), None, "fewer proxy parts than main parts");
    }

    /// Real camera footage with its LRV chain, when the files are on this machine: point
    /// `FILMCRAFT_INSV` at a part of a spanned recording whose `LRV_…_.lrv` twins are in
    /// the same folder. Guards the reverse-engineered assumptions: the naming twin rule,
    /// the trailer's first-part reference across the VID/LRV families, and that the two
    /// chains' durations agree. Without the files the test prints SKIPPED.
    #[test]
    fn real_lrv_chain_is_the_recording_s_proxy() {
        let Some(path) = std::env::var_os("FILMCRAFT_INSV").map(std::path::PathBuf::from) else {
            eprintln!("SKIPPED: set FILMCRAFT_INSV to a part of a spanned .insv recording with LRVs beside it");
            return;
        };
        if !path.is_file() {
            eprintln!("SKIPPED: {path:?} not found");
            return;
        }
        let p = path.to_string_lossy().to_string();
        let Some(twin) = camera_proxy(&p, &crate::FsServices) else {
            eprintln!("SKIPPED: no LRV chain beside {p}");
            return;
        };
        assert!(twin.ends_with(".lrv"), "the twin is an LRV: {twin}");
        let pool = crate::MediaPool::default();
        let main = pool.open_file(&p, &crate::FsServices).expect("main chain opens");
        let proxy = pool.open_file(&twin, &crate::FsServices).expect("proxy chain opens");
        assert!(proxy.info().container.contains("LRV"), "the proxy chain reports LRV: {}", proxy.info().container);
        // both chains report the same camera model (the trailer's field 2 on each)
        let model = |c: &str| c.trim_end_matches(" (spanned)").rsplit_once('(').map(|(_, m)| m.trim_end_matches(')').to_string());
        assert_eq!(model(&proxy.info().container), model(&main.info().container), "the same camera wrote both chains");
        let fd = main.info().frame_rate().frame_duration();
        assert!(
            (proxy.info().duration - main.info().duration).0.abs() <= fd.0,
            "the chains agree within a frame: proxy {:.3}s vs main {:.3}s",
            proxy.info().duration.seconds(),
            main.info().duration.seconds()
        );
        // a frame from each chain: the same sphere, the proxy's at its own (smaller) size
        let t = filmcraft_time::Tick::from_seconds_f64(30.0);
        let (pf, mf) = (
            proxy.video_frame(filmcraft_media::FrameRequest::full(t)).expect("proxy frame"),
            main.video_frame(filmcraft_media::FrameRequest::full(t)).expect("main frame"),
        );
        assert_eq!(pf.width, pf.height * 2, "the proxy frame is 2:1 equirect: {}x{}", pf.width, pf.height);
        assert_eq!(mf.width, mf.height * 2, "the main frame is 2:1 equirect: {}x{}", mf.width, mf.height);
        assert!(pf.width < mf.width, "the proxy is the low-resolution chain: {} < {}", pf.width, mf.width);
    }

    #[test]
    fn joining_sums_the_parts_and_maps_frames() {
        let c = card();
        let src = open_spanned(&part_path(55), &c, |p| {
            let b = c.read_file(p).expect("part bytes");
            filmcraft_codecs::insta360::opener(p.rsplit('/').next().unwrap_or(p), b.into()).expect("claimed")
        })
        .expect("joined");
        let info = src.info();
        let rate = filmcraft_time::FrameRate::FPS_25;
        assert_eq!(info.duration, rate.tick_of(6), "3 parts x 2 frames");
        assert!(info.container.ends_with("(spanned)"), "the container says so: {}", info.container);
        // part 54 (t<0.08s) bright-left, part 55 dark-left, part 56 bright-left again
        let mean_left = |t: i64| -> u32 {
            let f = src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(t))).expect("frame");
            let l = f.luma8();
            let w = f.width as usize;
            (0..f.height as usize).map(|r| u64::from(l[r * w + w / 8])).sum::<u64>() as u32 / f.height
        };
        assert!(mean_left(0) > 200, "part 54 frame 0 bright-left");
        assert!(mean_left(2) < 100, "part 55 frame 0 dark-left");
        assert!(mean_left(4) > 200, "part 56 frame 0 bright-left");
    }

    #[test]
    fn a_missing_part_falls_back_to_the_single_file() {
        let c = card();
        // the middle part is unreadable: the join is refused, the file still opens alone
        let src = open_spanned(&part_path(55), &c, |p| {
            if p == part_path(56).as_str() {
                return Err(MediaError::Io("card error".into()));
            }
            let b = c.read_file(p).expect("part bytes");
            filmcraft_codecs::insta360::opener(p.rsplit('/').next().unwrap_or(p), b.into()).expect("claimed")
        });
        assert!(src.is_none(), "falls back rather than failing the import");
    }

    #[test]
    fn hostile_paths_never_panic() {
        let c = Card::new();
        for p in ["", "/", "a", "/a", "//", "VID_20261010_205908_00_054.insv", "///VID_20261010_205908_00_054.insv", "a\\b\\c.insv", &part_path(54)] {
            let _ = spanned_parts(p, &c);
            let _ = open_spanned(p, &c, |_| Err(MediaError::Io("none".into())));
        }
    }

    /// Real camera footage, when a file is on this machine: point `FILMCRAFT_INSV` at any
    /// part of a spanned recording (an Insta360 X-series `VID_…_<seg>.insv` whose siblings
    /// are in the same folder). Opening any part yields one clip whose duration is the sum
    /// of the parts, and frames from the first and last part decode. Without the files the
    /// test prints SKIPPED.
    #[test]
    fn real_spanned_recording_joins_its_parts() {
        let Some(path) = std::env::var_os("FILMCRAFT_INSV").map(std::path::PathBuf::from) else {
            eprintln!("SKIPPED: set FILMCRAFT_INSV to a part of a spanned .insv recording");
            return;
        };
        if !path.is_file() {
            eprintln!("SKIPPED: {path:?} not found");
            return;
        }
        let p = path.to_string_lossy().to_string();
        let parts = spanned_parts(&p, &crate::FsServices).expect("the parts of a spanned recording are found");
        assert!(parts.len() >= 2, "a spanned recording has at least two parts: {parts:?}");
        assert!(parts.iter().any(|x| x == &p), "the file asked for is one of the parts");

        let pool = crate::MediaPool::default();
        let joined = pool.open_file(&p, &crate::FsServices).expect("open");
        assert!(joined.info().container.ends_with("(spanned)"), "the container says so: {}", joined.info().container);
        // the joined duration is exactly the sum of the parts' own durations
        let sum: filmcraft_time::Tick = parts
            .iter()
            .map(|x| {
                // open the part directly: through the pool it would join again
                let r = filmcraft_media::reader::FileReader::open(std::path::Path::new(x)).expect("part reader");
                filmcraft_media::reader::open_reader(
                    x.rsplit('/').next().unwrap_or(x),
                    std::sync::Arc::new(r),
                    &filmcraft_codecs::reader_openers(),
                    &filmcraft_codecs::openers(),
                )
                .expect("part opens")
                .info()
                .duration
            })
            .fold(filmcraft_time::Tick::ZERO, |a, d| a + d);
        assert_eq!(joined.info().duration, sum, "the clip is as long as its parts together");

        // the first frame and a frame in the final second (the last part) both decode
        let first = joined.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick::ZERO)).expect("first frame");
        assert!(first.width > 0 && first.height > 0);
        let late = filmcraft_time::Tick(joined.info().duration.0 - filmcraft_time::TICKS_PER_SECOND);
        let last = joined.video_frame(filmcraft_media::FrameRequest::full(late)).expect("a frame from the last part");
        assert_eq!((last.width, last.height), (first.width, first.height), "every part shows the same picture size");
    }
}
