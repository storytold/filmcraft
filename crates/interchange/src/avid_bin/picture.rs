//! The observed CDCI picture descriptor. Integer fields stay exact; labels retain raw bytes.
use super::{bad, semantic::Decode};
use crate::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Named integer descriptor metadata, plus explicitly typed arrays/flags/labels. No render policy.
#[derive(Debug, Serialize)]
pub struct Picture<'a> {
    pub integers: BTreeMap<&'static str, i64>,
    pub booleans: BTreeMap<&'static str, bool>,
    pub labels: BTreeMap<&'static str, &'a [u8]>,
    pub aspect: [i32; 2],
    pub line_map: Vec<i32>,
    pub compression: [u8; 4],
    pub next: u32,
    pub boxes: Option<[[[i32; 2]; 4]; 3]>,
    pub frame_index_offset: u64,
}
impl<'a> Decode<'a, '_> {
    pub(super) fn picture(&mut self) -> Result<Picture<'a>> {
        self.expect(&[2, 2])?;
        let mut p = Picture {
            integers: BTreeMap::new(),
            booleans: BTreeMap::new(),
            labels: BTreeMap::new(),
            aspect: [0; 2],
            line_map: Vec::new(),
            compression: [0; 4],
            next: 0,
            boxes: None,
            frame_index_offset: 0,
        };
        for key in [
            "stored_height",
            "stored_width",
            "sampled_height",
            "sampled_width",
            "sampled_x_offset",
            "sampled_y_offset",
            "display_height",
            "display_width",
            "display_x_offset",
            "display_y_offset",
        ] {
            p.integers.insert(key, i64::from(self.i32()?));
        }
        p.integers.insert("frame_layout", i64::from(self.u16()?));
        p.aspect = [self.i32()?, self.i32()?];
        let size = self.u32()? as usize;
        if !size.is_multiple_of(4) || size > 4096 {
            return Err(bad("invalid/excessive picture line map"));
        }
        for _ in 0..size / 4 {
            p.line_map.push(self.i32()?);
        }
        p.integers.insert("alpha_transparency", i64::from(self.u32()?));
        p.booleans.insert("uniformness", self.boolean()?);
        p.integers.insert("did_image_size", i64::from(self.u32()?));
        p.next = self.reference()?;
        p.compression = self.c.tag()?;
        p.integers.insert("resolution_id", i64::from(self.u32()?));
        p.integers.insert("image_alignment_factor", i64::from(self.u32()?));
        let mut seen = BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            match tag {
                1 => {
                    self.expect(&[0x45])?;
                    p.integers.insert("frame_index_byte_order", i64::from(self.u16()?));
                }
                2 | 3 | 5 => {
                    let key = match tag {
                        2 => "frame_sample_size",
                        3 => "first_frame_offset",
                        _ => "offset_to_rle_frame_index",
                    };
                    self.expect(&[0x47])?;
                    p.integers.insert(key, i64::from(self.i32()?));
                }
                4 => {
                    for key in ["client_fill_start", "client_fill_end"] {
                        self.expect(&[0x47])?;
                        p.integers.insert(key, i64::from(self.i32()?));
                    }
                }
                8 => {
                    let mut boxes = [[[0; 2]; 4]; 3];
                    for rectangle in &mut boxes {
                        for point in rectangle {
                            for value in point {
                                self.expect(&[0x47])?;
                                *value = self.i32()?;
                            }
                        }
                    }
                    p.boxes = Some(boxes);
                }
                10 => {
                    self.expect(&[0x50])?;
                    p.labels.insert("transfer_characteristic", self.c.take(16)?);
                }
                11 => {
                    for key in ["color_primaries", "coding_equations"] {
                        self.expect(&[0x50])?;
                        p.labels.insert(key, self.c.take(16)?);
                    }
                }
                12 => {
                    self.expect(&[0x50])?;
                    p.labels.insert("essence_compression", self.c.take(16)?);
                }
                15 => {
                    self.expect(&[0x42])?;
                    p.booleans.insert("frame_checked_with_mapper", self.boolean()?);
                }
                _ => return Err(bad(format!("unsupported picture extension {tag}"))),
            }
        }
        self.expect(&[2, 2])?;
        for key in ["horizontal_subsampling", "vertical_subsampling", "component_width"] {
            p.integers.insert(key, i64::from(self.u32()?));
        }
        p.integers.insert("color_sitting", i64::from(self.u16()?));
        for key in ["black_ref_level", "white_ref_level", "color_range"] {
            p.integers.insert(key, i64::from(self.u32()?));
        }
        p.frame_index_offset = self.c.number(8)?;
        let mut seen = BTreeSet::new();
        while let Some(tag) = self.extension(&mut seen)? {
            let key = match tag {
                1 => "alpha_sampled_width",
                2 => "ignore_bw",
                _ => return Err(bad(format!("unsupported CDCI extension {tag}"))),
            };
            self.expect(&[0x48])?;
            p.integers.insert(key, i64::from(self.u32()?));
        }
        Ok(p)
    }
}
