//! DaVinci Resolve projects, read straight from a Resolve project library: each project is a
//! folder holding a `Project.db` SQLite database (`…/Resolve Project Library/Resolve Projects/
//! Users/<user>/Projects/<project>/Project.db`).
//!
//! Resolve does not document this database. What is read here was worked out from projects a
//! user made (field meanings checked against what Resolve shows), so it is deliberately
//! conservative and every assumption is listed:
//!
//! - **Media Pool:** folders (`Sm2MpFolder`, the root "Master" folder is the project's root bin)
//!   become bins; video and audio clips (`Sm2MpMedia`) become media items. A clip's file is in
//!   its `BtVideoInfo` / `BtAudioInfo` record: a zstd-compressed protobuf whose fields 1 and 2 are
//!   the folder and the file name. Frame size, rate, frame count, sample rate and channels come
//!   from the Qt-serialised maps next to it, so offline media still has the right shape.
//! - **Timelines** (`Sm2Timeline` → `Sm2Sequence`) become sequences, in the order of the project's
//!   timeline list; compound clips (`Sm2MpCompoundClip`) become sequences too. The frame rate is
//!   the little-endian double in `Sm2Sequence.FrameRate`, the frame size the two big-endian
//!   integers in `Resolution`.
//! - **Time:** clip `Start`, `Duration` and `In` are frame counts (as text; a transition's start
//!   may carry a fractional frame after `|`). Positions are absolute timeline frames: the
//!   timeline's origin is the first double of `Sm2Sequence.MediaExtents` (seconds; the start of
//!   the timeline as a source, e.g. 3600 for 01:00:00:00), which becomes the sequence's start
//!   timecode. `In` counts frames of the clip's source at its own rate (`MediaFrameRate`), from
//!   the source's origin for a nested timeline.
//! - **Scaling:** Resolve fits a clip whose picture is another size into the timeline frame
//!   (the default "Scale entire image to fit"), so every video clip gets Scale to Frame Size; a
//!   720p clip fills a 1080p or UHD sequence as it does in Resolve.
//! - **Tracks** come in the order of `Sm2SequenceContainer_Sm2TiTrack` (`VideoTrackVec`,
//!   `AudioTrackVec`), with their user-defined names. Video and audio clips of the same source at
//!   the same place are linked. Transitions (`Sm2TiTransition`) attach to the clips they touch.
//! - **Not imported** (each reported): generators and titles, Fusion clips, retimed clips (speed
//!   is left at 100 %), effects, grades, transforms, audio levels and markers. Subtitle tracks are
//!   skipped.
//!
//! Like every importer here it does no file I/O: the caller passes the database bytes.

use std::collections::{HashMap, HashSet};
use std::io::Read;

use filmcraft_media::MediaKind;
use filmcraft_project::{BinId, ClipId, ItemId, Project, Sequence, SequenceSettings, TrackKind, Transition, TransitionAlign, TransitionId, find_effect};
use filmcraft_sqlite::{Database, Row, Table, Value};
use filmcraft_time::{FrameRate, Tick};

use crate::common::{Builder, MediaSpec, empty_sequence, file_name, kind_from_ext, rate_from_f64, settings_for, transition_effect};
use crate::{Error, ImportOptions, Imported, Report, Result};

/// The name used in errors and reports.
pub const FORMAT_NAME: &str = "DaVinci Resolve";

/// Decompressed clip records are a few hundred bytes; anything larger is damage.
const MAX_RECORD: u64 = 1 << 20;
/// Media Pool folders nest a handful deep; a deeper chain is a cycle.
const MAX_FOLDER_DEPTH: usize = 64;
/// Frame counts beyond a century at 1000 fps are damage.
const MAX_FRAMES: i64 = 100 * 365 * 24 * 3600 * 1000;

/// A parse error of this format.
fn err(message: impl Into<String>) -> Error {
    Error::Parse { format: FORMAT_NAME, message: message.into() }
}

/// Whether `bytes` are a Resolve project database (`Project.db`).
pub fn sniff(bytes: &[u8]) -> bool {
    filmcraft_sqlite::sniff(bytes) && Database::open(bytes).is_ok_and(|db| is_resolve(&db))
}

/// Whether the database has Resolve's timeline tables.
fn is_resolve(db: &Database) -> bool {
    ["Sm2Timeline", "Sm2Sequence", "Sm2TiTrack", "Sm2TiItem"].iter().all(|t| db.has_table(t))
}

/// The project's name as Resolve shows it, if the database is a Resolve project.
pub fn project_name(bytes: &[u8]) -> Option<String> {
    let db = Database::open(bytes).ok()?;
    if !is_resolve(&db) {
        return None;
    }
    let t = Tbl::load(&db, "SM_Project").ok()?;
    t.rows.first().and_then(|r| t.text(r, "ProjectName")).map(str::to_string)
}

/// Import a project database.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Report)> {
    let db = Database::open(bytes).map_err(|e| err(e.to_string()))?;
    if !is_resolve(&db) {
        return Err(err("not a DaVinci Resolve project database (it has no timeline tables)"));
    }
    let r = Tables::load(&db)?;
    let name = r
        .project
        .rows
        .first()
        .and_then(|row| r.project.text(row, "ProjectName"))
        .map(str::to_string)
        .or_else(|| opts.name.clone())
        .unwrap_or_else(|| "DaVinci Resolve Project".into());
    let mut report = Report::default();
    let mut imp = Imp {
        b: Builder::new(&name),
        r: &r,
        report: &mut report,
        bins: HashMap::new(),
        pool: HashMap::new(),
        sequences: HashMap::new(),
        rates: HashMap::new(),
        default_size: (1920, 1080),
    };
    imp.run();
    break_nesting_cycles(&mut imp.b.p, imp.report);
    if imp.b.top.is_empty() {
        return Err(Error::Empty);
    }
    Ok((imp.b.finish(), report))
}

// ---------------------------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------------------------

/// A table's rows (none when the table is missing: older or newer schemas lack some).
struct Tbl {
    table: Option<Table>,
    rows: Vec<Row>,
}

