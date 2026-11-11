//! Source location evidence. A file locator is not proof that an AMA or managed source is online.
use super::semantic::Decode;
use super::{ByteOrder, DecimalRate, Text, bad};
use crate::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Locator<'a> {
    pub path: Option<Text<'a>>,
    pub posix: Option<Text<'a>>,
    pub utf8: Option<Text<'a>>,
    /// FILE extension 3: an additional UTF-8 location, observed as either a UNC or
    /// POSIX path. Retained separately; its precedence over other paths is unknown.
    pub alternate_utf8: Option<Text<'a>>,
    pub volume: Option<Text<'a>>,
    pub volume_utf8: Option<Text<'a>>,
    pub legacy_id_raw: Option<&'a [u8]>,
    pub id: Option<[u8; 32]>,
    pub domain: Option<u32>,
}
#[derive(Debug, Serialize)]
pub struct BinReference<'a> {
    pub high: u32,
    pub low: u32,
    pub name: Option<Text<'a>>,
    pub utf8: Option<Text<'a>>,
}
#[derive(Debug, Serialize)]
pub struct Descriptor<'a> {
    pub mob_kind: u8,
    pub locator: u32,
    pub intermediate: bool,
    pub physical_media: u32,
    pub uuid: Option<[u8; 16]>,
    pub plugin_raw: Option<&'a [u8]>,
    pub attributes: Option<u32>,
    pub file: Option<FileDescriptor>,
    pub pcm: Option<Pcm>,
    pub picture: Option<super::Picture<'a>>,
}
#[derive(Debug, Serialize)]
pub struct FileDescriptor {
    pub rate: DecimalRate,
    pub length: i32,
    pub omfi: u16,
    pub data_offset: u32,
}
#[derive(Debug, Serialize)]
pub struct Pcm {
    pub channels: u16,
    pub bits: u16,
    pub rate: DecimalRate,
    pub locked: bool,
    pub reference_level: i16,
    pub spatial_formulation: u32,
    pub dial_norm: u16,
    pub coding_format: u32,
    pub block_align: u32,
    pub sequence_offset: u16,
    pub average_bps: u32,
    pub peak_present: bool,
    pub peak_version: u32,
    pub peak_format: u32,
    pub points_per_peak: u32,
    pub peak_block_size: u32,
    pub peak_channels: u32,
    pub peak_frames: u32,
    pub peak_offset: u64,
    pub peak_timestamp_raw: u32,
}
impl<'a> Decode<'a, '_> {
    pub(super) fn blob(&mut self) -> Result<&'a [u8]> {
        self.expect(&[0x41])?;
        let size = self.u32()? as usize;
        self.c.take(size)
    }
    pub(super) fn boolean(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(bad("invalid boolean")),
        }
    }
    pub(super) fn utf8(&mut self) -> Result<Text<'a>> {
        self.expect(&[0x4c])?;
        let size = usize::from(self.u16()?);
        self.expect(&[0, 0])?;
        let raw = self.c.take(size.checked_sub(2).ok_or_else(|| bad("invalid UTF-8 field length"))?)?;
        let value = std::str::from_utf8(raw).map_err(|_| bad("malformed UTF-8 locator"))?.trim_end_matches('\0');
        Ok(Text { raw, value: value.into() })
    }
    pub(super) fn rate(&mut self) -> Result<DecimalRate> {
        let rate = DecimalRate { mantissa: self.i32()?, exponent: self.u16()? as i16 };
        rate.rational()?;
        Ok(rate)
    }
    pub(super) fn locator(&mut self, class: [u8; 4]) -> Result<Locator<'a>> {
        let mut out =
            Locator { path: None, posix: None, utf8: None, alternate_utf8: None, volume: None, volume_utf8: None, legacy_id_raw: None, id: None, domain: None };
        if class == *b"URLL" {
            return Ok(out);
        } // Only the observed empty locator is supported; EOF checked by caller.
        self.expect(&[2, 2])?;
        if class == *b"MSML" {
            out.legacy_id_raw = Some(self.c.take(8)?);
            out.volume = self.text()?;
        } else {
            out.path = self.text()?;
        }
        let mut seen = std::collections::BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            match (class, tag) {
                (c, 1) if c == *b"FILE" => {
                    self.expect(&[0x4c])?;
                    out.posix = self.text()?;
                }
                // WINF records the original imported source file, in the same shape as FILE's
                // own path fields. Only this UTF-8 extension is observed on it; FILE's POSIX and
                // alternate paths stay FILE-only rather than being assumed.
                (c, 2) if c == *b"FILE" || c == *b"WINF" => out.utf8 = Some(self.utf8()?),
                (c, 3) if c == *b"FILE" => out.alternate_utf8 = Some(self.utf8()?),
                (c, 1) if c == *b"MSML" => {
                    self.expect(&[0x47])?;
                    out.domain = Some(self.u32()?);
                }
                (c, 2) if c == *b"MSML" => out.id = Some(self.identity()?),
                (c, 3) if c == *b"MSML" => out.volume_utf8 = Some(self.utf8()?),
                _ => return Err(bad(format!("unsupported locator extension {tag}"))),
            }
        }
        Ok(out)
    }
    pub(super) fn bin_reference(&mut self) -> Result<BinReference<'a>> {
        self.expect(&[2, 1])?;
        let high = self.u32()?;
        let low = self.u32()?;
        let name = self.text()?;
        let mut utf8 = None;
        let mut seen = std::collections::BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            if tag != 1 {
                return Err(bad(format!("unsupported bin-reference extension {tag}")));
            }
            utf8 = Some(self.utf8()?);
        }
        Ok(BinReference { high, low, name, utf8 })
    }
    pub(super) fn descriptor(&mut self, class: [u8; 4]) -> Result<Descriptor<'a>> {
        self.expect(&[2, 3])?;
        let mob_kind = self.u8()?;
        let locator = self.reference()?;
        let intermediate = self.boolean()?;
        let physical_media = self.reference()?;
        let mut out = Descriptor {
            mob_kind,
            locator,
            intermediate,
            physical_media,
            uuid: None,
            plugin_raw: None,
            attributes: None,
            file: None,
            pcm: None,
            picture: None,
        };
        let mut seen = std::collections::BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            match tag {
                1 => {
                    let mut uuid: [u8; 16] = self.blob()?.try_into().map_err(|_| bad("invalid descriptor UUID width"))?;
                    if self.c.order == ByteOrder::Little {
                        for range in [0..4, 4..6, 6..8] {
                            uuid.get_mut(range).ok_or_else(|| bad("UUID bounds"))?.reverse();
                        }
                    }
                    out.uuid = Some(uuid);
                }
                2 => out.plugin_raw = Some(self.blob()?),
                3 => {
                    self.expect(&[0x48])?;
                    out.attributes = Some(self.reference()?);
                }
                _ => return Err(bad(format!("unsupported descriptor extension {tag}"))),
            }
        }
        if class == *b"MDES" {
            return Ok(out);
        }
        self.expect(&[2, 3])?;
        out.file = Some(FileDescriptor { rate: self.rate()?, length: self.i32()?, omfi: self.u16()?, data_offset: self.u32()? });
        if class == *b"CDCI" {
            out.picture = Some(self.picture()?);
            return Ok(out);
        }
        self.expect(&[2, 1])?;
        out.pcm = Some(Pcm {
            channels: self.u16()?,
            bits: self.u16()?,
            rate: self.rate()?,
            locked: self.boolean()?,
            reference_level: self.u16()? as i16,
            spatial_formulation: self.u32()?,
            dial_norm: self.u16()?,
            coding_format: self.u32()?,
            block_align: self.u32()?,
            sequence_offset: self.u16()?,
            average_bps: self.u32()?,
            peak_present: self.boolean()?,
            peak_version: self.u32()?,
            peak_format: self.u32()?,
            points_per_peak: self.u32()?,
            peak_block_size: self.u32()?,
            peak_channels: self.u32()?,
            peak_frames: self.u32()?,
            peak_offset: (u64::from(self.u32()?) << 32) | u64::from(self.u32()?),
            peak_timestamp_raw: self.u32()?,
        });
        Ok(out)
    }
}
