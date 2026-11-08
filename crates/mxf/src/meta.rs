//! Header metadata (ST 377-1 §9, Annex A/B): the primer pack and local sets.
//!
//! Local sets code each property as a 2-byte local tag and a 2-byte length. Tags below 0x8000 are
//! static (fixed by ST 377-1 and the mapping documents); dynamic tags (0x8000 and up) are mapped
//! to their property UL by the partition's primer pack.

use std::collections::HashMap;

use crate::klv::{Cur, Ul, batch_of, int_of, rational_of, ul_of, utf16_of};

/// Local tag → property UL (ST 377-1 §9.2).
#[derive(Clone, Debug, Default)]
pub struct Primer(pub HashMap<u16, Ul>);

impl Primer {
    pub fn parse(v: &[u8]) -> Primer {
        let mut map = HashMap::new();
        for item in batch_of(v) {
            if item.len() >= 18 {
                map.insert(u16::from_be_bytes([item[0], item[1]]), Ul(item[2..18].try_into().unwrap_or([0; 16])));
            }
        }
        Primer(map)
    }
}

/// Header metadata set types: byte 14 of `06 0E 2B 34 02 53 01 01 0D 01 01 01 01 01 tt 00`.
#[allow(dead_code)]
pub mod set_type {
    pub const PREFACE: u8 = 0x2F;
    pub const IDENTIFICATION: u8 = 0x30;
    pub const CONTENT_STORAGE: u8 = 0x18;
    pub const ESSENCE_CONTAINER_DATA: u8 = 0x23;
    pub const MATERIAL_PACKAGE: u8 = 0x36;
    pub const SOURCE_PACKAGE: u8 = 0x37;
    pub const TIMELINE_TRACK: u8 = 0x3B;
    pub const EVENT_TRACK: u8 = 0x39;
    pub const STATIC_TRACK: u8 = 0x3A;
    pub const SEQUENCE: u8 = 0x0F;
    pub const SOURCE_CLIP: u8 = 0x11;
    pub const TIMECODE_COMPONENT: u8 = 0x14;
    pub const FILLER: u8 = 0x09;
    pub const MULTIPLE_DESCRIPTOR: u8 = 0x44;
    pub const GENERIC_PICTURE_DESCRIPTOR: u8 = 0x27;
    pub const CDCI_DESCRIPTOR: u8 = 0x28;
    pub const RGBA_DESCRIPTOR: u8 = 0x29;
    pub const GENERIC_SOUND_DESCRIPTOR: u8 = 0x42;
    pub const GENERIC_DATA_DESCRIPTOR: u8 = 0x43;
    pub const AES3_DESCRIPTOR: u8 = 0x47;
    pub const WAVE_DESCRIPTOR: u8 = 0x48;
    pub const MPEG2_VIDEO_DESCRIPTOR: u8 = 0x51;
    pub const JPEG2000_SUBDESCRIPTOR: u8 = 0x5A;
    pub const AVC_SUBDESCRIPTOR: u8 = 0x6E;
}

/// One header metadata local set.
#[derive(Clone, Debug, Default)]
pub struct Set {
    pub key: Ul,
    pub instance_uid: [u8; 16],
    /// Static-tag properties.
    pub props: HashMap<u16, Vec<u8>>,
    /// Dynamic-tag properties, by property UL.
    pub dynamic: HashMap<Ul, Vec<u8>>,
}

impl Set {
    pub fn parse(key: Ul, v: &[u8], primer: &Primer) -> Set {
        let mut s = Set { key, ..Default::default() };
        let mut c = Cur::new(v);
        while c.remaining() >= 4 {
            let (Some(tag), Some(len)) = (c.u16(), c.u16()) else { break };
            let Some(val) = c.bytes(len as usize) else { break };
            if tag == 0x3C0A && val.len() >= 16 {
                s.instance_uid.copy_from_slice(&val[..16]);
            }
            if tag >= 0x8000 {
                if let Some(ul) = primer.0.get(&tag) {
                    s.dynamic.insert(*ul, val.to_vec());
                }
            } else {
                s.props.insert(tag, val.to_vec());
            }
        }
        s
    }

    /// The set type byte when this is a structural metadata set.
    pub fn kind(&self) -> Option<u8> {
        self.key.item_starts_with(&[0x0D, 0x01, 0x01, 0x01, 0x01, 0x01]).then_some(self.key.0[14])
    }
    pub fn get(&self, tag: u16) -> Option<&[u8]> {
        self.props.get(&tag).map(Vec::as_slice)
    }
    pub fn u(&self, tag: u16) -> Option<i64> {
        self.get(tag).and_then(|v| int_of(v, false))
    }
    pub fn i(&self, tag: u16) -> Option<i64> {
        self.get(tag).and_then(|v| int_of(v, true))
    }
    pub fn rational(&self, tag: u16) -> Option<crate::Rational> {
        self.get(tag).and_then(rational_of)
    }
    pub fn ul(&self, tag: u16) -> Option<Ul> {
        self.get(tag).and_then(ul_of)
    }
    pub fn string(&self, tag: u16) -> Option<String> {
        self.get(tag).map(utf16_of).filter(|s| !s.is_empty())
    }
    /// A strong / weak reference (16-byte UUID).
    pub fn reference(&self, tag: u16) -> Option<[u8; 16]> {
        self.get(tag).and_then(|v| v.get(..16)).map(|b| b.try_into().unwrap_or([0; 16]))
    }
    /// A batch of references.
    pub fn references(&self, tag: u16) -> Vec<[u8; 16]> {
        self.get(tag).map(|v| batch_of(v).into_iter().filter_map(|b| b.get(..16).map(|x| x.try_into().unwrap_or([0; 16]))).collect()).unwrap_or_default()
    }
    /// A dynamic property whose UL's item designator (bytes 8..) equals `item`.
    pub fn dynamic_item(&self, item: &[u8]) -> Option<&[u8]> {
        self.dynamic.iter().find(|(k, _)| k.item_starts_with(item)).map(|(_, v)| v.as_slice())
    }
}