static NULL: Value = Value::Null;

impl Tbl {
    /// Read table `name` (no rows when the database has no such table).
    fn load(db: &Database, name: &str) -> Result<Tbl> {
        match db.read_table(name) {
            Ok((table, rows)) => Ok(Tbl { table: Some(table), rows }),
            Err(filmcraft_sqlite::Error::NoTable(_)) => Ok(Tbl { table: None, rows: Vec::new() }),
            Err(e) => Err(err(format!("table {name}: {e}"))),
        }
    }
    /// The value of column `col` in `row` (NULL when the table has no such column).
    fn get<'r>(&self, row: &'r Row, col: &str) -> &'r Value {
        match self.table.as_ref().and_then(|t| t.column(col)) {
            Some(i) => row.get(i),
            None => &NULL,
        }
    }
    /// A non-empty text value.
    fn text<'r>(&self, row: &'r Row, col: &str) -> Option<&'r str> {
        self.get(row, col).as_str().map(str::trim).filter(|s| !s.is_empty())
    }
    /// A non-empty BLOB value.
    fn blob<'r>(&self, row: &'r Row, col: &str) -> Option<&'r [u8]> {
        match self.get(row, col) {
            Value::Blob(b) if !b.is_empty() => Some(b),
            _ => None,
        }
    }
    /// Rows by the value of their id column.
    fn index(&self, col: &str) -> HashMap<String, usize> {
        self.rows.iter().enumerate().filter_map(|(i, r)| self.text(r, col).map(|id| (id.to_string(), i))).collect()
    }
    /// For a many-to-many link table (`DbOwner`, `DbAssociate`, `DbPropertyName`, `DbIndex`):
    /// owner → [(property, index, associate)] sorted by property then index.
    fn links(&self) -> HashMap<String, Vec<(String, i64, String)>> {
        let mut m: HashMap<String, Vec<(String, i64, String)>> = HashMap::new();
        for r in &self.rows {
            let (Some(owner), Some(assoc)) = (self.text(r, "DbOwner"), self.text(r, "DbAssociate")) else { continue };
            let prop = self.text(r, "DbPropertyName").unwrap_or_default().to_string();
            let idx = self.get(r, "DbIndex").as_i64().unwrap_or(i64::MAX);
            m.entry(owner.to_string()).or_default().push((prop, idx, assoc.to_string()));
        }
        for v in m.values_mut() {
            v.sort();
        }
        m
    }
}

struct Tables {
    project: Tbl,
    project_timelines: HashMap<String, Vec<(String, i64, String)>>,
    folders: Tbl,
    folder_media: HashMap<String, Vec<(String, i64, String)>>,
    media: Tbl,
    video_info: Tbl,
    audio_info: Tbl,
    timelines: Tbl,
    sequences: Tbl,
    sequence_by_id: HashMap<String, usize>,
    containers: Tbl,
    container_tracks: HashMap<String, Vec<(String, i64, String)>>,
    tracks: Tbl,
    track_by_id: HashMap<String, usize>,
    items: Tbl,
    /// track id → its items (row indices), in track order.
    track_items: HashMap<String, Vec<usize>>,
}

