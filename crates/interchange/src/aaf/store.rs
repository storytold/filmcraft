//! AAF objects in structured storage (the AAF Low-Level Container Specification): every object
//! is a storage whose class id is the object's class AUID, holding a `properties` stream; strong
//! references are child storages, collections have an index stream, weak references name a key
//! in a target collection through the root's `referenced properties` table.
//!
//! `properties` stream (little-endian):
//!
//! ```text
//! u8  byte order (0x4C 'L')       u8  format version (0x20)        u16 entry count
//! entry count × { u16 pid, u16 stored form, u16 value length }     the values, in entry order
//! ```
//!
//! | stored form | value |
//! |---|---|
//! | 0x82 data | the property's bytes |
//! | 0x22 strong reference | UTF-16 name of the child storage |
//! | 0x32 strong reference vector | UTF-16 collection name; storages `name{key}`, stream `name index`: u32 count, u32 first free key, u32 last free key, count × u32 local key |
//! | 0x3A strong reference set | as a vector; the index adds u16 key pid, u8 key size, and each entry is u32 local key, u32 reference count, key bytes |
//! | 0x02 weak reference | u16 referenced-property tag, u16 key pid, u8 key size, key bytes |
//! | 0x12 / 0x1A weak reference vector / set | UTF-16 collection name; stream `name index`: u32 count, u16 tag, u16 key pid, u8 key size, count × key |
//! | 0x42 data stream | u8 byte order, UTF-16 name of the stream |
//!
//! `referenced properties` (root stream): u8 byte order, u16 path count, u32 pid count, then the
//! pids of each path (from the root object) terminated by a 0 pid.

use filmcraft_cfb::{CompoundFile, EntryKind, NodeId, Version, Writer};

pub(crate) type Auid = [u8; 16];

pub(crate) const SF_DATA: u16 = 0x82;
pub(crate) const SF_DATA_STREAM: u16 = 0x42;
pub(crate) const SF_STRONG: u16 = 0x22;
pub(crate) const SF_STRONG_VECTOR: u16 = 0x32;
pub(crate) const SF_STRONG_SET: u16 = 0x3A;
pub(crate) const SF_WEAK: u16 = 0x02;
pub(crate) const SF_WEAK_VECTOR: u16 = 0x12;
pub(crate) const SF_WEAK_SET: u16 = 0x1A;

const BYTE_ORDER_LE: u8 = 0x4C;
const FORMAT_VERSION: u8 = 0x20;

/// A property value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Data(Vec<u8>),
    Strong(Box<Obj>),
    StrongVec(Vec<Obj>),
    /// Objects keyed by the data property `key_pid` of each.
    StrongSet(Vec<Obj>, u16),
    /// (path of pids from the root to the target collection, key pid, key)
    Weak(Vec<u16>, u16, Vec<u8>),
    WeakVec(Vec<u16>, u16, Vec<Vec<u8>>),
    Stream(Vec<u8>),
}

/// An AAF object: its class and properties.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Obj {
    pub class: Auid,
    pub props: Vec<(u16, Value)>,
}

