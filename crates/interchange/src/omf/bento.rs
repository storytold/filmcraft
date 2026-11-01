//! The Bento container (Apple, *Bento Specification* revision 1.0d5), the storage layer of OMF
//! Interchange 2.0 files.
//!
//! A Bento container is a heap of value bytes, a table of contents (TOC) describing every value of
//! every object, and a fixed-size label at the very end of the file that locates the TOC:
//!
//! ```text
//! label (24 bytes, big-endian): magic A4 'C' 'M' A5 'H' 'd' 'r' D7, u16 flags, u16 buffer size
//!                               (KiB), u16 major version (1), u16 minor version (0),
//!                               u32 TOC offset, u32 TOC size
//! ```
//!
//! Every value belongs to an (object id, property id, type id) triple. Properties and types are
//! themselves objects carrying a global name (`"OMFI:CPNT:Length"`, `"omfi:Length32"`). The TOC
//! is a stream of one-byte operations, each followed by its operands (big-endian):
//!
//! | op | operands | meaning |
//! |---|---|---|
//! | 1 NewObject | u32 id | following entries belong to this object |
//! | 2 NewProperty | u32 id | … to this property |
//! | 3 NewType | u32 id | starts a new value of this type |
//! | 4 ExplicitGen | u32 | generation of the value |
//! | 5 Offset4Len4 | u32 offset, u32 length | a value segment in the file |
//! | 6 ContdOffset4Len4 | u32 offset, u32 length | a continuation segment |
//! | 7 Offset8Len4 / 8 Contd… | u64 offset, u32 length | segments beyond 4 GiB |
//! | 9–13 Immediate0–4 | 4 bytes | a value of 0–4 bytes stored in the TOC |
//! | 14 ContdImmediate4 | 4 bytes | continuation |
//! | 15 ReferenceListID | u32 id | the object holding this value's reference list |
//! | 24 EndOfBufr | — | skip to the end of the current TOC buffer |
//! | 0xFF Padding | — | ignored |
//!
//! Object references (OMF's `omfi:ObjRef`) are 4-byte keys resolved through the referencing
//! object's reference list (standard property 22: pairs of u32 key, u32 object id). FilmCraft
//! uses the referenced object's id as the key.

use std::collections::{BTreeMap, HashMap};

pub(crate) const MAGIC: [u8; 8] = [0xA4, b'C', b'M', 0xA5, b'H', b'd', b'r', 0xD7];
pub(crate) const LABEL_SIZE: usize = 24;

// standard object ids
const TOC_OBJECT: u32 = 1;
const TOC_SEED: u32 = 2;
const TOC_MIN_SEED: u32 = 3;
const TOC_TYPE: u32 = 19;
const TYPE_7BIT_ASCII: u32 = 21;
pub(crate) const OBJ_REFERENCES: u32 = 22;
const GLOBAL_TYPE_NAME: u32 = 23;
const GLOBAL_PROP_NAME: u32 = 24;
const MIN_USER_ID: u32 = 100;

const OP_NEW_OBJECT: u8 = 1;
const OP_NEW_PROPERTY: u8 = 2;
const OP_NEW_TYPE: u8 = 3;
const OP_EXPLICIT_GEN: u8 = 4;
const OP_OFFSET4_LEN4: u8 = 5;
const OP_CONTD_OFFSET4_LEN4: u8 = 6;
const OP_OFFSET8_LEN4: u8 = 7;
const OP_CONTD_OFFSET8_LEN4: u8 = 8;
const OP_IMMEDIATE0: u8 = 9;
const OP_IMMEDIATE4: u8 = 13;
const OP_CONTD_IMMEDIATE4: u8 = 14;
const OP_REFERENCE_LIST_ID: u8 = 15;
const OP_END_OF_BUFFER: u8 = 24;
const OP_PADDING: u8 = 0xFF;

const TOC_BUFFER_KIB: u16 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seg {
    Imm(Vec<u8>),
    At(u64, u32),
}

/// Bento values are metadata, not essence payloads. Limit reassembled segments so repeated
/// references cannot amplify a small OMF into an enormous allocation.
const MAX_VALUE_BYTES: usize = 16 << 20;