impl Tables {
    /// Read every table the import uses, with each track's items in the track's order.
    fn load(db: &Database) -> Result<Tables> {
        let sequences = Tbl::load(db, "Sm2Sequence")?;
        let tracks = Tbl::load(db, "Sm2TiTrack")?;
        let items = Tbl::load(db, "Sm2TiItem")?;
        let item_links = Tbl::load(db, "Sm2TiItem_Sm2TiTrack")?.links();
        // items in the order the track lists them, then any the list misses, by start
        let item_by_id = items.index("Sm2TiItem_id");
        let mut track_items: HashMap<String, Vec<usize>> = HashMap::new();
        let mut listed = HashSet::new();
        for (track, list) in &item_links {
            for (_, _, item) in list {
                if let Some(&i) = item_by_id.get(item)
                    && listed.insert(i)
                {
                    track_items.entry(track.clone()).or_default().push(i);
                }
            }
        }
        let mut stray: Vec<(String, i64, usize)> = Vec::new();
        for (i, r) in items.rows.iter().enumerate() {
            if listed.contains(&i) {
                continue;
            }
            if let Some(t) = items.text(r, "Sm2TiTrack_id").or_else(|| items.text(r, "Track")) {
                stray.push((t.to_string(), frames(items.get(r, "Start")).unwrap_or(0), i));
            }
        }
        stray.sort();
        for (t, _, i) in stray {
            track_items.entry(t).or_default().push(i);
        }
        Ok(Tables {
            project: Tbl::load(db, "SM_Project")?,
            project_timelines: Tbl::load(db, "SM_Project_Sm2Timeline")?.links(),
            folders: Tbl::load(db, "Sm2MpFolder")?,
            folder_media: Tbl::load(db, "Sm2MpFolder_Sm2MpMedia")?.links(),
            media: Tbl::load(db, "Sm2MpMedia")?,
            video_info: Tbl::load(db, "BtVideoInfo")?,
            audio_info: Tbl::load(db, "BtAudioInfo")?,
            timelines: Tbl::load(db, "Sm2Timeline")?,
            sequence_by_id: sequences.index("Sm2Sequence_id"),
            sequences,
            containers: Tbl::load(db, "Sm2SequenceContainer")?,
            container_tracks: Tbl::load(db, "Sm2SequenceContainer_Sm2TiTrack")?.links(),
            track_by_id: tracks.index("Sm2TiTrack_id"),
            tracks,
            items,
            track_items,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Value decoding
// ---------------------------------------------------------------------------------------------

/// A frame count: an integer, or text such as `86400` or `140909|0080f1155ff1b53f` (whole frames,
/// then a little-endian double fraction in hex, rounded here).
fn frames(v: &Value) -> Option<i64> {
    let f = match v {
        Value::Integer(i) => *i,
        Value::Real(r) if r.is_finite() => r.round() as i64,
        Value::Text(s) => {
            let (whole, frac) = s.split_once('|').unwrap_or((s, ""));
            let mut f: i64 = whole.trim().parse().ok()?;
            if let Some(x) = hex_f64_le(frac)
                && (0.5..1.0).contains(&x)
            {
                f = f.saturating_add(1);
            }
            f
        }
        _ => return None,
    };
    f.checked_abs().is_some_and(|a| a <= MAX_FRAMES).then_some(f)
}

/// `f` frames at `rate` as ticks, or `None` beyond the tick range a project can hold (a damaged
/// count would otherwise saturate in `tick_of` and land the clip at a meaningless time).
fn ticks(rate: FrameRate, f: i64) -> Option<Tick> {
    let t = rate.tick_of(f);
    (Tick::MIN..=Tick::MAX).contains(&t).then_some(t)
}

/// A little-endian double written as 16 hex digits.
fn hex_f64_le(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.len() != 16 || !s.is_ascii() {
        return None;
    }
    let mut b = [0u8; 8];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(f64::from_le_bytes(b))
}

/// A frame-rate blob: a little-endian double (then flags).
fn rate_blob(b: Option<&[u8]>) -> Option<FrameRate> {
    let s = b?.get(..8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    let r = f64::from_le_bytes(a);
    (r.is_finite() && r > 0.5 && r <= 1000.0).then(|| rate_from_f64(r))
}

/// A resolution blob: two big-endian 64-bit integers (width, height).
fn size_blob(b: Option<&[u8]>) -> Option<(u32, u32)> {
    let b = b?;
    let be = |at: usize| -> Option<u64> { Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?)) };
    let (w, h) = (be(0)?, be(8)?);
    let ok = |v: u64| (16..=16384).contains(&v);
    (ok(w) && ok(h)).then_some((w as u32, h as u32))
}

/// Two little-endian doubles: (start, duration) in seconds.
fn extents_blob(b: Option<&[u8]>) -> Option<(f64, f64)> {
    let b = b?;
    let le = |at: usize| -> Option<f64> { Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?)) };
    let (s, d) = (le(0)?, le(8)?);
    ((0.0..1e9).contains(&s) && d.is_finite()).then_some((s, d))
}

/// A value of a Qt-serialised variant map.
#[derive(Clone, Debug, PartialEq)]
enum QVal {
    Int(i64),
    Real(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl QVal {
    /// The value as an integer (a finite real is truncated).
    fn int(&self) -> Option<i64> {
        match self {
            QVal::Int(i) => Some(*i),
            QVal::Real(r) if r.is_finite() => Some(*r as i64),
            _ => None,
        }
    }
}

/// A map serialised as: u32 version, u32 count, then `count` × (string key, variant), where a
/// string is a u32 byte length and UTF-16BE text and a variant is a u32 type, a null flag byte
/// and the value. Unknown variant types end the map (what was read so far is kept).
fn qmap(b: &[u8]) -> HashMap<String, QVal> {
    let mut out = HashMap::new();
    let mut p = 0usize;
    let u32_at = |p: &mut usize| -> Option<u32> {
        let v = u32::from_be_bytes(b.get(*p..*p + 4)?.try_into().ok()?);
        *p += 4;
        Some(v)
    };
    let bytes_at = |p: &mut usize| -> Option<Vec<u8>> {
        let n = u32::from_be_bytes(b.get(*p..*p + 4)?.try_into().ok()?);
        *p += 4;
        if n == u32::MAX {
            return Some(Vec::new());
        }
        let v = b.get(*p..p.checked_add(n as usize)?)?.to_vec();
        *p += n as usize;
        Some(v)
    };
    let text = |v: &[u8]| String::from_utf16_lossy(&v.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>());
    let _version = u32_at(&mut p);
    let count = u32_at(&mut p).unwrap_or(0).min(4096);
    for _ in 0..count {
        let Some(key) = bytes_at(&mut p).map(|k| text(&k)) else { break };
        let Some(ty) = u32_at(&mut p) else { break };
        p += 1; // null flag
        let fixed = |p: &mut usize, n: usize| -> Option<Vec<u8>> {
            let v = b.get(*p..*p + n)?.to_vec();
            *p += n;
            Some(v)
        };
        let value = match ty {
            1 => fixed(&mut p, 1).map(|v| QVal::Int(i64::from(v[0]))),
            2 => fixed(&mut p, 4).and_then(|v| Some(QVal::Int(i64::from(i32::from_be_bytes(v.try_into().ok()?))))),
            3 => fixed(&mut p, 4).and_then(|v| Some(QVal::Int(i64::from(u32::from_be_bytes(v.try_into().ok()?))))),
            4 => fixed(&mut p, 8).and_then(|v| Some(QVal::Int(i64::from_be_bytes(v.try_into().ok()?)))),
            5 => fixed(&mut p, 8).and_then(|v| Some(QVal::Int(i64::try_from(u64::from_be_bytes(v.try_into().ok()?)).unwrap_or(i64::MAX)))),
            6 => fixed(&mut p, 8).and_then(|v| Some(QVal::Real(f64::from_be_bytes(v.try_into().ok()?)))),
            10 => bytes_at(&mut p).map(|v| QVal::Text(text(&v))),
            12 => bytes_at(&mut p).map(QVal::Bytes),
            _ => None,
        };
        let Some(value) = value else { break };
        out.insert(key, value);
    }
    out
}

/// The file behind a `BtVideoInfo.Clip` / `BtAudioInfo.Clip` record: a u32 version and u32
/// length, a flag byte, then a protobuf (zstd-compressed when it starts with the zstd magic)
/// whose field 1 is the folder and field 2 the file name.
fn clip_path(blob: &[u8]) -> Option<String> {
    let body = blob.get(9..)?;
    let data = if body.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) { decompress(body)? } else { body.to_vec() };
    let (mut dir, mut name) = (None, None);
    let mut p = 0usize;
    while p < data.len() {
        let (key, np) = varint(&data, p)?;
        p = np;
        match key & 7 {
            0 => p = varint(&data, p)?.1,
            1 => p = p.checked_add(8)?,
            5 => p = p.checked_add(4)?,
            2 => {
                let (n, np) = varint(&data, p)?;
                let end = np.checked_add(usize::try_from(n).ok()?)?;
                let s = data.get(np..end)?;
                match key >> 3 {
                    1 if dir.is_none() => dir = std::str::from_utf8(s).ok().map(str::to_string),
                    2 if name.is_none() => name = std::str::from_utf8(s).ok().map(str::to_string),
                    _ => {}
                }
                p = end;
            }
            _ => break,
        }
    }
    let name = name.filter(|n| !n.is_empty())?;
    Some(match dir.filter(|d| !d.is_empty()) {
        Some(d) if d.ends_with(['/', '\\']) => format!("{d}{name}"),
        Some(d) => {
            let sep = if d.contains('\\') || d.as_bytes().get(1) == Some(&b':') { '\\' } else { '/' };
            format!("{d}{sep}{name}")
        }
        None => name,
    })
}

/// The protobuf varint at `p`: its value and the position after it.
fn varint(b: &[u8], mut p: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let x = *b.get(p)?;
        p += 1;
        v |= u64::from(x & 0x7f) << shift;
        if x < 0x80 {
            return Some((v, p));
        }
    }
    None
}

/// Decompress a zstd frame, at most [`MAX_RECORD`] bytes (`None` when it is damaged).
fn decompress(frame: &[u8]) -> Option<Vec<u8>> {
    // a third-party decoder: a damaged frame must not take the import down with it
    std::panic::catch_unwind(|| {
        let mut out = Vec::new();
        let decoder = ruzstd::decoding::StreamingDecoder::new(frame).ok()?;
        decoder.take(MAX_RECORD).read_to_end(&mut out).ok()?;
        Some(out)
    })
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

struct Imp<'r, 'p> {
    b: Builder,
    r: &'r Tables,
    report: &'p mut Report,
    /// Media Pool folder id → bin (`None` for the root folder).
    bins: HashMap<String, Option<BinId>>,
    /// Media Pool item id → project item.
    pool: HashMap<String, ItemId>,
    /// `Sm2Sequence` id → sequence item.
    sequences: HashMap<String, ItemId>,
    /// Frame rate of each imported sequence (the source rate of nested clips).
    rates: HashMap<ItemId, FrameRate>,
    default_size: (u32, u32),
}

/// One transition found on a track, placed once the clips are in.
struct PendingTransition {
    kind: TrackKind,
    track: usize,
    start: Tick,
    end: Tick,
    name: String,
}

impl Imp<'_, '_> {
    /// Import everything: bins, a sequence for every timeline and compound clip, the Media Pool
    /// clips, then each sequence's tracks.
    fn run(&mut self) {
        self.default_size = self.common_size();
        self.folders();
        self.reserve_sequences();
        self.pool_media();
        let order: Vec<(String, ItemId)> = self.sequences.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let mut order = order;
        order.sort_by_key(|(_, id)| *id);
        for (seq, id) in order {
            self.sequence(&seq, id);
        }
    }

    /// The frame size most timelines use (for timelines that store none).
    fn common_size(&self) -> (u32, u32) {
        let t = &self.r.sequences;
        let mut counts: HashMap<(u32, u32), usize> = HashMap::new();
        for r in &t.rows {
            if let Some(s) = size_blob(t.blob(r, "Resolution")) {
                *counts.entry(s).or_default() += 1;
            }
        }
        counts.into_iter().max_by_key(|(s, n)| (*n, *s)).map(|(s, _)| s).unwrap_or((1920, 1080))
    }

    /// Media Pool folders as bins, each under its parent folder's bin.
    fn folders(&mut self) {
        let t = &self.r.folders;
        let by_id = t.index("Sm2MpFolder_id");
        /// The folder's parent folder.
        fn parent<'a>(t: &Tbl, r: &'a Row) -> Option<&'a str> {
            t.text(r, "MpFolder").or_else(|| t.text(r, "Sm2MpFolder_Owner_id"))
        }
        // in row (Media Pool) order: a HashMap's order changes from run to run
        for row in &t.rows {
            let Some(id) = t.text(row, "Sm2MpFolder_id") else { continue };
            let Some(&i) = by_id.get(id) else { continue };
            let mut chain = vec![(id.to_string(), i)];
            // walk up to a folder that already has a bin (or the root)
            while let Some(&(_, at)) = chain.last() {
                let Some(p) = parent(t, &t.rows[at]) else { break };
                if self.bins.contains_key(p) || chain.len() > MAX_FOLDER_DEPTH || chain.iter().any(|(c, _)| c == p) {
                    break;
                }
                match by_id.get(p) {
                    Some(&pi) => chain.push((p.to_string(), pi)),
                    None => break,
                }
            }
            for (fid, at) in chain.into_iter().rev() {
                if self.bins.contains_key(&fid) {
                    continue;
                }
                let row = &t.rows[at];
                let bin = match parent(t, row) {
                    // the Media Pool's root folder ("Master") is the project's root
                    None => None,
                    Some(p) => {
                        let parent_bin = self.bins.get(p).copied().flatten();
                        Some(self.b.bin(t.text(row, "Name").unwrap_or("Bin"), parent_bin))
                    }
                };
                self.bins.insert(fid, bin);
            }
        }
    }

