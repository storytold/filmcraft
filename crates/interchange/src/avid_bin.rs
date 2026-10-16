//! Read-only AVB indexing and supported semantic object decoding. Raw payloads stay borrowed.
//!
//! This module does not convert a bin into a project. Supported identities, attributes,
//! descriptors and timeline wrappers are decoded separately from import policy. Unknown payloads
//! remain accessible; root settings and comprehensive effect semantics are not interpreted.
//! See README for evidence and coverage.
use crate::{Error, Result};

mod effects;
pub use effects::*;
mod picture;
pub use picture::*;
mod locations;
pub use locations::*;
mod semantic;
pub use semantic::*;

/// Native file cap is separate from the smaller external JSON-report cap.
/// Payloads stay borrowed; at most two million object-index entries are allocated.
pub const MAX_BYTES: usize = 512 * 1024 * 1024;
const MAX_OBJECTS: usize = 2_000_000;

fn bad(message: impl std::fmt::Display) -> Error {
    Error::Other(format!("Avid bin: {message}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    order: ByteOrder,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(size).ok_or_else(|| bad("offset overflow"))?;
        let data = self.bytes.get(self.at..end).ok_or_else(|| bad(format!("truncated data at byte {}", self.at)))?;
        self.at = end;
        Ok(data)
    }
    fn number(&mut self, size: usize) -> Result<u64> {
        let order = self.order;
        let data = self.take(size)?;
        Ok(match order {
            ByteOrder::Little => data.iter().rev().fold(0u64, |n, b| (n << 8) | u64::from(*b)),
            ByteOrder::Big => data.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)),
        })
    }
    fn text(&mut self) -> Result<&'a [u8]> {
        let len = self.number(2)? as usize;
        self.take(len)
    }
    fn tag(&mut self) -> Result<[u8; 4]> {
        let mut tag: [u8; 4] = self.take(4)?.try_into().map_err(|_| bad("invalid class tag"))?;
        if self.order == ByteOrder::Little {
            tag.reverse();
        }
        Ok(tag)
    }
}

#[derive(Debug)]
pub struct Object<'a> {
    /// Serialization-local one-based reference, not a persistent mob identity.
    pub index: u32,
    pub class: [u8; 4],
    pub offset: usize,
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub struct Bin<'a> {
    pub byte_order: ByteOrder,
    pub root_index: u32,
    pub saved_at_raw: &'a [u8],
    pub creator_raw: &'a [u8],
    /// Uninterpreted marker/save fields and reserved bytes are retained, never zeroed or guessed.
    pub save_fields_raw: &'a [u8],
    pub reserved_raw: &'a [u8],
    pub objects: Vec<Object<'a>>,
}

#[derive(Debug)]
pub struct Member<'a> {
    pub object_index: u32,
    pub user_placed: bool,
    /// Eight bytes between the reference and placement flag; no coordinate/keyframe semantics yet.
    pub placement_raw: &'a [u8],
}

#[derive(Debug)]
pub struct Membership<'a> {
    pub uid: u64,
    pub view_index: u32,
    pub members: Vec<Member<'a>>,
    /// Bin settings, references and extensions remain opaque. This is not a complete root decoder.
    pub tail_raw: &'a [u8],
}

/// Index exactly the declared object stream. Unknown classes remain available as raw payloads.
/// Bounds checks precede allocation; no payload is copied and no file I/O takes place here.
pub fn index(bytes: &[u8]) -> Result<Bin<'_>> {
    index_with_checkpoint(bytes, &|| Ok(()))
}
/// Index with cancellation checkpoints before allocation and every 4096 objects.
pub fn index_with_checkpoint<'a>(bytes: &'a [u8], checkpoint: &dyn Fn() -> Result<()>) -> Result<Bin<'a>> {
    checkpoint()?;
    if bytes.len() > MAX_BYTES {
        return Err(bad("file exceeds 512 MiB"));
    }
    let order = match bytes.get(..2) {
        Some([6, 0]) => ByteOrder::Little,
        Some([0, 6]) => ByteOrder::Big,
        _ => return Err(bad("missing AVB byte-order signature")),
    };
    let mut c = Cursor { bytes, at: 2, order };
    if c.take(6)? != b"Domain" || c.tag()? != *b"OBJD" || c.text()? != b"AObjDoc" || c.number(1)? != 4 {
        return Err(bad("unsupported document header"));
    }
    let saved_at_raw = c.text()?;
    let count = c.number(4)? as usize;
    let root_index = c.number(4)? as u32;
    let save_fields_raw = c.take(20)?;
    let creator_raw = c.text()?;
    let reserved_raw = c.take(16)?;
    if count == 0 || count > MAX_OBJECTS || count > bytes.len().saturating_sub(c.at) / 8 {
        return Err(bad("invalid or excessive object count"));
    }
    if root_index == 0 || root_index as usize > count {
        return Err(bad("root reference outside object table"));
    }
    let mut objects = Vec::new();
    objects.try_reserve_exact(count).map_err(|e| bad(format!("cannot allocate object index: {e}")))?;
    for i in 0..count {
        if i.is_multiple_of(4096) {
            checkpoint()?;
        }
        let offset = c.at;
        let class = c.tag()?;
        let size = c.number(4)? as usize;
        let payload = c.take(size)?;
        objects.push(Object { index: (i + 1) as u32, class, offset, payload });
    }
    if c.at != bytes.len() {
        return Err(bad(format!("{} unindexed trailing bytes", bytes.len() - c.at)));
    }
    Ok(Bin { byte_order: order, root_index, saved_at_raw, creator_raw, save_fields_raw, reserved_raw, objects })
}

impl<'a> Bin<'a> {
    /// Reference zero is null. Nonzero dangling references are errors, never null substitutes.
    pub fn object(&self, reference: u32) -> Result<Option<&Object<'a>>> {
        if reference == 0 {
            return Ok(None);
        }
        self.objects.get((reference - 1) as usize).map(Some).ok_or_else(|| bad(format!("dangling object reference {reference}")))
    }

    /// The observed ABIN v0x0e membership table. Other root/version variants are explicit errors.
    pub fn membership(&self) -> Result<Membership<'a>> {
        let root = self.object(self.root_index)?.ok_or_else(|| bad("null bin root"))?;
        if root.class != *b"ABIN" {
            return Err(bad("unsupported root class for membership"));
        }
        let mut c = Cursor { bytes: root.payload, at: 0, order: self.byte_order };
        if c.take(2)? != [2, 0x0e] {
            return Err(bad("unsupported ABIN membership version"));
        }
        let view_index = c.number(4)? as u32;
        self.object(view_index)?;
        let uid = c.number(8)?;
        let count = c.number(2)? as usize;
        if count > root.payload.len().saturating_sub(c.at) / 13 {
            return Err(bad("truncated bin membership table"));
        }
        let mut members = Vec::new();
        members.try_reserve_exact(count).map_err(|e| bad(format!("cannot allocate membership: {e}")))?;
        for _ in 0..count {
            let object_index = c.number(4)? as u32;
            let object = self.object(object_index)?.ok_or_else(|| bad("null bin member"))?;
            if object.class != *b"CMPO" {
                return Err(bad("unsupported bin member class"));
            }
            let placement_raw = c.take(8)?;
            let user_placed = match c.number(1)? {
                0 => false,
                1 => true,
                _ => return Err(bad("unsupported bin member placement flag")),
            };
            members.push(Member { object_index, user_placed, placement_raw });
        }
        let tail_raw = c.take(root.payload.len().saturating_sub(c.at))?;
        Ok(Membership { uid, view_index, members, tail_raw })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod semantic_tests;