impl Obj {
    pub fn new(class: Auid) -> Obj {
        Obj { class, props: Vec::new() }
    }
    pub fn set(&mut self, pid: u16, v: Value) -> &mut Obj {
        match self.props.iter_mut().find(|(p, _)| *p == pid) {
            Some(slot) => slot.1 = v,
            None => self.props.push((pid, v)),
        }
        self
    }
    pub fn with(mut self, pid: u16, v: Value) -> Obj {
        self.set(pid, v);
        self
    }
    pub fn data_prop(mut self, pid: u16, d: Vec<u8>) -> Obj {
        self.set(pid, Value::Data(d));
        self
    }
    pub fn get(&self, pid: u16) -> Option<&Value> {
        self.props.iter().find(|(p, _)| *p == pid).map(|(_, v)| v)
    }
    pub fn data(&self, pid: u16) -> Option<&[u8]> {
        match self.get(pid)? {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }
    pub fn strong(&self, pid: u16) -> Option<&Obj> {
        match self.get(pid)? {
            Value::Strong(o) => Some(o),
            _ => None,
        }
    }
    /// Elements of a strong vector or set (empty when absent).
    pub fn objs(&self, pid: u16) -> &[Obj] {
        match self.get(pid) {
            Some(Value::StrongVec(v)) | Some(Value::StrongSet(v, _)) => v,
            _ => &[],
        }
    }
    pub fn weak_key(&self, pid: u16) -> Option<&[u8]> {
        match self.get(pid)? {
            Value::Weak(_, _, k) => Some(k),
            _ => None,
        }
    }
    pub fn stream(&self, pid: u16) -> Option<&[u8]> {
        match self.get(pid)? {
            Value::Stream(d) => Some(d),
            _ => None,
        }
    }
    pub fn u8(&self, pid: u16) -> Option<u8> {
        self.data(pid).and_then(|d| d.first().copied())
    }
    pub fn u16(&self, pid: u16) -> Option<u16> {
        self.data(pid).filter(|d| d.len() >= 2).map(|d| u16::from_le_bytes([d[0], d[1]]))
    }
    pub fn u32(&self, pid: u16) -> Option<u32> {
        self.data(pid).filter(|d| d.len() >= 4).map(|d| u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }
    pub fn i64(&self, pid: u16) -> Option<i64> {
        let d = self.data(pid)?;
        match d.len() {
            8.. => Some(i64::from_le_bytes(d[..8].try_into().ok()?)),
            4..=7 => Some(i32::from_le_bytes(d[..4].try_into().ok()?) as i64),
            _ => None,
        }
    }
    pub fn rational(&self, pid: u16) -> Option<(i64, i64)> {
        self.data(pid).and_then(rational_of)
    }
    pub fn string(&self, pid: u16) -> Option<String> {
        self.data(pid).map(utf16_of)
    }
    pub fn auid(&self, pid: u16) -> Option<Auid> {
        self.data(pid).and_then(|d| d.get(..16)).and_then(|d| d.try_into().ok())
    }
    pub fn mob_id(&self, pid: u16) -> Option<[u8; 32]> {
        self.data(pid).and_then(|d| d.get(..32)).and_then(|d| d.try_into().ok())
    }
}

pub(crate) fn rational_of(d: &[u8]) -> Option<(i64, i64)> {
    (d.len() >= 8).then(|| (i32::from_le_bytes([d[0], d[1], d[2], d[3]]) as i64, i32::from_le_bytes([d[4], d[5], d[6], d[7]]) as i64))
}

pub(crate) fn rational(num: i64, den: i64) -> Vec<u8> {
    let mut v = (num.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_le_bytes().to_vec();
    v.extend_from_slice(&(den.clamp(i32::MIN as i64, i32::MAX as i64) as i32).to_le_bytes());
    v
}

/// UTF-16LE with a terminating NUL.
pub(crate) fn utf16z(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

pub(crate) fn utf16_of(d: &[u8]) -> String {
    let u: Vec<u16> = d.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&u)
}

/// Short property names used to name storages and streams (any name is valid: readers follow
/// the names stored in the property values).
fn prop_name(pid: u16) -> &'static str {
    match pid {
        0x0001 => "MetaDictionary",
        0x0002 => "Header",
        0x0003 => "ClassDefinitions",
        0x0004 => "TypeDefinitions",
        0x3B03 => "Content",
        0x3B04 => "Dictionary",
        0x3B06 => "IdentificationList",
        0x1901 => "Mobs",
        0x1902 => "EssenceData",
        0x2603 => "OperationDefinitions",
        0x2604 => "ParameterDefinitions",
        0x2605 => "DataDefinitions",
        0x2607 => "CodecDefinitions",
        0x2608 => "ContainerDefinitions",
        0x2609 => "InterpolationDefs",
        0x4403 => "Slots",
        0x4406 => "UserComments",
        0x4803 => "Segment",
        0x1001 => "Components",
        0x1801 => "OperationGroup",
        0x0B02 => "InputSegments",
        0x0B03 => "Parameters",
        0x4E02 => "PointList",
        0x4701 => "EssenceDescription",
        0x2F01 => "Locator",
        0x2702 => "Data",
        0x0204 => "ComponentComments",
        0x1E09 => "ParametersDefined",
        _ => "Property",
    }
}

/// The key size of a strong reference set keyed by `key_pid`, for a set with no entry to take it
/// from: AUIDs (definitions) are 16 bytes, MobIDs 32.
fn set_key_size(key_pid: u16) -> usize {
    match key_pid {
        0x0005 | 0x1B01 => 16,
        0x4401 | 0x2701 => 32,
        _ => 0,
    }
}

fn storage_name(pid: u16) -> String {
    let suffix = format!("-{pid:x}");
    let name: String = prop_name(pid).chars().take(22 - suffix.len()).collect();
    format!("{name}{suffix}")
}

/// Errors while reading the object tree.
fn bad(s: impl Into<String>) -> String {
    s.into()
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

struct W {
    cfb: Writer,
    paths: Vec<Vec<u16>>,
}

/// Serialise `root` (the Root object: MetaDictionary 0x0001, Header 0x0002) into a compound file.
pub(crate) fn write(root: &Obj, version: Version) -> Result<Vec<u8>, String> {
    let mut w = W { cfb: Writer::new(version), paths: Vec::new() };
    w.obj(Writer::ROOT, root)?;
    let mut rp = vec![BYTE_ORDER_LE];
    rp.extend_from_slice(&(w.paths.len() as u16).to_le_bytes());
    let n: usize = w.paths.iter().map(|p| p.len() + 1).sum();
    rp.extend_from_slice(&(n as u32).to_le_bytes());
    for p in &w.paths {
        for pid in p {
            rp.extend_from_slice(&pid.to_le_bytes());
        }
        rp.extend_from_slice(&0u16.to_le_bytes());
    }
    w.cfb.stream(Writer::ROOT, "referenced properties", rp).map_err(|e| e.to_string())?;
    Ok(w.cfb.finish())
}

impl W {
    fn tag(&mut self, path: &[u16]) -> u16 {
        match self.paths.iter().position(|p| p == path) {
            Some(i) => i as u16,
            None => {
                self.paths.push(path.to_vec());
                (self.paths.len() - 1) as u16
            }
        }
    }

    fn obj(&mut self, node: NodeId, o: &Obj) -> Result<(), String> {
        self.cfb.set_clsid(node, o.class);
        let mut entries: Vec<(u16, u16, Vec<u8>)> = Vec::new();
        for (pid, v) in &o.props {
            let pid = *pid;
            let (sf, value) = match v {
                Value::Data(d) => (SF_DATA, d.clone()),
                Value::Strong(child) => {
                    let name = storage_name(pid);
                    let s = self.cfb.storage(node, &name, child.class).map_err(|e| e.to_string())?;
                    self.obj(s, child)?;
                    (SF_STRONG, utf16z(&name))
                }
                Value::StrongVec(items) | Value::StrongSet(items, _) => {
                    let name = storage_name(pid);
                    let mut index = Vec::new();
                    index.extend_from_slice(&(items.len() as u32).to_le_bytes());
                    index.extend_from_slice(&(items.len() as u32).to_le_bytes()); // first free key
                    index.extend_from_slice(&u32::MAX.to_le_bytes()); // last free key
                    let set_key = if let Value::StrongSet(_, k) = v { Some(*k) } else { None };
                    let key_size = set_key.map(|k| items.first().and_then(|i| i.data(k)).map_or_else(|| set_key_size(k), <[u8]>::len)).unwrap_or(0);
                    if let Some(k) = set_key {
                        index.extend_from_slice(&k.to_le_bytes());
                        index.push(key_size as u8);
                    }
                    for (i, item) in items.iter().enumerate() {
                        let s = self.cfb.storage(node, &format!("{name}{{{i:x}}}"), item.class).map_err(|e| e.to_string())?;
                        self.obj(s, item)?;
                        index.extend_from_slice(&(i as u32).to_le_bytes());
                        if let Some(k) = set_key {
                            index.extend_from_slice(&1u32.to_le_bytes());
                            let mut key = item.data(k).unwrap_or(&[]).to_vec();
                            key.resize(key_size, 0);
                            index.extend_from_slice(&key);
                        }
                    }
                    self.cfb.stream(node, &format!("{name} index"), index).map_err(|e| e.to_string())?;
                    (if set_key.is_some() { SF_STRONG_SET } else { SF_STRONG_VECTOR }, utf16z(&name))
                }
                Value::Weak(path, key_pid, key) => {
                    let mut d = self.tag(path).to_le_bytes().to_vec();
                    d.extend_from_slice(&key_pid.to_le_bytes());
                    d.push(key.len() as u8);
                    d.extend_from_slice(key);
                    (SF_WEAK, d)
                }
                Value::WeakVec(path, key_pid, keys) => {
                    let name = storage_name(pid);
                    let tag = self.tag(path);
                    let size = keys.first().map_or(0, Vec::len);
                    let mut index = (keys.len() as u32).to_le_bytes().to_vec();
                    index.extend_from_slice(&tag.to_le_bytes());
                    index.extend_from_slice(&key_pid.to_le_bytes());
                    index.push(size as u8);
                    for k in keys {
                        let mut k = k.clone();
                        k.resize(size, 0);
                        index.extend_from_slice(&k);
                    }
                    self.cfb.stream(node, &format!("{name} index"), index).map_err(|e| e.to_string())?;
                    (SF_WEAK_VECTOR, utf16z(&name))
                }
                Value::Stream(data) => {
                    let name = storage_name(pid);
                    self.cfb.stream(node, &name, data.clone()).map_err(|e| e.to_string())?;
                    let mut d = vec![BYTE_ORDER_LE];
                    d.extend_from_slice(&utf16z(&name));
                    (SF_DATA_STREAM, d)
                }
            };
            if value.len() > u16::MAX as usize {
                return Err(format!("property {pid:#06x} is {} bytes (the limit is 65535)", value.len()));
            }
            entries.push((pid, sf, value));
        }
        let mut props = vec![BYTE_ORDER_LE, FORMAT_VERSION];
        props.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (pid, sf, v) in &entries {
            props.extend_from_slice(&pid.to_le_bytes());
            props.extend_from_slice(&sf.to_le_bytes());
            props.extend_from_slice(&(v.len() as u16).to_le_bytes());
        }
        for (_, _, v) in &entries {
            props.extend_from_slice(v);
        }
        self.cfb.stream(node, "properties", props).map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

struct R<'a> {
    cf: CompoundFile<'a>,
    paths: Vec<Vec<u16>>,
    objects: usize,
}

const MAX_DEPTH: usize = 200;
const MAX_OBJECTS: usize = 2_000_000;

/// Parse the object tree of an AAF compound file (the root object).
pub(crate) fn read(bytes: &[u8]) -> Result<Obj, String> {
    let cf = CompoundFile::open(bytes).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    if let Ok(rp) = cf.read_path("referenced properties")
        && rp.len() >= 7
    {
        let count = u16::from_le_bytes([rp[1], rp[2]]) as usize;
        let mut cur = Vec::new();
        for c in rp[7..].as_chunks::<2>().0 {
            let pid = u16::from_le_bytes([c[0], c[1]]);
            if pid == 0 {
                paths.push(std::mem::take(&mut cur));
                if paths.len() == count {
                    break;
                }
            } else {
                cur.push(pid);
            }
        }
    }
    let mut r = R { cf, paths, objects: 0 };
    r.obj(0, 0)
}

fn rd_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn rd_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl R<'_> {
    fn child(&self, parent: usize, name: &str) -> Result<usize, String> {
        self.cf.child(parent, name).ok_or_else(|| bad(format!("missing storage or stream \"{name}\"")))
    }

    fn stream(&self, parent: usize, name: &str) -> Result<Vec<u8>, String> {
        let id = self.child(parent, name)?;
        self.cf.read(id).map_err(|e| e.to_string())
    }

    fn obj(&mut self, entry: usize, depth: usize) -> Result<Obj, String> {
        if depth > MAX_DEPTH {
            return Err(bad("objects nested too deeply"));
        }
        self.objects += 1;
        if self.objects > MAX_OBJECTS {
            return Err(bad("too many objects"));
        }
        let e = self.cf.entry(entry).ok_or_else(|| bad("bad directory entry"))?;
        if !matches!(e.kind, EntryKind::Root | EntryKind::Storage) {
            return Err(bad(format!("\"{}\" is not a storage", e.name)));
        }
        let mut o = Obj::new(e.clsid);
        let props = self.stream(entry, "properties")?;
        if props.len() < 4 {
            return Err(bad("short properties stream"));
        }
        if props[0] != BYTE_ORDER_LE {
            return Err(bad("big-endian AAF files are not supported"));
        }
        let n = rd_u16(&props, 2).unwrap_or(0) as usize;
        let mut at = 4 + n * 6;
        if at > props.len() {
            return Err(bad("truncated property index"));
        }
        for i in 0..n {
            let (Some(pid), Some(sf), Some(len)) = (rd_u16(&props, 4 + i * 6), rd_u16(&props, 6 + i * 6), rd_u16(&props, 8 + i * 6)) else {
                return Err(bad("truncated property index"));
            };
            let v = props.get(at..at + len as usize).ok_or_else(|| bad("truncated property value"))?;
            at += len as usize;
            let value = match sf {
                SF_STRONG => {
                    let c = self.child(entry, &utf16_of(v))?;
                    Value::Strong(Box::new(self.obj(c, depth + 1)?))
                }
                SF_STRONG_VECTOR | SF_STRONG_SET => {
                    let name = utf16_of(v);
                    let idx = self.stream(entry, &format!("{name} index"))?;
                    let count = rd_u32(&idx, 0).ok_or_else(|| bad("short collection index"))? as usize;
                    let mut items = Vec::new();
                    if sf == SF_STRONG_VECTOR {
                        for k in 0..count {
                            let key = rd_u32(&idx, 12 + k * 4).ok_or_else(|| bad("short vector index"))?;
                            let c = self.child(entry, &format!("{name}{{{key:x}}}"))?;
                            items.push(self.obj(c, depth + 1)?);
                        }
                        Value::StrongVec(items)
                    } else {
                        let key_pid = rd_u16(&idx, 12).ok_or_else(|| bad("short set index"))?;
                        let key_size = *idx.get(14).ok_or_else(|| bad("short set index"))? as usize;
                        let stride = 8 + key_size;
                        for k in 0..count {
                            let key = rd_u32(&idx, 15 + k * stride).ok_or_else(|| bad("short set index"))?;
                            let c = self.child(entry, &format!("{name}{{{key:x}}}"))?;
                            items.push(self.obj(c, depth + 1)?);
                        }
                        Value::StrongSet(items, key_pid)
                    }
                }
                SF_WEAK => {
                    let (Some(tag), Some(key_pid), Some(&size)) = (rd_u16(v, 0), rd_u16(v, 2), v.get(4)) else {
                        return Err(bad("short weak reference"));
                    };
                    let key = v.get(5..5 + size as usize).ok_or_else(|| bad("short weak reference"))?.to_vec();
                    Value::Weak(self.paths.get(tag as usize).cloned().unwrap_or_default(), key_pid, key)
                }
                SF_WEAK_VECTOR | SF_WEAK_SET => {
                    let name = utf16_of(v);
                    let idx = self.stream(entry, &format!("{name} index"))?;
                    let (Some(count), Some(tag), Some(key_pid), Some(&size)) = (rd_u32(&idx, 0), rd_u16(&idx, 4), rd_u16(&idx, 6), idx.get(8)) else {
                        return Err(bad("short weak collection index"));
                    };
                    let size = size as usize;
                    if count > 0 && size == 0 {
                        return Err(bad("zero-size weak collection key"));
                    }
                    // every key must lie in the index, so its length (not the count) bounds the loop
                    if (count as usize).checked_mul(size).and_then(|n| n.checked_add(9)).is_none_or(|end| end > idx.len()) {
                        return Err(bad("short weak collection index"));
                    }
                    let mut keys = Vec::new();
                    for k in 0..count as usize {
                        keys.push(idx.get(9 + k * size..9 + (k + 1) * size).ok_or_else(|| bad("short weak collection index"))?.to_vec());
                    }
                    Value::WeakVec(self.paths.get(tag as usize).cloned().unwrap_or_default(), key_pid, keys)
                }
                SF_DATA_STREAM => {
                    let name = utf16_of(v.get(1..).unwrap_or(&[]));
                    Value::Stream(self.stream(entry, &name)?)
                }
                _ => Value::Data(v.to_vec()),
            };
            o.props.push((pid, value));
        }
        Ok(o)
    }
}