    /// The bin of a Media Pool item's folder (`None`: the project's root).
    fn bin_of_media(&self, row: &Row) -> Option<BinId> {
        let t = &self.r.media;
        t.text(row, "Sm2MpFolder_id").or_else(|| t.text(row, "MpFolder")).and_then(|f| self.bins.get(f).copied().flatten())
    }

    /// A timeline's sequence settings: its frame rate and size (the most common size when it stores
    /// none).
    fn settings(&self, seq_row: &Row) -> SequenceSettings {
        let t = &self.r.sequences;
        let rate = rate_blob(t.blob(seq_row, "FrameRate")).unwrap_or(FrameRate::FPS_24);
        let (w, h) = size_blob(t.blob(seq_row, "Resolution")).unwrap_or(self.default_size);
        settings_for(rate, w, h, false)
    }

    /// Reserve a sequence item for every timeline (in the project's timeline order) and every
    /// compound clip, so clips can nest them before they are filled.
    fn reserve_sequences(&mut self) {
        let t = &self.r.timelines;
        let order: HashMap<&str, i64> =
            self.r.project_timelines.values().flatten().filter(|(prop, ..)| prop == "TimelineHandleVec").map(|(_, i, tl)| (tl.as_str(), *i)).collect();
        let mut rows: Vec<(i64, usize)> =
            t.rows.iter().enumerate().map(|(i, r)| (t.text(r, "Sm2Timeline_id").and_then(|id| order.get(id).copied()).unwrap_or(i64::MAX), i)).collect();
        rows.sort();
        let media_by_id = self.r.media.index("Sm2MpMedia_id");
        for (_, i) in rows {
            let row = &t.rows[i];
            let Some(seq_id) = t.text(row, "Sequence") else { continue };
            let Some(&si) = self.r.sequence_by_id.get(seq_id) else {
                self.report.warn(format!("timeline \"{}\" has no sequence data; skipped", t.text(row, "Name").unwrap_or("?")));
                continue;
            };
            if self.sequences.contains_key(seq_id) {
                continue;
            }
            let name = t.text(row, "Name").unwrap_or("Timeline").to_string();
            let mp = t.text(row, "Sm2MpMedia_id").or_else(|| t.text(row, "MpTimelineClip"));
            let bin = mp.and_then(|m| media_by_id.get(m)).and_then(|&mi| self.bin_of_media(&self.r.media.rows[mi]));
            let settings = self.settings(&self.r.sequences.rows[si]);
            let rate = settings.frame_rate;
            let id = self.b.reserve_sequence(&name, settings, bin);
            self.rates.insert(id, rate);
            self.sequences.insert(seq_id.to_string(), id);
            if let Some(m) = mp {
                self.pool.insert(m.to_string(), id);
            }
            self.b.top.push(id);
        }
        let m = &self.r.media;
        for row in &m.rows {
            if m.text(row, "DbType") != Some("Sm2MpCompoundClip") {
                continue;
            }
            let (Some(id), Some(seq_id)) = (m.text(row, "Sm2MpMedia_id"), m.text(row, "Sequence")) else { continue };
            let Some(&si) = self.r.sequence_by_id.get(seq_id) else { continue };
            let settings = self.settings(&self.r.sequences.rows[si]);
            let rate = settings.frame_rate;
            let item = match self.sequences.get(seq_id) {
                Some(item) => *item,
                None => {
                    let item = self.b.reserve_sequence(m.text(row, "Name").unwrap_or("Compound Clip"), settings, self.bin_of_media(row));
                    self.sequences.insert(seq_id.to_string(), item);
                    self.rates.insert(item, rate);
                    item
                }
            };
            self.pool.insert(id.to_string(), item);
        }
    }

