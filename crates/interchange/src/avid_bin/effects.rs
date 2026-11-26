//! Observed group wrappers and auxiliary components; decoding does not promise effect fidelity.
use super::{Component, Track, bad, semantic::Decode};
use crate::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum Field<'a> {
    Integer(i64),
    Boolean(bool),
    Ratio([i32; 2]),
    Float(f64),
    Reference(u32),
    Bytes(&'a [u8]),
}
#[derive(Debug, Serialize)]
pub struct GroupEffect<'a> {
    pub common: Component<'a>,
    pub class: [u8; 4],
    pub mode: u8,
    pub length: i32,
    pub tracks: Vec<Track>,
    pub fields: BTreeMap<&'static str, Field<'a>>,
}
impl<'a> Decode<'a, '_> {
    pub(super) fn effect(&mut self, common: Component<'a>, class: [u8; 4]) -> Result<GroupEffect<'a>> {
        let (mode, length, tracks) = self.group()?;
        let mut fields = BTreeMap::new();
        if class == *b"TKFX" {
            self.expect(&[2, 6])?;
            for key in ["left_length", "right_length", "info_version", "info_current"] {
                fields.insert(key, Field::Integer(i64::from(self.i32()?)));
            }
            for key in ["info_smooth", "info_color_item", "info_quality"] {
                fields.insert(key, Field::Integer(i64::from(self.u16()?)));
            }
            fields.insert("info_is_reversed", Field::Integer(i64::from(self.u8()?)));
            fields.insert("info_aspect_on", Field::Boolean(self.boolean()?));
            fields.insert("keyframes", Field::Reference(self.reference()?));
            fields.insert("info_force_software", Field::Boolean(self.boolean()?));
            fields.insert("info_never_hardware", Field::Boolean(self.boolean()?));
            let mut seen = BTreeSet::new();
            while let Some(tag) = self.extension(&mut seen)? {
                if tag != 2 {
                    return Err(bad(format!("unsupported track-effect extension {tag}")));
                }
                self.expect(&[0x48])?;
                fields.insert("trackman", Field::Reference(self.reference()?));
            }
        } else {
            self.expect(&[2, 2])?;
            fields.insert("phase_offset", Field::Integer(i64::from(self.i32()?)));
            if class == *b"MASK" {
                self.expect(&[2, 1])?;
                fields.insert("is_double", Field::Boolean(self.boolean()?));
                fields.insert("mask_bits", Field::Integer(i64::from(self.u32()?)));
            } else {
                self.expect(&[2, 3])?;
                fields.insert("speed_ratio", Field::Ratio([self.i32()?, self.i32()?]));
                let mut seen = BTreeSet::new();
                while let Some(tag) = self.extension(&mut seen)? {
                    match tag {
                        1 => {
                            self.expect(&[0x4b])?;
                            let v = f64::from_bits(self.c.number(8)?);
                            if !v.is_finite() {
                                return Err(bad("non-finite speed adjustment"));
                            }
                            fields.insert("offset_adjust", Field::Float(v));
                        }
                        2 => {
                            self.expect(&[0x48])?;
                            fields.insert("source_param_list", Field::Reference(self.reference()?));
                        }
                        3 => {
                            self.expect(&[0x42])?;
                            fields.insert("new_source_calculation", Field::Boolean(self.boolean()?));
                        }
                        _ => return Err(bad(format!("unsupported motion extension {tag}"))),
                    }
                }
            }
        }
        Ok(GroupEffect { common, class, mode, length, tracks, fields })
    }
    pub(super) fn auxiliary(&mut self, class: [u8; 4]) -> Result<BTreeMap<&'static str, Field<'a>>> {
        self.expect(&[2, 1])?;
        let mut fields = BTreeMap::new();
        if class == *b"TRKR" {
            for key in ["relative_scope", "relative_track"] {
                fields.insert(key, Field::Integer(i64::from(self.u16()? as i16)));
            }
        } else {
            fields.insert("header", Field::Bytes(self.c.take(8)?));
            fields.insert("film_kind", Field::Integer(i64::from(self.u8()?)));
            fields.insert("code_format", Field::Integer(i64::from(self.u8()?)));
            fields.insert("base_perf", Field::Integer(i64::from(self.u16()?)));
            fields.insert("start_ec", Field::Integer((i64::from(self.i32()?) << 32) | i64::from(self.u32()?)));
        }
        Ok(fields)
    }
}