/// Builds a Bento container.
pub(crate) struct Writer {
    data: Vec<u8>,
    names: HashMap<(u32, String), u32>,
    next: u32,
    /// (object, property, type, segments)
    entries: Vec<(u32, u32, u32, Vec<Seg>)>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { data: Vec::new(), names: HashMap::new(), next: MIN_USER_ID, entries: Vec::new() }
    }

    fn named(&mut self, kind: u32, name: &str) -> u32 {
        if let Some(&id) = self.names.get(&(kind, name.to_string())) {
            return id;
        }
        let id = self.new_object();
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        let seg = self.store(&v);
        self.entries.push((id, kind, TYPE_7BIT_ASCII, vec![seg]));
        self.names.insert((kind, name.to_string()), id);
        id
    }

    pub fn property(&mut self, name: &str) -> u32 {
        self.named(GLOBAL_PROP_NAME, name)
    }

    pub fn type_id(&mut self, name: &str) -> u32 {
        self.named(GLOBAL_TYPE_NAME, name)
    }

    pub fn new_object(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    fn store(&mut self, v: &[u8]) -> Seg {
        if v.len() <= 4 {
            return Seg::Imm(v.to_vec());
        }
        let at = self.data.len() as u64;
        self.data.extend_from_slice(v);
        Seg::At(at, v.len() as u32)
    }

    /// Set a value of `obj` (values of more than 4 GiB are not supported by OMF).
    pub fn set(&mut self, obj: u32, property: &str, ty: &str, value: &[u8]) {
        let p = self.property(property);
        let t = self.type_id(ty);
        let seg = self.store(value);
        self.entries.push((obj, p, t, vec![seg]));
    }

    /// The reference list of `obj` (keys are object ids).
    pub fn set_references(&mut self, obj: u32, targets: &[u32]) {
        let mut v = Vec::with_capacity(targets.len() * 8);
        for t in targets {
            v.extend_from_slice(&t.to_be_bytes());
            v.extend_from_slice(&t.to_be_bytes());
        }
        let seg = self.store(&v);
        self.entries.push((obj, OBJ_REFERENCES, TYPE_7BIT_ASCII, vec![seg]));
    }

    pub fn finish(mut self) -> Vec<u8> {
        let seed = self.next;
        self.entries.push((TOC_OBJECT, TOC_SEED, TOC_TYPE, vec![Seg::Imm(seed.to_be_bytes().to_vec())]));
        self.entries.push((TOC_OBJECT, TOC_MIN_SEED, TOC_TYPE, vec![Seg::Imm(MIN_USER_ID.to_be_bytes().to_vec())]));
        self.entries.sort_by_key(|e| (e.0, e.1, e.2));
        let mut toc = Vec::new();
        let (mut cur_obj, mut cur_prop) = (u32::MAX, u32::MAX);
        for (obj, prop, ty, segs) in &self.entries {
            if *obj != cur_obj {
                toc.push(OP_NEW_OBJECT);
                toc.extend_from_slice(&obj.to_be_bytes());
                cur_obj = *obj;
                cur_prop = u32::MAX;
            }
            if *prop != cur_prop {
                toc.push(OP_NEW_PROPERTY);
                toc.extend_from_slice(&prop.to_be_bytes());
                cur_prop = *prop;
            }
            toc.push(OP_NEW_TYPE);
            toc.extend_from_slice(&ty.to_be_bytes());
            for (i, s) in segs.iter().enumerate() {
                match s {
                    Seg::Imm(b) => {
                        toc.push(if i > 0 { OP_CONTD_IMMEDIATE4 } else { OP_IMMEDIATE0 + b.len() as u8 });
                        let mut w = [0u8; 4];
                        w[..b.len()].copy_from_slice(b);
                        toc.extend_from_slice(&w);
                    }
                    Seg::At(off, len) if *off <= u32::MAX as u64 => {
                        toc.push(if i > 0 { OP_CONTD_OFFSET4_LEN4 } else { OP_OFFSET4_LEN4 });
                        toc.extend_from_slice(&(*off as u32).to_be_bytes());
                        toc.extend_from_slice(&len.to_be_bytes());
                    }
                    Seg::At(off, len) => {
                        toc.push(if i > 0 { OP_CONTD_OFFSET8_LEN4 } else { OP_OFFSET8_LEN4 });
                        toc.extend_from_slice(&off.to_be_bytes());
                        toc.extend_from_slice(&len.to_be_bytes());
                    }
                }
            }
        }
        let mut out = self.data;
        let toc_at = out.len() as u64;
        out.extend_from_slice(&toc);
        let mut label = MAGIC.to_vec();
        label.extend_from_slice(&0u16.to_be_bytes()); // flags
        label.extend_from_slice(&TOC_BUFFER_KIB.to_be_bytes());
        label.extend_from_slice(&1u16.to_be_bytes());
        label.extend_from_slice(&0u16.to_be_bytes());
        label.extend_from_slice(&(toc_at.min(u32::MAX as u64) as u32).to_be_bytes());
        label.extend_from_slice(&(toc.len() as u32).to_be_bytes());
        out.extend_from_slice(&label);
        out
    }
}