    /// Media Pool video and audio clips, folder by folder in Media Pool order.
    fn pool_media(&mut self) {
        let m = &self.r.media;
        let mut position: HashMap<&str, (i64, i64)> = HashMap::new();
        let folders = &self.r.folders;
        for (n, f) in folders.rows.iter().enumerate() {
            let Some(list) = folders.text(f, "Sm2MpFolder_id").and_then(|id| self.r.folder_media.get(id)) else { continue };
            for (_, idx, media) in list {
                position.insert(media, (n as i64, *idx));
            }
        }
        let mut rows: Vec<((i64, i64), usize)> = m
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| (m.text(r, "Sm2MpMedia_id").and_then(|id| position.get(id).copied()).unwrap_or((i64::MAX, i as i64)), i))
            .collect();
        rows.sort();
        let videos = self.r.video_info.index("Sm2MpMedia_id");
        let video_ids = self.r.video_info.index("BtVideoInfo_id");
        let audios = self.r.audio_info.index("Sm2MpMedia_id");
        // timeline clips carry their file path too: a fallback for pool clips without a record
        let mut item_paths: HashMap<&str, &str> = HashMap::new();
        for r in &self.r.items.rows {
            if let (Some(mr), Some(p)) = (self.r.items.text(r, "MediaRef"), self.r.items.text(r, "MediaFilePath")) {
                item_paths.entry(mr).or_insert(p);
            }
        }
        for (_, i) in rows {
            let row = &m.rows[i];
            let ty = m.text(row, "DbType").unwrap_or_default();
            let name = m.text(row, "Name").unwrap_or("Clip");
            let Some(id) = m.text(row, "Sm2MpMedia_id") else { continue };
            match ty {
                "Sm2MpVideoClip" | "Sm2MpAudioClip" => {}
                "Sm2MpTimelineClip" | "Sm2MpCompoundClip" => continue,
                other => {
                    self.report.warn(format!("Media Pool item \"{name}\" ({}) is not imported", pretty_type(other)));
                    continue;
                }
            }
            let video = m.text(row, "Video").and_then(|v| video_ids.get(v)).or_else(|| videos.get(id)).map(|&vi| &self.r.video_info.rows[vi]);
            let audio = audios.get(id).map(|&ai| &self.r.audio_info.rows[ai]);
            let path = video
                .and_then(|v| self.r.video_info.blob(v, "Clip"))
                .and_then(clip_path)
                .or_else(|| audio.and_then(|a| self.r.audio_info.blob(a, "Clip")).and_then(clip_path))
                .or_else(|| item_paths.get(id).map(|p| p.to_string()));
            let Some(path) = path else {
                self.report.warn(format!("Media Pool clip \"{name}\" has no file path; skipped"));
                continue;
            };
            let spec = self.spec(&path, ty == "Sm2MpAudioClip", video, audio);
            let bin = self.bin_of_media(row);
            let item = self.b.file_media(&path, name, &path, &spec, bin);
            self.pool.insert(id.to_string(), item);
        }
    }

    /// What the Media Pool records say about a file (so offline media keeps its shape).
    fn spec(&self, path: &str, audio_only: bool, video: Option<&Row>, audio: Option<&Row>) -> MediaSpec {
        let mut spec = MediaSpec::default();
        let kind = kind_from_ext(path);
        spec.kind = Some(if audio_only { MediaKind::AudioOnly } else { kind });
        if let Some(v) = video {
            let vt = &self.r.video_info;
            let time = vt.blob(v, "Time").map(qmap).unwrap_or_default();
            let geometry = vt.blob(v, "Geometry").map(qmap).unwrap_or_default();
            let rate = match time.get("FrameRate") {
                Some(QVal::Bytes(b)) => rate_blob(Some(b)),
                _ => None,
            };
            let size = match geometry.get("Resolution") {
                Some(QVal::Bytes(b)) => size_blob(Some(b)),
                _ => None,
            };
            if let (Some(rate), Some((w, h))) = (rate, size)
                && !audio_only
            {
                spec.video = Some((w, h, rate));
                if let Some(n) = time.get("NumFrames").and_then(QVal::int).filter(|n| *n > 0 && *n <= MAX_FRAMES) {
                    spec.duration = ticks(rate, n);
                }
                if let Some(s) = time.get("StartFrame").and_then(QVal::int).filter(|s| *s > 0 && *s <= MAX_FRAMES) {
                    spec.start_tc = Some(s);
                }
            }
        }
        if let Some(a) = audio {
            let tracks = self.r.audio_info.blob(a, "TracksBA").map(qmap).unwrap_or_default();
            let first = tracks.get("0").and_then(|v| match v {
                QVal::Bytes(b) => Some(qmap(b)),
                _ => None,
            });
            if let Some(t) = first {
                let sr = t.get("SampleRate").and_then(QVal::int).filter(|s| (1..=384_000).contains(s));
                let ch = t.get("NumChannels").and_then(QVal::int).filter(|c| (1..=64).contains(c));
                if let (Some(sr), Some(ch)) = (sr, ch) {
                    spec.audio = Some((sr as u32, ch as u32));
                }
            }
        }
        if spec.kind == Some(MediaKind::Movie) && spec.video.is_none() && spec.audio.is_some() && audio_only {
            spec.kind = Some(MediaKind::AudioOnly);
        }
        spec
    }

    /// The source of a timeline clip: (item, the rate its `In` counts in).
    fn source(&mut self, row: &Row, timeline_rate: FrameRate, kind: TrackKind) -> Option<(ItemId, FrameRate)> {
        let t = &self.r.items;
        let media_rate = rate_blob(t.blob(row, "MediaFrameRate"));
        let reference = t.text(row, "MediaRef");
        if let Some(&item) = reference.and_then(|m| self.pool.get(m)) {
            if let Some(&nested) = self.rates.get(&item) {
                return Some((item, nested));
            }
            return Some((item, media_rate.unwrap_or(timeline_rate)));
        }
        let path = t.text(row, "MediaFilePath")?;
        let mut spec = MediaSpec { kind: Some(kind_from_ext(path)), ..Default::default() };
        if kind == TrackKind::Audio && spec.kind == Some(MediaKind::Still) {
            spec.kind = Some(MediaKind::AudioOnly);
        }
        let item = self.b.file_media(path, file_name(path), path, &spec, None);
        if let Some(m) = reference {
            self.pool.insert(m.to_string(), item);
        }
        Some((item, media_rate.unwrap_or(timeline_rate)))
    }

    /// The tracks of a sequence: (kind, track row), in Resolve's order within each kind.
    /// Also returns how many tracks of other kinds (subtitles) hold clips.
    fn tracks_of(&self, seq_id: &str) -> (Vec<(TrackKind, usize)>, usize) {
        let t = &self.r.tracks;
        let kind_of = |row: &Row| match t.get(row, "Type").as_i64() {
            Some(0) => Some(TrackKind::Video),
            Some(1) => Some(TrackKind::Audio),
            _ => None,
        };
        let containers: Vec<&str> = self
            .r
            .containers
            .rows
            .iter()
            .filter(|r| self.r.containers.text(r, "Sm2Sequence_id") == Some(seq_id))
            .filter_map(|r| self.r.containers.text(r, "Sm2SequenceContainer_id"))
            .collect();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut skipped = 0;
        let has_items = |ti: usize| t.text(&t.rows[ti], "Sm2TiTrack_id").and_then(|id| self.r.track_items.get(id)).is_some_and(|v| !v.is_empty());
        for c in &containers {
            for (prop, _, track) in self.r.container_tracks.get(*c).into_iter().flatten() {
                let Some(&ti) = self.r.track_by_id.get(track) else { continue };
                if !seen.insert(ti) {
                    continue;
                }
                let kind = kind_of(&t.rows[ti]).or(match prop.as_str() {
                    "VideoTrackVec" => Some(TrackKind::Video),
                    "AudioTrackVec" => Some(TrackKind::Audio),
                    _ => None,
                });
                match kind {
                    Some(k) => out.push((k, ti)),
                    None => skipped += usize::from(has_items(ti)),
                }
            }
        }
        // tracks no container lists, by row order
        for (ti, row) in t.rows.iter().enumerate() {
            if seen.contains(&ti) || (t.text(row, "Sequence") != Some(seq_id) && t.text(row, "Sm2Sequence_id") != Some(seq_id)) {
                continue;
            }
            match kind_of(row) {
                Some(k) => out.push((k, ti)),
                None => skipped += usize::from(has_items(ti)),
            }
        }
        // video then audio, each in order (stable: the order found above is the track order)
        out.sort_by_key(|(k, _)| *k == TrackKind::Audio);
        (out, skipped)
    }

    /// Fill sequence `id` from the Resolve sequence `seq_id`: tracks, clips, links and transitions.
    fn sequence(&mut self, seq_id: &str, id: ItemId) {
        let Some(&si) = self.r.sequence_by_id.get(seq_id) else { return };
        let seq_row = &self.r.sequences.rows[si];
        let settings = self.settings(seq_row);
        let rate = settings.frame_rate;
        let seq_name = self.b.p.item(id).map(|i| i.name.clone()).unwrap_or_default();
        let (tracks, skipped) = self.tracks_of(seq_id);
        if skipped > 0 {
            self.report.warn(format!("timeline \"{seq_name}\": {skipped} subtitle track(s) are not imported"));
        }
        let r: &Tables = self.r;
        let items = &r.items;
        let track_rows = |ti: usize| -> &[usize] {
            r.tracks.text(&r.tracks.rows[ti], "Sm2TiTrack_id").and_then(|tid| r.track_items.get(tid)).map(Vec::as_slice).unwrap_or(&[])
        };
        // the timeline's origin: where it starts as a source, never after its first clip
        let first = tracks.iter().flat_map(|(_, ti)| track_rows(*ti)).filter_map(|&i| frames(items.get(&items.rows[i], "Start"))).min();
        let declared = extents_blob(r.sequences.blob(seq_row, "MediaExtents")).map(|(s, _)| (s * rate.as_f64()).round() as i64);
        let origin = match (declared, first) {
            (Some(d), Some(f)) => d.min(f),
            (Some(d), None) => d,
            (None, Some(f)) => f,
            (None, None) => 0,
        }
        .max(0);
        let mut seq: Sequence = empty_sequence(settings);
        seq.start_timecode = origin;
        let mut index = [0usize; 2];
        // each Resolve track's index among the sequence's tracks of its kind (None: over the cap)
        let mut placed: Vec<Option<usize>> = Vec::new();
        for (kind, ti) in &tracks {
            let k = usize::from(*kind == TrackKind::Audio);
            let i = index[k];
            index[k] += 1;
            if !self.b.ensure_tracks(&mut seq, *kind, i + 1) {
                self.report.warn(format!("timeline \"{seq_name}\": more than {} tracks; the rest are skipped", crate::common::MAX_IMPORTED_TRACKS));
                placed.push(None);
                continue;
            }
            if let Some(n) = r.tracks.text(&r.tracks.rows[*ti], "UserDefinedName")
                && let Some(t) = seq.tracks_mut(*kind).get_mut(i)
            {
                t.name = n.to_string();
            }
            placed.push(Some(i));
        }
        let mut transitions = Vec::new();
        // (source item, start, duration, in) → clips, to link picture and sound
        let mut links: HashMap<(ItemId, i64, i64, i64), Vec<ClipId>> = HashMap::new();
        for ((kind, ti), track_index) in tracks.iter().zip(placed.iter()) {
            let Some(track_index) = track_index else { continue };
            for &ii in track_rows(*ti) {
                let row = &items.rows[ii];
                let ty = items.text(row, "DbType").unwrap_or_default();
                let name = items.text(row, "Name").unwrap_or("Clip").to_string();
                let start = frames(items.get(row, "Start"));
                let duration = frames(items.get(row, "Duration")).filter(|d| *d > 0);
                let (Some(start), Some(duration)) = (start, duration) else {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" has no position; skipped"));
                    continue;
                };
                let (Some(at), Some(len)) = (ticks(rate, start.saturating_sub(origin).max(0)), ticks(rate, duration)) else {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" lies outside the time a sequence can hold; skipped"));
                    continue;
                };
                match ty {
                    "Sm2TiVideoClip" | "Sm2TiAudioClip" => {}
                    "Sm2TiTransition" => {
                        transitions.push(PendingTransition { kind: *kind, track: *track_index, start: at, end: at + len, name });
                        continue;
                    }
                    other => {
                        self.report.warn(format!("timeline \"{seq_name}\": {} \"{name}\" at {} is not imported", pretty_type(other), timecode(rate, start)));
                        continue;
                    }
                }
                let Some((item, src_rate)) = self.source(row, rate, *kind) else {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" has no media; skipped"));
                    continue;
                };
                if item == id {
                    self.report.warn(format!("timeline \"{seq_name}\" contains itself; that clip is skipped"));
                    continue;
                }
                // an absent In is the start of the source; one that is there but no frame count
                // is damage, like a bad Start or Duration (not a clip that plays from frame 0)
                let in_value = items.get(row, "In");
                let absent = matches!(in_value, Value::Null) || in_value.as_str().is_some_and(|s| s.trim().is_empty());
                let Some(inn) = (if absent { Some(0) } else { frames(in_value) }) else {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" has an unreadable In point; skipped"));
                    continue;
                };
                let mut inn = inn.max(0);
                // a source with a start timecode may count In from it
                if let Some(mst) = items.get(row, "MediaStartTime").as_f64().filter(|s| s.is_finite() && *s > 0.0 && *s < 1e7) {
                    let tc = (mst * src_rate.as_f64()).round() as i64;
                    if inn >= tc {
                        inn -= tc;
                    }
                }
                let Some(source_in) = ticks(src_rate, inn) else {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" starts outside the time its source can hold; skipped"));
                    continue;
                };
                let mut clip = self.b.clip(item, *kind, &name, at, len, source_in);
                // Resolve fits a picture of another size to the timeline frame (its default
                // "Scale entire image to fit"): Scale to Frame Size does the same
                clip.scale_to_frame = *kind == TrackKind::Video;
                let clip_id = clip.id;
                // (a still has no speed: its curve there animates something else)
                let still = self.b.p.item(item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == MediaKind::Still);
                if !still && items.blob(row, "MediaTimemapBA").is_some_and(|b| b.first() == Some(&0)) {
                    self.report.warn(format!("timeline \"{seq_name}\": the speed change on \"{name}\" is not imported (it plays at 100%)"));
                }
                if items.blob(row, "EffectFiltersBA").is_some() {
                    self.report.warn("clip effects, grades and transforms are not imported");
                }
                if items.blob(row, "MarkersBA").is_some_and(|b| b.len() > 16) {
                    self.report.warn("clip markers are not imported");
                }
                if self.b.place(&mut seq, *kind, *track_index, clip).is_none() {
                    self.report.warn(format!("timeline \"{seq_name}\": clip \"{name}\" could not be placed; skipped"));
                    continue;
                }
                links.entry((item, start, duration, inn)).or_default().push(clip_id);
            }
        }
        // linked picture and sound
        let mut link_of: HashMap<ClipId, u64> = HashMap::new();
        for clips in links.values() {
            let has = |k: TrackKind| clips.iter().any(|c| seq.tracks(k).iter().any(|t| t.items.iter().any(|i| i.id == *c)));
            if clips.len() > 1 && has(TrackKind::Video) && has(TrackKind::Audio) {
                let l = self.b.link_id();
                for c in clips {
                    link_of.insert(*c, l);
                }
            }
        }
        for t in seq.all_tracks_mut() {
            for i in &mut t.items {
                if let Some(l) = link_of.get(&i.id) {
                    i.link = Some(*l);
                }
            }
        }
        for tr in transitions {
            self.transition(&mut seq, &seq_name, rate, origin, tr);
        }
        self.b.put_sequence(id, seq);
    }

    /// Attach a transition to the clips it touches (Resolve stores none of that: clips meet at
    /// the cut and the transition covers it).
    fn transition(&mut self, seq: &mut Sequence, seq_name: &str, rate: FrameRate, origin: i64, tr: PendingTransition) {
        let (s, e) = (tr.start, tr.end);
        let Some(track) = seq.tracks_mut(tr.kind).get_mut(tr.track) else { return };
        let mut from = track.items.iter().filter(|i| i.end() >= s && i.end() <= e).max_by_key(|i| i.end()).map(|i| (i.id, i.end()));
        let mut to = track.items.iter().filter(|i| i.start >= s && i.start <= e).min_by_key(|i| i.start).map(|i| (i.id, i.start));
        if let (Some((_, fe)), Some((_, ts))) = (from, to)
            && fe != ts
        {
            // they don't meet: keep the side the transition starts or ends on
            if ts == s {
                from = None;
            } else {
                to = None;
            }
        }
        if from.is_none() && to.is_none() {
            let at = timecode(rate, rate.frame_at(s) + origin);
            self.report.warn(format!("timeline \"{seq_name}\": transition \"{}\" at {at} touches no clip; skipped", tr.name));
            return;
        }
        let cut = from.map(|f| f.1).or(to.map(|t| t.1)).unwrap_or(s);
        let align = if from.is_some() && to.is_some() {
            if cut == s {
                TransitionAlign::StartAtCut
            } else if cut == e {
                TransitionAlign::EndAtCut
            } else {
                TransitionAlign::CenterAtCut
            }
        } else if cut == s {
            TransitionAlign::StartAtCut
        } else {
            TransitionAlign::EndAtCut
        };
        let audio = tr.kind == TrackKind::Audio;
        let effect = transition_for(&tr.name, audio, self.report);
        let id = TransitionId(self.b.alloc());
        let Some(track) = seq.tracks_mut(tr.kind).get_mut(tr.track) else { return };
        track.transitions.push(Transition { id, effect, start: s, duration: e - s, from: from.map(|f| f.0), to: to.map(|t| t.0), align, reverse: false });
        track.sort();
    }
}

