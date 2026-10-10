//! Offline media and relinking (File ▸ Link Media…, Make Offline…).
//!
//! - **Identity.** Imports record each file's size and a fast content fingerprint
//!   ([`MediaIdentity`]: a hash of the size, the first MiB and the last MiB). A relink candidate
//!   whose fingerprint differs is refused unless forced, so a different take with the same name
//!   can't silently replace the original.
//! - **Offline detection.** Opening a project checks that every file exists ([`scan`]); missing
//!   items render the offline slate (the media pool) and the frontend shows the Link Media dialog.
//! - **Matching.** [`check_candidate`] compares a candidate with the clip: file name, extension,
//!   fingerprint ("Clip ID"), duration, media start (timecode) and stream metadata.
//! - **Relink others automatically.** Linking one file derives a folder remap (the old path's
//!   prefix → the new one) and applies it to every other missing file, checking each one.
//! - **Search.** [`search`] walks a folder for files named like the missing ones.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use filmcraft_media::MediaInfo;
use filmcraft_project::{ItemId, ItemKind, MediaIdentity, MediaRef, Project};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::{EngineError, Result, Services, Session};

/// Bytes hashed at each end of the file.
pub const FINGERPRINT_SPAN: usize = 1 << 20;

/// 64-bit hash of `size`, `head` and `tail` (a simple multiply–xorshift over 8-byte words).
pub fn fingerprint(size: u64, head: &[u8], tail: &[u8]) -> u64 {
    let mut h: u64 = 0x243f_6a88_85a3_08d3 ^ size.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut mix = |w: u64| {
        h = (h ^ w).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^= h >> 29;
    };
    for part in [head, tail] {
        let (chunks, r) = part.as_chunks::<8>();
        for c in chunks {
            mix(u64::from_le_bytes(*c));
        }
        let mut last = [0u8; 8];
        last[..r.len()].copy_from_slice(r);
        mix(u64::from_le_bytes(last) ^ (r.len() as u64) << 56);
        mix(part.len() as u64);
    }
    h
}

/// Identity of bytes already in memory (import).
pub fn identity_of_bytes(b: &[u8]) -> MediaIdentity {
    let size = b.len() as u64;
    let head = &b[..b.len().min(FINGERPRINT_SPAN)];
    let tail = if b.len() > 2 * FINGERPRINT_SPAN { &b[b.len() - FINGERPRINT_SPAN..] } else { &b[head.len()..] };
    MediaIdentity { size, fingerprint: fingerprint(size, head, tail) }
}

/// Identity of a file, reading at most 2 MiB of it.
pub fn identity_of(services: &dyn Services, path: &str) -> std::io::Result<MediaIdentity> {
    let size = services.file_size(path)?;
    let n = FINGERPRINT_SPAN as u64;
    let head = services.read_range(path, 0, FINGERPRINT_SPAN)?;
    let tail =
        if size > 2 * n { services.read_range(path, size - n, FINGERPRINT_SPAN)? } else { services.read_range(path, head.len() as u64, FINGERPRINT_SPAN)? };
    Ok(MediaIdentity { size, fingerprint: fingerprint(size, &head, &tail) })
}

/// What the Link Media dialog lists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OfflineState {
    /// Missing or offline items found by the last scan (by item id).
    pub missing: Vec<ItemId>,
    /// The frontend should show the Link Media dialog (set when a project opens with missing
    /// media; cleared by relinking everything, Offline All or Cancel).
    pub prompt: bool,
}

/// One missing / offline media item.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
    pub item: u64,
    pub name: String,
    pub file_name: String,
    pub path: String,
    /// `missing` (file not found), `offline` (made offline) or `proxyMissing` (only the proxy).
    pub status: &'static str,
    pub duration: i64,
    pub start_timecode: Option<i64>,
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// Every media item whose file is missing (or that was made offline), in project order.
pub fn scan(p: &Project, services: &dyn Services) -> Vec<Missing> {
    let mut out = Vec::new();
    for it in p.items.values() {
        let ItemKind::Media(m) = &it.kind else { continue };
        let MediaRef::File { path } = &m.media else { continue };
        let status = if m.offline {
            "offline"
        } else if services.file_size(path).is_err() {
            "missing"
        } else if let Some(MediaRef::File { path: pp }) = &m.proxy
            && services.file_size(pp).is_err()
        {
            "proxyMissing"
        } else {
            continue;
        };
        out.push(Missing {
            item: it.id.0,
            name: it.name.clone(),
            file_name: file_name(path).to_string(),
            path: path.clone(),
            status,
            duration: m.info.duration.0,
            start_timecode: m.info.start_timecode,
        });
    }
    out
}

