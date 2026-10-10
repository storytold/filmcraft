//! Class ids, property ids and definition ids of the AAF object model subset FilmCraft reads and
//! writes (AAF Object Specification v1.1, AAF Edit Protocol). Class and data definition AUIDs are
//! SMPTE labels (RP 224 / RP 210 registers) in AUID byte order; property ids are the AAF / ST 377-1
//! local tags.

use super::store::Auid;

/// AUID of a SMPTE universal label: the label's last 8 bytes become Data1–Data3 (stored
/// little-endian), its first 8 bytes Data4.
pub(crate) const fn ul(b: [u8; 16]) -> Auid {
    [b[11], b[10], b[9], b[8], b[13], b[12], b[15], b[14], b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
}

/// A GUID `{d1-d2-d3-d4}` in AUID byte order.
pub(crate) const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Auid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

/// Class AUID: SMPTE label 06.0E.2B.34.02.06.01.01.0D.01.01.01.01.01.xx.00.
pub(crate) const fn class(xx: u8) -> Auid {
    ul([0x06, 0x0E, 0x2B, 0x34, 0x02, 0x06, 0x01, 0x01, 0x0D, 0x01, 0x01, 0x01, 0x01, 0x01, xx, 0x00])
}

/// The root object's class (the storage class of the file's root).
pub(crate) const ROOT: Auid = guid(0xB3B398A5, 0x1C90, 0x11D4, [0x80, 0x53, 0x08, 0x00, 0x36, 0x21, 0x08, 0x04]);
/// The meta-dictionary (class 0x25 of the meta-model, 06.0E.2B.34.02.06.01.01.0D.01.01.01.02.25.00.00).
pub(crate) const META_DICTIONARY: Auid = ul([0x06, 0x0E, 0x2B, 0x34, 0x02, 0x06, 0x01, 0x01, 0x0D, 0x01, 0x01, 0x01, 0x02, 0x25, 0x00, 0x00]);

#[allow(dead_code)]
pub(crate) mod cls {
    use super::class;
    use crate::aaf::store::Auid;
    pub const COMMENT_MARKER: Auid = class(0x08);
    pub const FILLER: Auid = class(0x09);
    pub const OPERATION_GROUP: Auid = class(0x0A);
    pub const NESTED_SCOPE: Auid = class(0x0B);
    pub const SCOPE_REFERENCE: Auid = class(0x0D);
    pub const SELECTOR: Auid = class(0x0E);
    pub const SEQUENCE: Auid = class(0x0F);
    pub const SOURCE_CLIP: Auid = class(0x11);
    pub const TIMECODE: Auid = class(0x14);
    pub const TRANSITION: Auid = class(0x17);
    pub const CONTENT_STORAGE: Auid = class(0x18);
    pub const CONTROL_POINT: Auid = class(0x19);
    pub const DATA_DEFINITION: Auid = class(0x1B);
    pub const OPERATION_DEFINITION: Auid = class(0x1C);
    pub const PARAMETER_DEFINITION: Auid = class(0x1D);
    pub const CONTAINER_DEFINITION: Auid = class(0x20);
    pub const INTERPOLATION_DEFINITION: Auid = class(0x21);
    pub const DICTIONARY: Auid = class(0x22);
    pub const ESSENCE_DATA: Auid = class(0x23);
    pub const CDCI_DESCRIPTOR: Auid = class(0x28);
    pub const TAPE_DESCRIPTOR: Auid = class(0x2E);
    pub const HEADER: Auid = class(0x2F);
    pub const IDENTIFICATION: Auid = class(0x30);
    pub const NETWORK_LOCATOR: Auid = class(0x32);
    pub const COMPOSITION_MOB: Auid = class(0x35);
    pub const MASTER_MOB: Auid = class(0x36);
    pub const SOURCE_MOB: Auid = class(0x37);
    pub const EVENT_MOB_SLOT: Auid = class(0x39);
    pub const TIMELINE_MOB_SLOT: Auid = class(0x3B);
    pub const CONSTANT_VALUE: Auid = class(0x3D);
    pub const VARYING_VALUE: Auid = class(0x3E);
    pub const TAGGED_VALUE: Auid = class(0x3F);
    pub const DESCRIPTIVE_MARKER: Auid = class(0x41);
    pub const PCM_DESCRIPTOR: Auid = class(0x48);
    pub const IMPORT_DESCRIPTOR: Auid = class(0x4A);
    /// Classes with a `Locator` and media properties FilmCraft reads as file descriptors.
    pub const WAVE_DESCRIPTOR: Auid = class(0x2C);
    pub const AIFC_DESCRIPTOR: Auid = class(0x26);
    pub const SOUND_DESCRIPTOR: Auid = class(0x42);
    pub const RGBA_DESCRIPTOR: Auid = class(0x29);
    pub const MULTIPLE_DESCRIPTOR: Auid = class(0x44);
}

/// Property ids.
#[allow(dead_code)]
pub(crate) mod pid {
    // Root
    pub const ROOT_META_DICTIONARY: u16 = 0x0001;
    pub const ROOT_HEADER: u16 = 0x0002;
    // MetaDictionary and MetaDefinition
    pub const META_CLASS_DEFINITIONS: u16 = 0x0003;
    pub const META_TYPE_DEFINITIONS: u16 = 0x0004;
    pub const META_IDENTIFICATION: u16 = 0x0005;
    // Header
    pub const BYTE_ORDER: u16 = 0x3B01;
    pub const LAST_MODIFIED: u16 = 0x3B02;
    pub const CONTENT: u16 = 0x3B03;
    pub const DICTIONARY: u16 = 0x3B04;
    pub const VERSION: u16 = 0x3B05;
    pub const IDENTIFICATION_LIST: u16 = 0x3B06;
    pub const OBJECT_MODEL_VERSION: u16 = 0x3B07;
    pub const OPERATIONAL_PATTERN: u16 = 0x3B09;
    // Identification
    pub const COMPANY_NAME: u16 = 0x3C01;
    pub const PRODUCT_NAME: u16 = 0x3C02;
    pub const PRODUCT_VERSION_STRING: u16 = 0x3C04;
    pub const PRODUCT_ID: u16 = 0x3C05;
    pub const DATE: u16 = 0x3C06;
    pub const PLATFORM: u16 = 0x3C08;
    pub const GENERATION_AUID: u16 = 0x3C09;
    // ContentStorage
    pub const MOBS: u16 = 0x1901;
    pub const ESSENCE_DATA: u16 = 0x1902;
    // Dictionary
    pub const OPERATION_DEFINITIONS: u16 = 0x2603;
    pub const PARAMETER_DEFINITIONS: u16 = 0x2604;
    pub const DATA_DEFINITIONS: u16 = 0x2605;
    pub const CONTAINER_DEFINITIONS: u16 = 0x2608;
    pub const INTERPOLATION_DEFINITIONS: u16 = 0x2609;
    // DefinitionObject
    pub const IDENTIFICATION: u16 = 0x1B01;
    pub const NAME: u16 = 0x1B02;
    pub const DESCRIPTION: u16 = 0x1B03;
    // OperationDefinition
    pub const OPDEF_DATA_DEFINITION: u16 = 0x1E01;
    pub const OPDEF_IS_TIME_WARP: u16 = 0x1E02;
    pub const OPDEF_NUMBER_INPUTS: u16 = 0x1E07;
    pub const OPDEF_PARAMETERS_DEFINED: u16 = 0x1E09;
    // ParameterDefinition
    pub const PARAMDEF_DISPLAY_UNITS: u16 = 0x1F03;
    // Mob
    pub const MOB_ID: u16 = 0x4401;
    pub const MOB_NAME: u16 = 0x4402;
    pub const SLOTS: u16 = 0x4403;
    pub const MOB_LAST_MODIFIED: u16 = 0x4404;
    pub const MOB_CREATION_TIME: u16 = 0x4405;
    pub const MOB_USER_COMMENTS: u16 = 0x4406;
    pub const USAGE_CODE: u16 = 0x4408;
    // SourceMob
    pub const ESSENCE_DESCRIPTION: u16 = 0x4701;
    // MobSlot
    pub const SLOT_ID: u16 = 0x4801;
    pub const SLOT_NAME: u16 = 0x4802;
    pub const SEGMENT: u16 = 0x4803;
    pub const PHYSICAL_TRACK_NUMBER: u16 = 0x4804;
    // TimelineMobSlot / EventMobSlot
    pub const EDIT_RATE: u16 = 0x4B01;
    pub const ORIGIN: u16 = 0x4B02;
    pub const EVENT_EDIT_RATE: u16 = 0x4901;
    // Component
    pub const DATA_DEFINITION: u16 = 0x0201;
    pub const LENGTH: u16 = 0x0202;
    pub const COMPONENT_USER_COMMENTS: u16 = 0x0204;
    // Sequence
    pub const COMPONENTS: u16 = 0x1001;
    // SourceReference / SourceClip
    pub const SOURCE_ID: u16 = 0x1101;
    pub const SOURCE_MOB_SLOT_ID: u16 = 0x1102;
    pub const START_TIME: u16 = 0x1201;
    // Event
    pub const POSITION: u16 = 0x0601;
    pub const COMMENT: u16 = 0x0602;
    // Timecode
    pub const TC_START: u16 = 0x1501;
    pub const TC_FPS: u16 = 0x1502;
    pub const TC_DROP: u16 = 0x1503;
    // Transition
    pub const OPERATION_GROUP: u16 = 0x1801;
    pub const CUT_POINT: u16 = 0x1802;
    // OperationGroup
    pub const OPERATION: u16 = 0x0B01;
    pub const INPUT_SEGMENTS: u16 = 0x0B02;
    pub const PARAMETERS: u16 = 0x0B03;
    // Selector / NestedScope
    pub const SELECTED: u16 = 0x0F01;
    pub const NESTED_SLOTS: u16 = 0x0C01;
    // Parameter / ConstantValue / VaryingValue / ControlPoint
    pub const PARAMETER_DEFINITION: u16 = 0x4C01;
    pub const CONSTANT_VALUE: u16 = 0x4D01;
    pub const INTERPOLATION: u16 = 0x4E01;
    pub const POINT_LIST: u16 = 0x4E02;
    pub const CP_VALUE: u16 = 0x1A02;
    pub const CP_TIME: u16 = 0x1A03;
    pub const CP_EDIT_HINT: u16 = 0x1A04;
    // TaggedValue
    pub const TAG_NAME: u16 = 0x5001;
    pub const TAG_VALUE: u16 = 0x5003;
    // EssenceDescriptor / FileDescriptor
    pub const LOCATOR: u16 = 0x2F01;
    pub const SAMPLE_RATE: u16 = 0x3001;
    pub const FILE_LENGTH: u16 = 0x3002;
    pub const CONTAINER_FORMAT: u16 = 0x3004;
    pub const STORED_HEIGHT: u16 = 0x3202;
    pub const STORED_WIDTH: u16 = 0x3203;
    pub const FRAME_LAYOUT: u16 = 0x320C;
    pub const VIDEO_LINE_MAP: u16 = 0x320D;
    pub const IMAGE_ASPECT_RATIO: u16 = 0x320E;
    pub const COMPONENT_WIDTH: u16 = 0x3301;
    pub const HORIZONTAL_SUBSAMPLING: u16 = 0x3302;
    pub const QUANTIZATION_BITS: u16 = 0x3D01;
    pub const LOCKED: u16 = 0x3D02;
    pub const AUDIO_SAMPLING_RATE: u16 = 0x3D03;
    pub const CHANNELS: u16 = 0x3D07;
    pub const AVERAGE_BPS: u16 = 0x3D09;
    pub const BLOCK_ALIGN: u16 = 0x3D0A;
    // NetworkLocator
    pub const URL_STRING: u16 = 0x4001;
    // EssenceData
    pub const ESSENCE_MOB_ID: u16 = 0x2701;
    pub const ESSENCE_STREAM: u16 = 0x2702;
}

/// Data definitions (SMPTE RP 224 labels 06.0E.2B.34.04.01.01.01.01.03.02.xx.yy).
pub(crate) mod ddef {
    use super::{guid, ul};
    use crate::aaf::store::Auid;
    pub const PICTURE: Auid = ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x01, 0x00, 0x00, 0x00]);
    pub const SOUND: Auid = ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x02, 0x00, 0x00, 0x00]);
    pub const TIMECODE: Auid = ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00]);
    pub const DESCRIPTIVE_METADATA: Auid = ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x01, 0x10, 0x00, 0x00, 0x00]);
    /// Pre-SMPTE ("legacy") data definitions written by older applications.
    pub const LEGACY_PICTURE: Auid = guid(0x6F3C8CE1, 0x6CEF, 0x11D2, [0x80, 0x7D, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F]);
    pub const LEGACY_SOUND: Auid = guid(0x78E1EBE1, 0x6CEF, 0x11D2, [0x80, 0x7D, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F]);
    pub const LEGACY_TIMECODE: Auid = guid(0x7F275E81, 0x77E5, 0x11D2, [0x80, 0x7F, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F]);
}

