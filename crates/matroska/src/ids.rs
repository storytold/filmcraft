//! EBML / Matroska element IDs (RFC 8794, RFC 9559). IDs keep their VINT marker bits.
#![allow(dead_code)]

// EBML header + global elements
pub const EBML: u32 = 0x1A45_DFA3;
pub const EBML_VERSION: u32 = 0x4286;
pub const EBML_READ_VERSION: u32 = 0x42F7;
pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
pub const DOC_TYPE: u32 = 0x4282;
pub const DOC_TYPE_VERSION: u32 = 0x4287;
pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
pub const CRC32: u32 = 0xBF;
pub const VOID: u32 = 0xEC;

// Segment and level-1 elements
pub const SEGMENT: u32 = 0x1853_8067;
pub const SEEK_HEAD: u32 = 0x114D_9B74;
pub const INFO: u32 = 0x1549_A966;
pub const TRACKS: u32 = 0x1654_AE6B;
pub const CLUSTER: u32 = 0x1F43_B675;
pub const CUES: u32 = 0x1C53_BB6B;
pub const ATTACHMENTS: u32 = 0x1941_A469;
pub const CHAPTERS: u32 = 0x1043_A770;
pub const TAGS: u32 = 0x1254_C367;

/// True for IDs that can only appear directly inside a Segment (or are the EBML/Segment headers):
/// used to find the end of unknown-size elements.
pub fn is_top_level(id: u32) -> bool {
    matches!(id, EBML | SEGMENT | SEEK_HEAD | INFO | TRACKS | CLUSTER | CUES | ATTACHMENTS | CHAPTERS | TAGS)
}

// SeekHead
pub const SEEK: u32 = 0x4DBB;
pub const SEEK_ID: u32 = 0x53AB;
pub const SEEK_POSITION: u32 = 0x53AC;

// Info
pub const SEGMENT_UUID: u32 = 0x73A4;
pub const SEGMENT_FILENAME: u32 = 0x7384;
pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
pub const DURATION: u32 = 0x4489;
pub const DATE_UTC: u32 = 0x4461;
pub const TITLE: u32 = 0x7BA9;
pub const MUXING_APP: u32 = 0x4D80;
pub const WRITING_APP: u32 = 0x5741;

// Cluster
pub const TIMESTAMP: u32 = 0xE7;
pub const POSITION: u32 = 0xA7;
pub const PREV_SIZE: u32 = 0xAB;
pub const SIMPLE_BLOCK: u32 = 0xA3;
pub const BLOCK_GROUP: u32 = 0xA0;
pub const ENCRYPTED_BLOCK: u32 = 0xAF;
pub const SILENT_TRACKS: u32 = 0x5854;
pub const BLOCK: u32 = 0xA1;
pub const BLOCK_ADDITIONS: u32 = 0x75A1;
pub const BLOCK_MORE: u32 = 0xA6;
pub const BLOCK_ADD_ID: u32 = 0xEE;
pub const BLOCK_ADDITIONAL: u32 = 0xA5;
pub const BLOCK_DURATION: u32 = 0x9B;
pub const REFERENCE_PRIORITY: u32 = 0xFA;
pub const REFERENCE_BLOCK: u32 = 0xFB;
pub const CODEC_STATE: u32 = 0xA4;
pub const DISCARD_PADDING: u32 = 0x75A2;

pub fn is_cluster_child(id: u32) -> bool {
    matches!(id, TIMESTAMP | POSITION | PREV_SIZE | SIMPLE_BLOCK | BLOCK_GROUP | ENCRYPTED_BLOCK | SILENT_TRACKS | CRC32 | VOID)
}