/// Rescan and remember the result. Returns the list.
pub fn refresh(s: &mut Session) -> Vec<Missing> {
    let list = scan(&s.project, &*s.services);
    s.offline.missing = list.iter().filter(|m| m.status != "proxyMissing").map(|m| ItemId(m.item)).collect();
    list
}

/// Which properties must match (Link Media ▸ Match File Properties) and how to relink.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MatchOptions {
    pub file_name: bool,
    pub extension: bool,
    /// The content fingerprint recorded at import ("Clip ID"). Items without one skip this.
    pub clip_id: bool,
    pub duration: bool,
    /// Media start timecode.
    pub media_start: bool,
    /// Frame size, frame rate and audio channel count.
    pub metadata: bool,
    /// Keep clips on the same timecode when the new file's start timecode differs.
    pub align_timecode: bool,
    /// After one file is found, apply its folder remap to the other missing files.
    pub relink_others: bool,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { file_name: true, extension: true, clip_id: true, duration: true, media_start: false, metadata: true, align_timecode: false, relink_others: true }
    }
}

impl MatchOptions {
    pub fn from_params(p: &Value) -> Self {
        let mut o: MatchOptions = p.get("match").and_then(|m| serde_json::from_value(m.clone()).ok()).unwrap_or_default();
        if let Some(b) = bool_p(p, "relinkOthers") {
            o.relink_others = b;
        }
        if let Some(b) = bool_p(p, "alignTimecode") {
            o.align_timecode = b;
        }
        o
    }
}

/// The result of checking one candidate file against a clip.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub path: String,
    /// Passes every enabled check.
    pub ok: bool,
    /// The fingerprint matched the one recorded at import (None = nothing recorded / not read).
    pub identity_match: Option<bool>,
    pub problems: Vec<String>,
    /// Ranking: identity match first, then name, extension, duration.
    pub score: u32,
    #[serde(skip)]
    pub info: Option<MediaInfo>,
    #[serde(skip)]
    pub identity: Option<MediaIdentity>,
    #[serde(skip)]
    pub source: Option<filmcraft_media::SharedSource>,
}

/// Check `path` as the new file of media item `item`. Opening the file (needed for duration,
/// timecode and metadata) is skipped when a cheap check already failed.
pub fn check_candidate(s: &Session, item: ItemId, path: &str, o: &MatchOptions) -> Result<Candidate> {
    let it = s.project.item(item).ok_or_else(|| bad("media.relink", format!("no item {}", item.0)))?;
    let m = it.as_media().ok_or_else(|| bad("media.relink", format!("item {} is not media", item.0)))?;
    let old = match &m.media {
        MediaRef::File { path } => path.clone(),
        MediaRef::Generator(_) => return Err(bad("media.relink", "generated media has no file to link")),
    };
    let mut c = Candidate { path: path.to_string(), ..Default::default() };
    let (on, oe) = split_ext(file_name(&old));
    let (nn, ne) = split_ext(file_name(path));
    let name_ok = on.eq_ignore_ascii_case(nn);
    let ext_ok = oe.eq_ignore_ascii_case(ne);
    if o.file_name && !name_ok {
        c.problems.push(format!("file name differs ({nn} vs {on})"));
    }
    if o.extension && !ext_ok {
        c.problems.push(format!("extension differs (.{ne} vs .{oe})"));
    }
    match identity_of(&*s.services, path) {
        Ok(id) => {
            c.identity = Some(id);
            if let Some(want) = m.identity {
                let same = want == id;
                c.identity_match = Some(same);
                if o.clip_id && !same {
                    c.problems.push(if want.size != id.size {
                        format!("not the same file: {} bytes, the original had {}", id.size, want.size)
                    } else {
                        "not the same file: the content fingerprint differs".to_string()
                    });
                }
            }
        }
        Err(e) => {
            c.problems.push(format!("can't read {path}: {e}"));
            return Ok(c);
        }
    }
    if c.problems.is_empty() {
        match s.media.open_file(path, &*s.services) {
            Ok(src) => {
                let info = src.info().clone();
                let fd = m.info.frame_rate().frame_duration();
                if o.duration && m.info.duration > Tick::ZERO && (info.duration - m.info.duration).0.abs() > fd.0 {
                    c.problems.push(format!("duration differs ({:.3}s vs {:.3}s)", info.duration.seconds(), m.info.duration.seconds()));
                }
                if o.media_start && info.start_timecode != m.info.start_timecode {
                    c.problems.push(format!("media start differs ({:?} vs {:?})", info.start_timecode, m.info.start_timecode));
                }
                if o.metadata {
                    if let (Some(a), Some(b)) = (&info.video, &m.info.video)
                        && ((a.width, a.height) != (b.width, b.height) || a.frame_rate != b.frame_rate)
                    {
                        c.problems.push(format!(
                            "video differs ({}×{} {} vs {}×{} {})",
                            a.width,
                            a.height,
                            a.frame_rate.label(),
                            b.width,
                            b.height,
                            b.frame_rate.label()
                        ));
                    }
                    if info.video.is_some() != m.info.video.is_some() {
                        c.problems.push("video stream presence differs".into());
                    }
                    if let (Some(a), Some(b)) = (info.audio(), m.info.audio())
                        && a.channels != b.channels
                    {
                        c.problems.push(format!("audio channels differ ({} vs {})", a.channels, b.channels));
                    }
                }
                c.info = Some(info);
                c.source = Some(src);
            }
            Err(e) => c.problems.push(format!("can't open {path}: {e}")),
        }
    }
    c.ok = c.problems.is_empty();
    c.score = u32::from(c.identity_match == Some(true)) * 8 + u32::from(name_ok) * 4 + u32::from(ext_ok) * 2 + u32::from(c.ok);
    Ok(c)
}