/// One value of a parsed object.
#[derive(Clone, Debug)]
pub(crate) struct Prop {
    pub prop: u32,
    #[allow(dead_code)]
    pub ty: u32,
    segs: Vec<Seg>,
}

/// A parsed Bento container.
pub(crate) struct Container<'a> {
    data: &'a [u8],
    pub objects: BTreeMap<u32, Vec<Prop>>,
    names: HashMap<u32, String>,
    by_name: HashMap<String, u32>,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Whether `b` ends with a Bento label.
pub(crate) fn sniff(b: &[u8]) -> bool {
    b.len() >= LABEL_SIZE && b[b.len() - LABEL_SIZE..b.len() - LABEL_SIZE + 8] == MAGIC
}

impl<'a> Container<'a> {
    pub fn open(data: &'a [u8]) -> Result<Container<'a>, String> {
        if !sniff(data) {
            return Err("no Bento label at the end of the file".into());
        }
        let l = &data[data.len() - LABEL_SIZE..];
        let buf = (u16::from_be_bytes([l[10], l[11]]) as usize).max(1) * 1024;
        let toc_at = be32(l, 16).unwrap_or(0) as usize;
        let toc_len = be32(l, 20).unwrap_or(0) as usize;
        let toc = data.get(toc_at..toc_at.checked_add(toc_len).ok_or("bad TOC size")?).ok_or("the TOC lies outside the file")?;
        let mut c = Container { data, objects: BTreeMap::new(), names: HashMap::new(), by_name: HashMap::new() };
        let (mut obj, mut prop) = (0u32, 0u32);
        let mut i = 0;
        while i < toc.len() {
            let op = toc[i];
            i += 1;
            let need = |n: usize| -> Result<&[u8], String> { toc.get(i..i + n).ok_or_else(|| "truncated TOC".to_string()) };
            match op {
                OP_NEW_OBJECT => {
                    obj = be32(need(4)?, 0).unwrap_or(0);
                    i += 4;
                }
                OP_NEW_PROPERTY => {
                    prop = be32(need(4)?, 0).unwrap_or(0);
                    i += 4;
                }
                OP_NEW_TYPE => {
                    let ty = be32(need(4)?, 0).unwrap_or(0);
                    i += 4;
                    c.objects.entry(obj).or_default().push(Prop { prop, ty, segs: Vec::new() });
                }
                OP_EXPLICIT_GEN | OP_REFERENCE_LIST_ID => {
                    need(4)?;
                    i += 4;
                }
                OP_OFFSET4_LEN4 | OP_CONTD_OFFSET4_LEN4 => {
                    let b = need(8)?;
                    let seg = Seg::At(be32(b, 0).unwrap_or(0) as u64, be32(b, 4).unwrap_or(0));
                    i += 8;
                    c.push_seg(obj, seg)?;
                }
                OP_OFFSET8_LEN4 | OP_CONTD_OFFSET8_LEN4 => {
                    let b = need(12)?;
                    let off = u64::from_be_bytes(b[..8].try_into().map_err(|_| "truncated TOC")?);
                    let seg = Seg::At(off, be32(b, 8).unwrap_or(0));
                    i += 12;
                    c.push_seg(obj, seg)?;
                }
                x @ (OP_IMMEDIATE0..=OP_IMMEDIATE4 | OP_CONTD_IMMEDIATE4) => {
                    let b = need(4)?;
                    let n = if x == OP_CONTD_IMMEDIATE4 { 4 } else { (x - OP_IMMEDIATE0) as usize };
                    let seg = Seg::Imm(b[..n].to_vec());
                    i += 4;
                    c.push_seg(obj, seg)?;
                }
                OP_END_OF_BUFFER => {
                    // the rest of this TOC buffer is unused
                    i = (i - 1) / buf * buf + buf;
                }
                OP_PADDING | 0 => {}
                other => return Err(format!("unknown TOC operation {other}")),
            }
        }
        // global names
        let ids: Vec<u32> = c.objects.keys().copied().collect();
        for id in ids {
            let name = c.objects[&id].iter().find(|p| p.prop == GLOBAL_PROP_NAME || p.prop == GLOBAL_TYPE_NAME).and_then(|p| c.value(p).ok());
            if let Some(n) = name {
                let s = String::from_utf8_lossy(&n).trim_end_matches('\0').to_string();
                c.by_name.insert(s.clone(), id);
                c.names.insert(id, s);
            }
        }
        Ok(c)
    }

    fn push_seg(&mut self, obj: u32, seg: Seg) -> Result<(), String> {
        let p = self.objects.get_mut(&obj).and_then(|v| v.last_mut()).ok_or("a value without a type in the TOC")?;
        if p.segs.len() > 1 << 20 {
            return Err("too many value segments".into());
        }
        p.segs.push(seg);
        Ok(())
    }

    /// The bytes of a value.
    pub fn value(&self, p: &Prop) -> Result<Vec<u8>, String> {
        let total = p.segs.iter().try_fold(0usize, |total, s| {
            let len = match s {
                Seg::Imm(b) => b.len(),
                Seg::At(_, len) => *len as usize,
            };
            total.checked_add(len).ok_or_else(|| "Bento value length overflows".to_string())
        })?;
        if total > MAX_VALUE_BYTES {
            return Err("Bento value exceeds the decoded-value budget".into());
        }
        let mut out = Vec::new();
        out.try_reserve_exact(total).map_err(|_| "cannot allocate Bento value")?;
        for s in &p.segs {
            match s {
                Seg::Imm(b) => out.extend_from_slice(b),
                Seg::At(off, len) => {
                    let start = usize::try_from(*off).map_err(|_| "value offset too large")?;
                    let b = self.data.get(start..start.checked_add(*len as usize).ok_or("bad value length")?).ok_or("a value lies outside the file")?;
                    out.extend_from_slice(b);
                }
            }
        }
        Ok(out)
    }

    pub fn id_of(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    #[allow(dead_code)]
    pub fn name_of(&self, id: u32) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// The first value of property `name` of `obj`.
    pub fn get(&self, obj: u32, name: &str) -> Option<Vec<u8>> {
        let pid = self.id_of(name)?;
        let p = self.objects.get(&obj)?.iter().find(|p| p.prop == pid)?;
        self.value(p).ok()
    }

    /// Resolve an object reference key of `obj` through its reference list.
    pub fn resolve(&self, obj: u32, key: u32) -> u32 {
        if let Some(p) = self.objects.get(&obj).and_then(|v| v.iter().find(|p| p.prop == OBJ_REFERENCES))
            && let Ok(v) = self.value(p)
        {
            for pair in v.as_chunks::<8>().0 {
                if be32(pair, 0) == Some(key) {
                    return be32(pair, 4).unwrap_or(key);
                }
            }
        }
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_segments_cannot_expand_a_value_past_its_budget() {
        let p = Prop { prop: 1, ty: 1, segs: vec![Seg::At(0, MAX_VALUE_BYTES as u32), Seg::Imm(vec![0])] };
        let c = Container { data: &[], objects: BTreeMap::new(), names: HashMap::new(), by_name: HashMap::new() };
        assert!(c.value(&p).unwrap_err().contains("budget"));
    }
}