// Tracks
pub const TRACK_ENTRY: u32 = 0xAE;
pub const TRACK_NUMBER: u32 = 0xD7;
pub const TRACK_UID: u32 = 0x73C5;
pub const TRACK_TYPE: u32 = 0x83;
pub const FLAG_ENABLED: u32 = 0xB9;
pub const FLAG_DEFAULT: u32 = 0x88;
pub const FLAG_FORCED: u32 = 0x55AA;
pub const FLAG_HEARING_IMPAIRED: u32 = 0x55AB;
pub const FLAG_VISUAL_IMPAIRED: u32 = 0x55AC;
pub const FLAG_ORIGINAL: u32 = 0x55AE;
pub const FLAG_COMMENTARY: u32 = 0x55AF;
pub const FLAG_LACING: u32 = 0x9C;
pub const DEFAULT_DURATION: u32 = 0x23_E383;
pub const TRACK_TIMESTAMP_SCALE: u32 = 0x23_314F;
pub const NAME: u32 = 0x536E;
pub const LANGUAGE: u32 = 0x22_B59C;
pub const LANGUAGE_BCP47: u32 = 0x22_B59D;
pub const CODEC_ID: u32 = 0x86;
pub const CODEC_PRIVATE: u32 = 0x63A2;
pub const CODEC_NAME: u32 = 0x25_8688;
pub const CODEC_DELAY: u32 = 0x56AA;
pub const SEEK_PRE_ROLL: u32 = 0x56BB;

// Video
pub const VIDEO: u32 = 0xE0;
pub const FLAG_INTERLACED: u32 = 0x9A;
pub const FIELD_ORDER: u32 = 0x9D;
pub const STEREO_MODE: u32 = 0x53B8;
pub const ALPHA_MODE: u32 = 0x53C0;
pub const PIXEL_WIDTH: u32 = 0xB0;
pub const PIXEL_HEIGHT: u32 = 0xBA;
pub const PIXEL_CROP_BOTTOM: u32 = 0x54AA;
pub const PIXEL_CROP_TOP: u32 = 0x54BB;
pub const PIXEL_CROP_LEFT: u32 = 0x54CC;
pub const PIXEL_CROP_RIGHT: u32 = 0x54DD;
pub const DISPLAY_WIDTH: u32 = 0x54B0;
pub const DISPLAY_HEIGHT: u32 = 0x54BA;
pub const DISPLAY_UNIT: u32 = 0x54B2;
pub const COLOUR_SPACE: u32 = 0x2E_B524;
pub const COLOUR: u32 = 0x55B0;
pub const PROJECTION: u32 = 0x7670;
pub const PROJECTION_TYPE: u32 = 0x7671;
pub const PROJECTION_POSE_YAW: u32 = 0x7673;
pub const PROJECTION_POSE_PITCH: u32 = 0x7674;
pub const PROJECTION_POSE_ROLL: u32 = 0x7675;

// Colour
pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
pub const BITS_PER_CHANNEL: u32 = 0x55B2;
pub const CHROMA_SUBSAMPLING_HORZ: u32 = 0x55B3;
pub const CHROMA_SUBSAMPLING_VERT: u32 = 0x55B4;
pub const CB_SUBSAMPLING_HORZ: u32 = 0x55B5;
pub const CB_SUBSAMPLING_VERT: u32 = 0x55B6;
pub const CHROMA_SITING_HORZ: u32 = 0x55B7;
pub const CHROMA_SITING_VERT: u32 = 0x55B8;
pub const RANGE: u32 = 0x55B9;
pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
pub const PRIMARIES: u32 = 0x55BB;
pub const MAX_CLL: u32 = 0x55BC;
pub const MAX_FALL: u32 = 0x55BD;
pub const MASTERING_METADATA: u32 = 0x55D0;
pub const PRIMARY_R_X: u32 = 0x55D1;
pub const PRIMARY_R_Y: u32 = 0x55D2;
pub const PRIMARY_G_X: u32 = 0x55D3;
pub const PRIMARY_G_Y: u32 = 0x55D4;
pub const PRIMARY_B_X: u32 = 0x55D5;
pub const PRIMARY_B_Y: u32 = 0x55D6;
pub const WHITE_POINT_X: u32 = 0x55D7;
pub const WHITE_POINT_Y: u32 = 0x55D8;
pub const LUMINANCE_MAX: u32 = 0x55D9;
pub const LUMINANCE_MIN: u32 = 0x55DA;