/// All sets of one partition's header metadata, by instance UID.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub sets: Vec<Set>,
    by_uid: HashMap<[u8; 16], usize>,
}

impl Metadata {
    pub fn push(&mut self, s: Set) {
        if s.instance_uid != [0; 16] {
            self.by_uid.insert(s.instance_uid, self.sets.len());
        }
        self.sets.push(s);
    }
    pub fn get(&self, uid: &[u8; 16]) -> Option<&Set> {
        self.by_uid.get(uid).map(|&i| &self.sets[i])
    }
    pub fn of_kind(&self, kind: u8) -> impl Iterator<Item = &Set> {
        self.sets.iter().filter(move |s| s.kind() == Some(kind))
    }
}

/// Static local tags (ST 377-1 Annex B and the mapping documents).
#[allow(dead_code)]
pub mod tag {
    // Generic package
    pub const PACKAGE_UID: u16 = 0x4401;
    pub const PACKAGE_NAME: u16 = 0x4402;
    pub const PACKAGE_TRACKS: u16 = 0x4403;
    pub const SOURCE_PACKAGE_DESCRIPTOR: u16 = 0x4701;
    // Track
    pub const TRACK_ID: u16 = 0x4801;
    pub const TRACK_NUMBER: u16 = 0x4804;
    pub const TRACK_NAME: u16 = 0x4802;
    pub const TRACK_SEQUENCE: u16 = 0x4803;
    pub const EDIT_RATE: u16 = 0x4B01;
    pub const ORIGIN: u16 = 0x4B02;
    // Structural components
    pub const DATA_DEFINITION: u16 = 0x0201;
    pub const DURATION: u16 = 0x0202;
    pub const STRUCTURAL_COMPONENTS: u16 = 0x1001;
    pub const START_POSITION: u16 = 0x1201;
    pub const SOURCE_PACKAGE_ID: u16 = 0x1101;
    pub const SOURCE_TRACK_ID: u16 = 0x1102;
    pub const START_TIMECODE: u16 = 0x1501;
    pub const ROUNDED_TIMECODE_BASE: u16 = 0x1502;
    pub const DROP_FRAME: u16 = 0x1503;
    // Preface / identification / content storage
    pub const OPERATIONAL_PATTERN: u16 = 0x3B09;
    pub const PRODUCT_NAME: u16 = 0x3C02;
    pub const COMPANY_NAME: u16 = 0x3C01;
    pub const ESSENCE_CONTAINER_DATA_LINKED_PACKAGE: u16 = 0x2701;
    pub const INDEX_SID: u16 = 0x3F06;
    pub const BODY_SID: u16 = 0x3F07;
    // Descriptors
    pub const SUB_DESCRIPTORS: u16 = 0x3F01;
    pub const LINKED_TRACK_ID: u16 = 0x3006;
    pub const SAMPLE_RATE: u16 = 0x3001;
    pub const CONTAINER_DURATION: u16 = 0x3002;
    pub const ESSENCE_CONTAINER: u16 = 0x3004;
    pub const CODEC: u16 = 0x3005;
    pub const FRAME_LAYOUT: u16 = 0x320C;
    pub const FIELD_DOMINANCE: u16 = 0x3212;
    pub const STORED_WIDTH: u16 = 0x3203;
    pub const STORED_HEIGHT: u16 = 0x3202;
    pub const SAMPLED_WIDTH: u16 = 0x3205;
    pub const SAMPLED_HEIGHT: u16 = 0x3204;
    pub const DISPLAY_WIDTH: u16 = 0x3209;
    pub const DISPLAY_HEIGHT: u16 = 0x3208;
    pub const ASPECT_RATIO: u16 = 0x320E;
    pub const TRANSFER_CHARACTERISTIC: u16 = 0x3210;
    pub const PICTURE_ESSENCE_CODING: u16 = 0x3201;
    pub const CODING_EQUATIONS: u16 = 0x321A;
    pub const COLOR_PRIMARIES: u16 = 0x3219;
    pub const COMPONENT_DEPTH: u16 = 0x3301;
    pub const HORIZONTAL_SUBSAMPLING: u16 = 0x3302;
    pub const VERTICAL_SUBSAMPLING: u16 = 0x3308;
    pub const BLACK_REF_LEVEL: u16 = 0x3304;
    pub const WHITE_REF_LEVEL: u16 = 0x3305;
    pub const ALPHA_SAMPLE_DEPTH: u16 = 0x3309;
    pub const PIXEL_LAYOUT: u16 = 0x3401;
    pub const AUDIO_SAMPLING_RATE: u16 = 0x3D03;
    pub const LOCKED: u16 = 0x3D02;
    pub const CHANNEL_COUNT: u16 = 0x3D07;
    pub const QUANTIZATION_BITS: u16 = 0x3D01;
    pub const SOUND_ESSENCE_CODING: u16 = 0x3D06;
    pub const BLOCK_ALIGN: u16 = 0x3D0A;
}

/// The `SubDescriptors` property (ST 377-1:2009, dynamic tag): 06 0E 2B 34 01 01 01 09 06 01 01 04 06 10 00 00.
pub const SUB_DESCRIPTORS_ITEM: [u8; 8] = [0x06, 0x01, 0x01, 0x04, 0x06, 0x10, 0x00, 0x00];