/// Operation, parameter, interpolation and container definitions.
pub(crate) mod def {
    use super::guid;
    use crate::aaf::store::Auid;
    const AV: [u8; 8] = [0x8A, 0x09, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F];
    pub const VIDEO_DISSOLVE: Auid = guid(0x0C3BEA41, 0xFC05, 0x11D2, AV);
    pub const VIDEO_FADE_TO_BLACK: Auid = guid(0x0C3BEA40, 0xFC05, 0x11D2, AV);
    pub const SMPTE_VIDEO_WIPE: Auid = guid(0x0C3BEA43, 0xFC05, 0x11D2, AV);
    pub const MONO_AUDIO_DISSOLVE: Auid = guid(0x0C3BEA44, 0xFC05, 0x11D2, AV);
    const AG: [u8; 8] = [0x8A, 0x38, 0x00, 0x50, 0x04, 0x0E, 0xF7, 0xD2];
    pub const MONO_AUDIO_GAIN: Auid = guid(0x9D2EA893, 0x0968, 0x11D3, AG);
    const PD: [u8; 8] = [0x8A, 0x4C, 0x00, 0x50, 0x04, 0x0E, 0xF7, 0xD2];
    pub const PARAM_LEVEL: Auid = guid(0xE4962320, 0x2267, 0x11D3, PD);
    pub const PARAM_AMPLITUDE: Auid = guid(0xE4962321, 0x2267, 0x11D3, PD);
    const IP: [u8; 8] = [0x80, 0xA9, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F];
    pub const INTERP_LINEAR: Auid = guid(0x5B6C85A4, 0x0EDE, 0x11D3, IP);
    pub const INTERP_CONSTANT: Auid = guid(0x5B6C85A5, 0x0EDE, 0x11D3, IP);
    const CD: [u8; 8] = [0x80, 0x9B, 0x00, 0x60, 0x08, 0x14, 0x3E, 0x6F];
    pub const CONTAINER_AAF: Auid = guid(0x4313B571, 0xD8BA, 0x11D2, CD);
    pub const CONTAINER_EXTERNAL: Auid = guid(0x4313B572, 0xD8BA, 0x11D2, CD);
    /// Type ids of indirect values (SMPTE RP 210 type labels).
    pub const TYPE_RATIONAL: Auid = super::ul([0x06, 0x0E, 0x2B, 0x34, 0x01, 0x04, 0x01, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    pub const TYPE_STRING: Auid = super::ul([0x06, 0x0E, 0x2B, 0x34, 0x01, 0x04, 0x01, 0x01, 0x01, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00]);
    /// Edit Protocol operational pattern and mob usage codes.
    pub const OP_EDIT_PROTOCOL: Auid = super::ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x05, 0x0D, 0x01, 0x12, 0x01, 0x01, 0x00, 0x00, 0x00]);
    pub const USAGE_TOP_LEVEL: Auid = super::ul([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x05, 0x0D, 0x01, 0x12, 0x01, 0x02, 0x03, 0x00, 0x00]);
}

/// Weak reference target paths (pids from the root object).
pub(crate) mod path {
    use super::pid;
    pub const DATA_DEFS: [u16; 3] = [pid::ROOT_HEADER, pid::DICTIONARY, pid::DATA_DEFINITIONS];
    pub const OPERATION_DEFS: [u16; 3] = [pid::ROOT_HEADER, pid::DICTIONARY, pid::OPERATION_DEFINITIONS];
    pub const PARAMETER_DEFS: [u16; 3] = [pid::ROOT_HEADER, pid::DICTIONARY, pid::PARAMETER_DEFINITIONS];
    pub const INTERPOLATION_DEFS: [u16; 3] = [pid::ROOT_HEADER, pid::DICTIONARY, pid::INTERPOLATION_DEFINITIONS];
    pub const CONTAINER_DEFS: [u16; 3] = [pid::ROOT_HEADER, pid::DICTIONARY, pid::CONTAINER_DEFINITIONS];
}