/// Resolve's transition names, then the shared name table.
fn transition_for(name: &str, audio: bool, report: &mut Report) -> filmcraft_project::EffectInstance {
    let n = name.to_ascii_lowercase().replace(' ', "");
    let id = if audio && n.starts_with("crossfade") {
        Some(if n.contains("-3db") {
            "constant_power"
        } else if n.contains("+3db") {
            "exponential_fade"
        } else {
            "constant_gain"
        })
    } else {
        None
    };
    if let Some(d) = id.and_then(find_effect) {
        return d.instance();
    }
    transition_effect(name, audio, report)
}

/// A Resolve item type as the report names it.
fn pretty_type(db_type: &str) -> &str {
    match db_type {
        "Sm2TiGenerator" => "generator or title",
        "Sm2TiTitle" => "title",
        "Sm2TiFusionClip" | "Sm2MpFusionClip" => "Fusion clip",
        "Sm2TiSubtitle" => "subtitle",
        "Sm2TiAdjustmentClip" => "adjustment clip",
        "Sm2MpMulticamClip" => "multicam clip",
        other => other.trim_start_matches("Sm2Ti").trim_start_matches("Sm2Mp"),
    }
}

/// `HH:MM:SS:FF` of an absolute frame (for messages).
fn timecode(rate: FrameRate, frame: i64) -> String {
    let fps = rate.as_f64().round().max(1.0) as i64;
    let f = frame.max(0);
    format!("{:02}:{:02}:{:02}:{:02}", f / (fps * 3600), f / (fps * 60) % 60, f / fps % 60, f % fps)
}