/// The folder remap implied by a file moving from `old` to `new`: the longest common trailing
/// path components are dropped, the rest is (old prefix, new prefix). Separators are normalised
/// to `/` for matching, so a Windows path can be remapped to a macOS one.
pub fn derive_remap(old: &str, new: &str) -> Option<(String, String)> {
    let norm = |p: &str| p.replace('\\', "/");
    let (o, n) = (norm(old), norm(new));
    let oc: Vec<&str> = o.split('/').collect();
    let nc: Vec<&str> = n.split('/').collect();
    let mut k = 0;
    while k < oc.len().min(nc.len()) && oc[oc.len() - 1 - k] == nc[nc.len() - 1 - k] {
        k += 1;
    }
    if k == 0 {
        return None;
    }
    let op = oc[..oc.len() - k].join("/");
    let np = nc[..nc.len() - k].join("/");
    (op != np).then_some((op, np))
}

/// Apply a remap to a path (None when it doesn't start with the old prefix).
pub fn apply_remap(path: &str, from: &str, to: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let from = from.replace('\\', "/");
    let to = to.replace('\\', "/");
    let rest = p.strip_prefix(from.trim_end_matches('/'))?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    Some(format!("{}{rest}", to.trim_end_matches('/')))
}

/// One planned relink: item → (new path, checked candidate).
struct Plan {
    item: ItemId,
    cand: Candidate,
}

/// Apply planned relinks as one undo step.
fn apply(s: &mut Session, plans: Vec<Plan>, align_timecode: bool, label: &str) -> Result<Vec<Value>> {
    let rows: Vec<Value> = plans.iter().map(|p| json!({"item": p.item.0, "path": p.cand.path})).collect();
    let sources: Vec<(ItemId, String, filmcraft_media::SharedSource)> =
        plans.iter().filter_map(|p| p.cand.source.clone().map(|src| (p.item, p.cand.path.clone(), src))).collect();
    s.edit(label, |proj, _| {
        for p in &plans {
            let Some(m) = proj.item_mut(p.item).and_then(|i| i.as_media_mut()) else { continue };
            let old_tc = m.info.start_timecode;
            let rate = m.info.frame_rate();
            m.media = MediaRef::File { path: p.cand.path.clone() };
            m.offline = false;
            if p.cand.identity.is_some() {
                m.identity = p.cand.identity;
            }
            let mut shift = Tick::ZERO;
            if let Some(info) = &p.cand.info {
                let new_tc = info.start_timecode;
                if align_timecode && let (Some(a), Some(b)) = (old_tc, new_tc) {
                    // what was at old timecode a + t sits at new media time t + (a - b)
                    shift = rate.tick_of(b - a);
                }
                m.info = info.clone();
            }
            proj.shift_media_time(p.item, shift);
            if p.cand.info.is_some() {
                // The item's size is now the file's own. Clips that came from an interchange import
                // while the file was missing still have an "auto" (NaN) anchor waiting for it:
                // centre it in the real picture. Points that are numbers are not touched.
                proj.resolve_placed_auto_points(|_| true, |source| source.id == p.item);
            }
        }
        Ok(())
    })?;
    for (item, path, src) in sources {
        s.media.insert_file(item, &path, src);
    }
    let gone: Vec<ItemId> = plans.iter().map(|p| p.item).collect();
    s.offline.missing.retain(|i| !gone.contains(i));
    if s.offline.missing.is_empty() {
        s.offline.prompt = false;
    }
    Ok(rows)
}

