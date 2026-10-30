//! The observed component, mob, attribute and simple editorial layouts. No graph expansion here.
use super::{Bin, ByteOrder, Cursor, bad};
use crate::Result;
use serde::Serialize;
use std::{borrow::Cow, collections::BTreeSet};

#[derive(Debug, Serialize)]
pub struct Text<'a> {
    pub raw: &'a [u8],
    pub value: Cow<'a, str>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct DecimalRate {
    pub mantissa: i32,
    pub exponent: i16,
}
impl DecimalRate {
    /// Exact decimal evidence, never an implicit NTSC rate conversion.
    pub fn rational(self) -> Result<(i64, i64)> {
        if self.mantissa <= 0 || !(-9..=9).contains(&self.exponent) {
            return Err(bad("unsupported or invalid decimal edit rate"));
        }
        let scale = 10i64.pow(u32::from(self.exponent.unsigned_abs()));
        Ok(if self.exponent < 0 { (i64::from(self.mantissa), scale) } else { (i64::from(self.mantissa) * scale, 1) })
    }
}
#[derive(Debug, Serialize)]
pub struct Component<'a> {
    pub left: u32,
    pub right: u32,
    pub media_kind: u16,
    pub rate: DecimalRate,
    pub name: Option<Text<'a>>,
    pub effect: Option<Text<'a>>,
    pub attributes: u32,
    pub session_attributes: u32,
    pub precomputed: u32,
    pub parameters: Option<u32>,
}
#[derive(Debug, Serialize)]
pub struct Track {
    pub flags: u16,
    pub index: Option<u16>,
    pub attributes: Option<u32>,
    pub session_attributes: Option<u32>,
    pub component: Option<u32>,
    pub filler_proxy: Option<u32>,
    pub bob_data: Option<u32>,
    pub start: Option<i32>,
    pub lock: Option<u16>,
}
#[derive(Debug, Serialize)]
pub struct Mob<'a> {
    pub common: Component<'a>,
    pub mode: u8,
    pub length: i32,
    pub tracks: Vec<Track>,
    /// The eight fixed legacy ID bytes are not silently promoted into a complete identity.
    pub legacy_id_raw: &'a [u8],
    pub modified_raw: u32,
    pub created_raw: Option<u32>,
    pub kind: u8,
    pub usage: u32,
    pub descriptor: u32,
    /// Canonical big-endian material-number fields; the original payload remains in Bin.
    pub id: Option<[u8; 32]>,
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum AttributeValue<'a> {
    Integer(i32),
    Text(Option<Text<'a>>),
    Reference(u32),
    Bytes(&'a [u8]),
}
#[derive(Debug, Serialize)]
pub struct Attribute<'a> {
    pub name: Text<'a>,
    pub value: AttributeValue<'a>,
}
#[derive(Debug, Serialize)]
#[serde(tag = "class")]
pub enum Decoded<'a> {
    Group(super::GroupEffect<'a>),
    Auxiliary {
        common: Component<'a>,
        length: i32,
        fields: std::collections::BTreeMap<&'static str, super::Field<'a>>,
    },
    Locator(super::Locator<'a>),
    BinReference(super::BinReference<'a>),
    Descriptor(super::Descriptor<'a>),
    Attributes {
        entries: Vec<Attribute<'a>>,
    },
    Mob(Mob<'a>),
    Sequence {
        common: Component<'a>,
        components: Vec<u32>,
    },
    /// A group clip's per-track selector. Each track is one alternative this track can play, one
    /// per angle carrying its media kind. `selection_raw` is a candidate selection field; it
    /// is 1 in every selector observed, so its meaning is unestablished. Alternative ordering
    /// or other composition metadata could also participate in the saved choice.
    Selector {
        common: Component<'a>,
        mode: u8,
        length: i32,
        tracks: Vec<Track>,
        selection_raw: u8,
        trailing_raw: u16,
    },
    SourceClip {
        common: Component<'a>,
        length: i32,
        legacy_id_raw: &'a [u8],
        track: u16,
        start: i32,
        id: Option<[u8; 32]>,
    },
    Filler {
        common: Component<'a>,
        length: i32,
    },
    Timecode {
        common: Component<'a>,
        length: i32,
        flags: u32,
        fps: u32,
        start: i64,
    },
}
pub(super) struct Decode<'a, 'b> {
    pub(super) c: Cursor<'a>,
    bin: &'b Bin<'a>,
}
impl<'a> Decode<'a, '_> {
    pub(super) fn expect(&mut self, bytes: &[u8]) -> Result<()> {
        let offset = self.c.at;
        if self.c.take(bytes.len())? != bytes {
            return Err(bad(format!("unsupported field marker at payload byte {offset}")));
        }
        Ok(())
    }
    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.c.number(1)? as u8)
    }
    pub(super) fn u16(&mut self) -> Result<u16> {
        Ok(self.c.number(2)? as u16)
    }
    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(self.c.number(4)? as u32)
    }
    pub(super) fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub(super) fn reference(&mut self) -> Result<u32> {
        let id = self.u32()?;
        self.bin.object(id)?;
        Ok(id)
    }
    pub(super) fn text(&mut self) -> Result<Option<Text<'a>>> {
        let count = self.u16()?;
        if count == u16::MAX {
            return Ok(None);
        }
        let raw = self.c.take(usize::from(count))?;
        let length = raw.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let bytes = raw.get(..length).ok_or_else(|| bad("invalid text bounds"))?;
        let (value, errors) = encoding_rs::MACINTOSH.decode_without_bom_handling(bytes);
        if errors {
            return Err(bad("invalid MacRoman text"));
        }
        Ok(Some(Text { raw, value }))
    }
    pub(super) fn extension(&mut self, seen: &mut BTreeSet<u8>) -> Result<Option<u8>> {
        if self.c.bytes.get(self.c.at) != Some(&1) {
            return Ok(None);
        }
        self.c.take(1)?;
        let tag = self.u8()?;
        if !seen.insert(tag) {
            return Err(bad("duplicate extension tag"));
        }
        Ok(Some(tag))
    }
    pub(super) fn common(&mut self) -> Result<Component<'a>> {
        self.expect(&[2, 3])?;
        let left = self.reference()?;
        let right = self.reference()?;
        let media_kind = self.u16()?;
        let rate = DecimalRate { mantissa: self.i32()?, exponent: self.u16()? as i16 };
        rate.rational()?;
        let name = self.text()?;
        let effect = self.text()?;
        let attributes = self.reference()?;
        let session_attributes = self.reference()?;
        let precomputed = self.reference()?;
        let mut parameters = None;
        let mut seen = BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            if tag != 1 {
                return Err(bad(format!("unsupported component extension {tag}")));
            }
            self.expect(&[0x48])?;
            parameters = Some(self.reference()?);
        }
        Ok(Component { left, right, media_kind, rate, name, effect, attributes, session_attributes, precomputed, parameters })
    }
    fn count(&mut self, min_bytes: usize, cap: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        if count > cap || count > self.c.bytes.len().saturating_sub(self.c.at) / min_bytes {
            return Err(bad("excessive or truncated collection"));
        }
        Ok(count)
    }
    pub(super) fn identity(&mut self) -> Result<[u8; 32]> {
        let mut id = [0; 32];
        self.expect(&[0x41])?;
        if self.u32()? != 12 {
            return Err(bad("invalid identity label width"));
        }
        id.get_mut(..12).ok_or_else(|| bad("identity bounds"))?.copy_from_slice(self.c.take(12)?);
        for i in 12..16 {
            self.expect(&[0x44])?;
            *id.get_mut(i).ok_or_else(|| bad("identity bounds"))? = self.u8()?;
        }
        for (marker, range) in [(0x48, 16..20), (0x46, 20..22), (0x46, 22..24)] {
            self.expect(&[marker])?;
            let field = id.get_mut(range).ok_or_else(|| bad("identity bounds"))?;
            field.copy_from_slice(self.c.take(field.len())?);
            if self.c.order == ByteOrder::Little {
                field.reverse();
            }
        }
        self.expect(&[0x41])?;
        if self.u32()? != 8 {
            return Err(bad("invalid identity tail width"));
        }
        id.get_mut(24..).ok_or_else(|| bad("identity bounds"))?.copy_from_slice(self.c.take(8)?);
        Ok(id)
    }
    pub(super) fn group(&mut self) -> Result<(u8, i32, Vec<Track>)> {
        self.expect(&[2, 8])?;
        let mode = self.u8()?;
        let length = self.i32()?;
        if self.u32()? != 0 {
            return Err(bad("unsupported nonzero scalar count"));
        }
        let count = self.count(2, 4096)?;
        let mut tracks = Vec::new();
        for _ in 0..count {
            let flags = self.u16()?;
            if flags & !0x29f != 0 {
                return Err(bad(format!("unsupported track flags {flags:04x}")));
            }
            let index = if flags & 1 != 0 { Some(self.u16()?) } else { None };
            let attributes = if flags & 2 != 0 { Some(self.reference()?) } else { None };
            let session_attributes = if flags & 512 != 0 { Some(self.reference()?) } else { None };
            let component = if flags & 4 != 0 { Some(self.reference()?) } else { None };
            let filler_proxy = if flags & 8 != 0 { Some(self.reference()?) } else { None };
            let bob_data = if flags & 16 != 0 { Some(self.reference()?) } else { None };
            let start = if flags & 128 != 0 { Some(self.i32()?) } else { None };
            tracks.push(Track { flags, index, attributes, session_attributes, component, filler_proxy, bob_data, start, lock: None });
        }
        let mut seen = BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            if tag != 1 {
                return Err(bad(format!("unsupported track-group extension {tag}")));
            }
            for track in &mut tracks {
                self.expect(&[0x45])?;
                track.lock = Some(self.u16()?);
            }
        }
        Ok((mode, length, tracks))
    }
    fn mob(&mut self, common: Component<'a>) -> Result<Mob<'a>> {
        let (mode, length, tracks) = self.group()?;
        self.expect(&[2, 2])?;
        let legacy_id_raw = self.c.take(8)?;
        let modified_raw = self.u32()?;
        let kind = self.u8()?;
        let usage = self.u32()?;
        let descriptor = self.reference()?;
        let mut created_raw = None;
        let mut id = None;
        let mut seen = BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            match tag {
                1 => {
                    self.expect(&[0x47])?;
                    created_raw = Some(self.u32()?);
                }
                2 => id = Some(self.identity()?),
                _ => return Err(bad(format!("unsupported mob extension {tag}"))),
            }
        }
        Ok(Mob { common, mode, length, tracks, legacy_id_raw, modified_raw, created_raw, kind, usage, descriptor, id })
    }
    fn attributes(&mut self) -> Result<Decoded<'a>> {
        self.expect(&[2, 1])?;
        let count = self.count(6, 65_536)?;
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for _ in 0..count {
            let kind = self.u32()?;
            let name = self.text()?.ok_or_else(|| bad("null attribute name"))?;
            if !names.insert(name.value.clone()) {
                return Err(bad("duplicate attribute name"));
            }
            let value = match kind {
                1 => AttributeValue::Integer(self.i32()?),
                2 => AttributeValue::Text(self.text()?),
                3 => AttributeValue::Reference(self.reference()?),
                4 => {
                    let size = self.u32()? as usize;
                    AttributeValue::Bytes(self.c.take(size)?)
                }
                _ => return Err(bad(format!("unsupported attribute kind {kind}"))),
            };
            entries.push(Attribute { name, value });
        }
        Ok(Decoded::Attributes { entries })
    }
}
impl<'a> Bin<'a> {
    /// Decode a supported object, without recursively following its references. Unsupported
    /// classes/extensions return errors; their original payload stays accessible through `object`.
    pub fn decode(&self, reference: u32) -> Result<Decoded<'a>> {
        let object = self.object(reference)?.ok_or_else(|| bad("cannot decode a null object"))?;
        let mut d = Decode { c: Cursor { bytes: object.payload, at: 0, order: self.byte_order }, bin: self };
        let out = if matches!(&object.class, b"FILE" | b"MSML" | b"URLL" | b"WINF") {
            Decoded::Locator(d.locator(object.class)?)
        } else if object.class == *b"MCBR" {
            Decoded::BinReference(d.bin_reference()?)
        } else if matches!(&object.class, b"MDES" | b"PCMA" | b"CDCI") {
            Decoded::Descriptor(d.descriptor(object.class)?)
        } else if object.class == *b"ATTR" {
            d.attributes()?
        } else {
            if !matches!(&object.class, b"CMPO" | b"SEQU" | b"SCLP" | b"FILL" | b"TCCP" | b"TKFX" | b"SPED" | b"MASK" | b"ECCP" | b"TRKR" | b"SLCT") {
                // Name the class and object so an unsupported bin is actionable evidence.
                return Err(bad(format!(
                    "unsupported semantic class {} at object {reference} (byte {})",
                    String::from_utf8_lossy(&object.class),
                    object.offset
                )));
            }
            let common = d.common()?;
            match &object.class {
                b"CMPO" => Decoded::Mob(d.mob(common)?),
                b"TKFX" | b"SPED" | b"MASK" => Decoded::Group(d.effect(common, object.class)?),
                b"SEQU" => {
                    d.expect(&[2, 3])?;
                    let count = d.count(4, 100_000)?;
                    let mut components = Vec::new();
                    for _ in 0..count {
                        components.push(d.reference()?);
                    }
                    Decoded::Sequence { common, components }
                }
                // A group clip's selector is a track group: each of its tracks is one alternative
                // this track can play, one per angle carrying its media kind. The trailing byte
                // is 1 in every selector observed. Its selection/flag semantics are not
                // established, so it is preserved rather than interpreted.
                b"SLCT" => {
                    let (mode, length, tracks) = d.group()?;
                    d.expect(&[2, 1])?;
                    let selection_raw = d.u8()?;
                    let trailing_raw = d.u16()?;
                    Decoded::Selector { common, mode, length, tracks, selection_raw, trailing_raw }
                }
                _ => {
                    d.expect(&[2, 1])?;
                    let length = d.i32()?;
                    match &object.class {
                        b"SCLP" => {
                            d.expect(&[2, 3])?;
                            let legacy_id_raw = d.c.take(8)?;
                            let track = d.u16()?;
                            let start = d.i32()?;
                            let mut id = None;
                            let mut seen = BTreeSet::new();
                            while let Some(tag) = d.extension(&mut seen)? {
                                if tag != 1 {
                                    return Err(bad(format!("unsupported source-clip extension {tag}")));
                                }
                                id = Some(d.identity()?);
                            }
                            Decoded::SourceClip { common, length, legacy_id_raw, track, start, id }
                        }
                        b"ECCP" | b"TRKR" => Decoded::Auxiliary { common, length, fields: d.auxiliary(object.class)? },
                        b"TCCP" => {
                            d.expect(&[2, 1])?;
                            let flags = d.u32()?;
                            let fps = d.u32()?;
                            // Two endian-aware words, high word first even in a little-endian file.
                            let start = (i64::from(d.i32()?) << 32) | i64::from(d.u32()?);
                            Decoded::Timecode { common, length, flags, fps, start }
                        }
                        _ => {
                            d.expect(&[2, 1])?;
                            Decoded::Filler { common, length }
                        }
                    }
                }
            }
        };
        d.expect(&[3])?;
        if d.c.at != object.payload.len() {
            return Err(bad("unparsed semantic object bytes"));
        }
        Ok(out)
    }
}