// Audio
pub const AUDIO: u32 = 0xE1;
pub const SAMPLING_FREQUENCY: u32 = 0xB5;
pub const OUTPUT_SAMPLING_FREQUENCY: u32 = 0x78B5;
pub const CHANNELS: u32 = 0x9F;
pub const BIT_DEPTH: u32 = 0x6264;

// ContentEncodings
pub const CONTENT_ENCODINGS: u32 = 0x6D80;
pub const CONTENT_ENCODING: u32 = 0x6240;
pub const CONTENT_ENCODING_ORDER: u32 = 0x5031;
pub const CONTENT_ENCODING_SCOPE: u32 = 0x5032;
pub const CONTENT_ENCODING_TYPE: u32 = 0x5033;
pub const CONTENT_COMPRESSION: u32 = 0x5034;
pub const CONTENT_COMP_ALGO: u32 = 0x4254;
pub const CONTENT_COMP_SETTINGS: u32 = 0x4255;
pub const CONTENT_ENCRYPTION: u32 = 0x5035;

// Cues
pub const CUE_POINT: u32 = 0xBB;
pub const CUE_TIME: u32 = 0xB3;
pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
pub const CUE_TRACK: u32 = 0xF7;
pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
pub const CUE_RELATIVE_POSITION: u32 = 0xF0;
pub const CUE_DURATION: u32 = 0xB2;
pub const CUE_BLOCK_NUMBER: u32 = 0x5378;

// Attachments
pub const ATTACHED_FILE: u32 = 0x61A7;
pub const FILE_DESCRIPTION: u32 = 0x467E;
pub const FILE_NAME: u32 = 0x466E;
pub const FILE_MEDIA_TYPE: u32 = 0x4660;
pub const FILE_DATA: u32 = 0x465C;
pub const FILE_UID: u32 = 0x46AE;

// Chapters
pub const EDITION_ENTRY: u32 = 0x45B9;
pub const EDITION_UID: u32 = 0x45BC;
pub const EDITION_FLAG_HIDDEN: u32 = 0x45BD;
pub const EDITION_FLAG_DEFAULT: u32 = 0x45DB;
pub const EDITION_FLAG_ORDERED: u32 = 0x45DD;
pub const CHAPTER_ATOM: u32 = 0xB6;
pub const CHAPTER_UID: u32 = 0x73C4;
pub const CHAPTER_STRING_UID: u32 = 0x5654;
pub const CHAPTER_TIME_START: u32 = 0x91;
pub const CHAPTER_TIME_END: u32 = 0x92;
pub const CHAPTER_FLAG_HIDDEN: u32 = 0x98;
pub const CHAPTER_FLAG_ENABLED: u32 = 0x4598;
pub const CHAPTER_DISPLAY: u32 = 0x80;
pub const CHAP_STRING: u32 = 0x85;
pub const CHAP_LANGUAGE: u32 = 0x437C;
pub const CHAP_LANGUAGE_BCP47: u32 = 0x437D;
pub const CHAP_COUNTRY: u32 = 0x437E;

// Tags
pub const TAG: u32 = 0x7373;
pub const TARGETS: u32 = 0x63C0;
pub const TARGET_TYPE_VALUE: u32 = 0x68CA;
pub const TARGET_TYPE: u32 = 0x63CA;
pub const TAG_TRACK_UID: u32 = 0x63C5;
pub const TAG_EDITION_UID: u32 = 0x63C9;
pub const TAG_CHAPTER_UID: u32 = 0x63C4;
pub const TAG_ATTACHMENT_UID: u32 = 0x63C6;
pub const SIMPLE_TAG: u32 = 0x67C8;
pub const TAG_NAME: u32 = 0x45A3;
pub const TAG_LANGUAGE: u32 = 0x447A;
pub const TAG_LANGUAGE_BCP47: u32 = 0x447B;
pub const TAG_DEFAULT: u32 = 0x4484;
pub const TAG_STRING: u32 = 0x4487;
pub const TAG_BINARY: u32 = 0x4485;