/// Remove nested-sequence clips that make a sequence contain itself (the engine refuses a
/// project that does), reporting each.
fn break_nesting_cycles(p: &mut Project, report: &mut Report) {
    for _ in 0..p.items.len() + 1 {
        let Some(on_cycle) = p.nest_cycle() else { return };
        // the nested sequences that lead back to `on_cycle`
        let reaches = |p: &Project, from: ItemId| {
            let mut seen = HashSet::new();
            let mut todo = vec![from];
            while let Some(s) = todo.pop() {
                if s == on_cycle {
                    return true;
                }
                if !seen.insert(s) {
                    continue;
                }
                if let Some(q) = p.sequence(s) {
                    todo.extend(q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.item)).filter(|i| p.sequence(*i).is_some()));
                }
            }
            false
        };
        let Some(seq) = p.sequence(on_cycle) else { return };
        let bad: HashSet<ItemId> =
            seq.all_tracks().flat_map(|t| t.items.iter().map(|i| i.item)).filter(|i| p.sequence(*i).is_some() && reaches(p, *i)).collect();
        let name = p.item(on_cycle).map(|i| i.name.clone()).unwrap_or_default();
        let Some(seq) = p.sequence_mut(on_cycle) else { return };
        for t in seq.all_tracks_mut() {
            t.items.retain(|i| !bad.contains(&i.item));
        }
        report.warn(format!("timeline \"{name}\" nests itself through another timeline; those nested clips are skipped"));
    }
}

#[cfg(test)]
mod tests;