/// Other missing items that the remap (or the new file's folder) finds, checked.
fn others(s: &Session, skip: ItemId, old: &str, new: &str, o: &MatchOptions) -> Vec<Plan> {
    let remap = derive_remap(old, new);
    let new_dir = Path::new(new).parent().map(Path::to_path_buf);
    let mut out = Vec::new();
    for m in scan(&s.project, &*s.services) {
        let id = ItemId(m.item);
        if id == skip || m.status != "missing" {
            continue;
        }
        let mut tries = Vec::new();
        if let Some((f, t)) = &remap
            && let Some(p) = apply_remap(&m.path, f, t)
        {
            tries.push(p);
        }
        if let Some(d) = &new_dir {
            tries.push(d.join(&m.file_name).to_string_lossy().into_owned());
        }
        for p in tries {
            if s.services.file_size(&p).is_err() {
                continue;
            }
            if let Ok(c) = check_candidate(s, id, &p, o)
                && c.ok
            {
                out.push(Plan { item: id, cand: c });
                break;
            }
        }
    }
    out
}

/// `media.relink`: link `item` to `path` (checked unless `force`), then the others.
pub fn relink(s: &mut Session, p: &Value) -> Result<Value> {
    let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("media.relink", "need `item`"))?;
    let path = str_p(p, "path").ok_or_else(|| bad("media.relink", "need `path`"))?.to_string();
    let force = bool_p(p, "force").unwrap_or(false);
    let o = MatchOptions::from_params(p);
    let old = match s.project.item(item).and_then(|i| i.as_media()).map(|m| m.media.clone()) {
        Some(MediaRef::File { path }) => path,
        _ => return Err(bad("media.relink", format!("item {} is not file media", item.0))),
    };
    let mut c = check_candidate(s, item, &path, &o)?;
    if !c.ok && !force {
        return Err(EngineError::Other(format!("{} doesn't match {}: {}", file_name(&path), file_name(&old), c.problems.join("; "))));
    }
    if c.source.is_none() {
        // Forced past a failed check: the file was never opened for its properties, so do it now
        // and let the item take the new file's own info instead of keeping the old file's.
        let Ok(src) = s.media.open_file(&path, &*s.services) else {
            return Err(EngineError::Other(format!("{path} can't be opened")));
        };
        c.info = Some(src.info().clone());
        c.source = Some(src);
    }
    let mut plans = vec![Plan { item, cand: c }];
    if o.relink_others {
        plans.extend(others(s, item, &old, &path, &o));
    }
    let rows = apply(s, plans, o.align_timecode, "Link Media")?;
    Ok(json!({"relinked": rows, "remaining": s.offline.missing.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// `media.autoRelink`: `{from, to}` remaps a folder prefix; `{folder}` searches a folder tree.
pub fn auto_relink(s: &mut Session, p: &Value) -> Result<Value> {
    let o = MatchOptions::from_params(p);
    let missing: Vec<Missing> = scan(&s.project, &*s.services).into_iter().filter(|m| m.status == "missing").collect();
    let mut plans = Vec::new();
    let mut failed = Vec::new();
    if let (Some(from), Some(to)) = (str_p(p, "from"), str_p(p, "to")) {
        for m in &missing {
            let Some(np) = apply_remap(&m.path, from, to) else { continue };
            match check_candidate(s, ItemId(m.item), &np, &o) {
                Ok(c) if c.ok => plans.push(Plan { item: ItemId(m.item), cand: c }),
                Ok(c) => failed.push(json!({"item": m.item, "path": np, "problems": c.problems})),
                Err(e) => failed.push(json!({"item": m.item, "path": np, "problems": [e.to_string()]})),
            }
        }
    } else if let Some(folder) = str_p(p, "folder") {
        let index = index_folder(Path::new(folder), 20_000);
        for m in &missing {
            let key = m.file_name.to_ascii_lowercase();
            let mut best: Option<Candidate> = None;
            for cand in index.get(&key).into_iter().flatten() {
                if let Ok(c) = check_candidate(s, ItemId(m.item), &cand.to_string_lossy(), &o)
                    && c.ok
                    && best.as_ref().is_none_or(|b| c.score > b.score)
                {
                    best = Some(c);
                }
            }
            match best {
                Some(c) => plans.push(Plan { item: ItemId(m.item), cand: c }),
                None => failed.push(json!({"item": m.item, "path": m.path, "problems": ["no matching file found"]})),
            }
        }
    } else {
        return Err(bad("media.autoRelink", "need `from` and `to`, or `folder`"));
    }
    let rows = if plans.is_empty() { Vec::new() } else { apply(s, plans, o.align_timecode, "Link Media")? };
    refresh(s);
    Ok(json!({"relinked": rows, "failed": failed, "remaining": s.offline.missing.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Files under `root` by lower-case file name (breadth-first, at most `limit` files).
pub fn index_folder(root: &Path, limit: usize) -> BTreeMap<String, Vec<PathBuf>> {
    let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut n = 0;
    while let Some(d) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                queue.push_back(p);
            } else {
                out.entry(name.to_ascii_lowercase()).or_default().push(p);
                n += 1;
                if n >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// `media.search {folder, item?, exactName?}`: candidate files for one missing item (or all).
pub fn search(s: &Session, p: &Value) -> Result<Value> {
    let folder = str_p(p, "folder").ok_or_else(|| bad("media.search", "need `folder`"))?;
    let exact = bool_p(p, "exactName").unwrap_or(true);
    let o = MatchOptions::from_params(p);
    let items: Vec<ItemId> = match u64_p(p, "item") {
        Some(i) => vec![ItemId(i)],
        None => scan(&s.project, &*s.services).into_iter().filter(|m| m.status == "missing").map(|m| ItemId(m.item)).collect(),
    };
    let index = index_folder(Path::new(folder), 20_000);
    let mut out = Vec::new();
    for id in items {
        let Some(MediaRef::File { path }) = s.project.item(id).and_then(|i| i.as_media()).map(|m| m.media.clone()) else { continue };
        let fname = file_name(&path).to_ascii_lowercase();
        let stem = split_ext(&fname).0.to_string();
        let mut cands: Vec<Candidate> = Vec::new();
        for (name, paths) in &index {
            let hit = if exact { *name == fname } else { split_ext(name).0 == stem || name.contains(&stem) };
            if !hit {
                continue;
            }
            for f in paths {
                if let Ok(c) = check_candidate(s, id, &f.to_string_lossy(), &o) {
                    cands.push(c);
                }
            }
        }
        cands.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
        out.push(json!({"item": id.0, "candidates": cands}));
    }
    Ok(json!({"results": out}))
}

/// `media.makeOffline {items?, deleteFiles?}`.
pub fn make_offline(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(ItemId).collect(),
        None => u64_p(p, "item").map(|i| vec![ItemId(i)]).unwrap_or_else(|| s.state.project_selection.clone()),
    };
    let files: Vec<(ItemId, String)> = items
        .iter()
        .filter_map(|i| match s.project.item(*i).and_then(|it| it.as_media()).map(|m| &m.media) {
            Some(MediaRef::File { path }) => Some((*i, path.clone())),
            _ => None,
        })
        .collect();
    if files.is_empty() {
        return Err(bad("media.makeOffline", "select file-based media items"));
    }
    let delete = bool_p(p, "deleteFiles").unwrap_or(false);
    s.edit("Make Offline", |proj, _| {
        for (i, _) in &files {
            if let Some(m) = proj.item_mut(*i).and_then(|it| it.as_media_mut()) {
                m.offline = true;
            }
        }
        Ok(())
    })?;
    let mut deleted = Vec::new();
    if delete {
        for (_, path) in &files {
            if std::fs::remove_file(path).is_ok() {
                deleted.push(path.clone());
            }
        }
    }
    refresh(s);
    Ok(json!({"offline": files.iter().map(|f| f.0.0).collect::<Vec<_>>(), "deleted": deleted}))
}

fn status_json(s: &Session, id: ItemId) -> Option<Value> {
    let it = s.project.item(id)?;
    let m = it.as_media()?;
    let path = match &m.media {
        MediaRef::File { path } => Some(path.clone()),
        MediaRef::Generator(_) => None,
    };
    let status = match &path {
        None => "generated",
        Some(_) if m.offline => "offline",
        Some(p) if s.services.file_size(p).is_err() => "missing",
        Some(_) => match s.media.offline_status(id) {
            Some(o) if o.reason == filmcraft_render::offline::OfflineReason::Unreadable => "unreadable",
            _ => "online",
        },
    };
    let proxy = match &m.proxy {
        Some(MediaRef::File { path }) => json!({"path": path, "online": s.services.file_size(path).is_ok()}),
        _ => Value::Null,
    };
    Some(json!({
        "item": id.0,
        "name": it.name,
        "path": path,
        "status": status,
        "identity": m.identity,
        "proxy": proxy,
    }))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "media.findMissing",
            label: "Find Missing Media",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, _| {
                let list = refresh(s);
                Ok(json!({"missing": list}))
            },
            journal: false,
        },
        CommandSpec {
            id: "media.status",
            label: "Media Status",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id?}"#,
            enabled: always,
            run: |s, p| {
                let ids: Vec<ItemId> = match u64_p(p, "item") {
                    Some(i) => vec![ItemId(i)],
                    None => s.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.id).collect(),
                };
                Ok(Value::Array(ids.into_iter().filter_map(|i| status_json(s, i)).collect()))
            },
            journal: false,
        },
        CommandSpec {
            id: "media.linkMedia",
            label: "Link Media…",
            menu: &["File"],
            shortcut: None,
            params: r#"{}  (opens the Link Media dialog; agents use media.relink / media.autoRelink)"#,
            enabled: always,
            run: |s, _| {
                let list = refresh(s);
                s.offline.prompt = !list.is_empty();
                Ok(json!({"missing": list}))
            },
            journal: false,
        },
        CommandSpec {
            id: "media.relink",
            label: "Relink Media",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"path":str,"force":bool=false,"relinkOthers":bool=true,"alignTimecode":bool=false,"match":{"fileName":bool,"extension":bool,"clipId":bool,"duration":bool,"mediaStart":bool,"metadata":bool}?}"#,
            enabled: always,
            run: relink,
            journal: true,
        },
        CommandSpec {
            id: "media.refreshChanged",
            label: "Refresh Changed Media",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: crate::media_watch::refresh_changed,
            journal: true,
        },
        CommandSpec {
            id: "media.replaceFootage",
            label: "Replace Footage…",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"path":str}"#,
            enabled: always,
            run: |s, p| {
                let mut q = p.clone();
                q["force"] = json!(true);
                q["relinkOthers"] = json!(false);
                relink(s, &q)
            },
            journal: true,
        },
        CommandSpec {
            id: "media.autoRelink",
            label: "Relink Moved Media",
            menu: &[],
            shortcut: None,
            params: r#"{"from":str,"to":str}|{"folder":str} + match options"#,
            enabled: always,
            run: auto_relink,
            journal: true,
        },
        CommandSpec {
            id: "media.search",
            label: "Search for Media",
            menu: &[],
            shortcut: None,
            params: r#"{"folder":str,"item":id?,"exactName":bool=true}"#,
            enabled: always,
            run: |s, p| search(s, p),
            journal: false,
        },
        CommandSpec {
            id: "media.makeOffline",
            label: "Make Offline…",
            menu: &["File"],
            shortcut: None,
            params: r#"{"items":[id]?,"deleteFiles":bool=false}"#,
            enabled: |s| {
                let any = s
                    .state
                    .project_selection
                    .iter()
                    .any(|i| s.project.item(*i).and_then(|x| x.as_media()).is_some_and(|m| matches!(m.media, MediaRef::File { .. })));
                if any { Ok(()) } else { Err("select file-based media in the Project panel".into()) }
            },
            run: make_offline,
            journal: true,
        },
        CommandSpec {
            id: "media.offlineAll",
            label: "Offline All",
            menu: &[],
            shortcut: None,
            params: "{}  (leave every missing file offline and close the Link Media dialog)",
            enabled: always,
            run: |s, _| {
                s.offline.prompt = false;
                Ok(json!({"offline": s.offline.missing.iter().map(|i| i.0).collect::<Vec<_>>()}))
            },
            journal: false,
        },
    ]
}

/// After a project opens: find missing files and ask for them.
pub fn on_open(s: &mut Session) -> usize {
    let list = refresh(s);
    s.offline.prompt = !s.offline.missing.is_empty();
    if !s.offline.missing.is_empty() {
        s.toast(format!("{} media file(s) are missing; File ▸ Link Media… reconnects them", s.offline.missing.len()));
    }
    list.len()
}
