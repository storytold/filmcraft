// The declarations in this file are transcribed from libva's `va/va.h`, `va/va_dec_hevc.h` and `va/va_drm.h`
// (VA-API 1.23, libva 2.23), which carry this notice:
//
// Copyright (c) 2007-2009 Intel Corporation. All Rights Reserved.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sub license, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to
// the following conditions:
//
// The above copyright notice and this permission notice (including the
// next paragraph) shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NON-INFRINGEMENT.
// IN NO EVENT SHALL INTEL AND/OR ITS SUPPLIERS BE LIABLE FOR
// ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
// TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
// SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

//! VA-API (`va.h`, `va_dec_hevc.h`, `va_drm.h`; MIT-licensed headers by Intel) as `libva.so.2` /
//! `libva-drm.so.2` expect it: the H.264 and HEVC decode buffers this backend fills in, the image structures it reads back
//! through, and the entry points it calls. Written from the public header; sizes and field offsets
//! are checked against a C compiler's by `abi_tests.rs`.
//!
//! The structures are plain data and compile on every target, so the code that fills them in
//! (`vaapi::h264`) is tested everywhere; only `vaapi::va` loads the library (Linux).
//!
//! FFI module (docs/adr/0001-platform-ffi.md): plain `repr(C)` data, no logic.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]

use std::ffi::{c_char, c_int, c_uint, c_void};

pub type VADisplay = *mut c_void;
pub type VAStatus = c_int;
pub type VAGenericID = c_uint;
pub type VAConfigID = VAGenericID;
pub type VAContextID = VAGenericID;
pub type VASurfaceID = VAGenericID;
pub type VABufferID = VAGenericID;
pub type VAImageID = VAGenericID;
/// `VAProfile` (a C enum: `int`).
pub type VAProfile = c_int;
/// `VAEntrypoint` (a C enum: `int`).
pub type VAEntrypoint = c_int;
/// `VABufferType` (a C enum: `int`).
pub type VABufferType = c_int;
/// `VAConfigAttribType` (a C enum: `int`).
pub type VAConfigAttribType = c_int;
pub type VAMessageCallback = Option<unsafe extern "C" fn(user_context: *mut c_void, message: *const c_char)>;

pub const VA_STATUS_SUCCESS: VAStatus = 0;
pub const VA_STATUS_ERROR_UNSUPPORTED_PROFILE: VAStatus = 0x0c;
pub const VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT: VAStatus = 0x0d;
pub const VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT: VAStatus = 0x0e;
pub const VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED: VAStatus = 0x13;
pub const VA_STATUS_ERROR_DECODING_ERROR: VAStatus = 0x17;

pub const VA_INVALID_ID: VAGenericID = 0xffff_ffff;
pub const VA_INVALID_SURFACE: VASurfaceID = VA_INVALID_ID;

pub const VAProfileH264Main: VAProfile = 6;
pub const VAProfileH264High: VAProfile = 7;
pub const VAProfileH264ConstrainedBaseline: VAProfile = 13;
pub const VAProfileHEVCMain: VAProfile = 17;
pub const VAProfileHEVCMain10: VAProfile = 18;
pub const VAEntrypointVLD: VAEntrypoint = 1;

pub const VAConfigAttribRTFormat: VAConfigAttribType = 0;
pub const VA_RT_FORMAT_YUV420: c_uint = 0x0000_0001;
pub const VA_RT_FORMAT_YUV420_10: c_uint = 0x0000_0100;

/// `vaCreateContext` flag: progressive pictures only.
pub const VA_PROGRESSIVE: c_int = 0x1;

pub const VAPictureParameterBufferType: VABufferType = 0;
pub const VAIQMatrixBufferType: VABufferType = 1;
pub const VASliceParameterBufferType: VABufferType = 4;
pub const VASliceDataBufferType: VABufferType = 5;

/// `VASliceParameterBuffer*::slice_data_flag`: the whole slice is in the buffer.
pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0x00;

pub const VA_PICTURE_H264_INVALID: u32 = 0x0000_0001;
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x0000_0008;
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x0000_0010;

pub const VA_FOURCC_NV12: u32 = 0x3231_564E;
pub const VA_FOURCC_P010: u32 = 0x3031_3050;

pub const VA_PICTURE_HEVC_INVALID: u32 = 0x0000_0001;
pub const VA_PICTURE_HEVC_LONG_TERM_REFERENCE: u32 = 0x0000_0008;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 0x0000_0010;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_AFTER: u32 = 0x0000_0020;
pub const VA_PICTURE_HEVC_RPS_LT_CURR: u32 = 0x0000_0040;

pub const VA_PADDING_LOW: usize = 4;
pub const VA_PADDING_MEDIUM: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAConfigAttrib {
    pub type_: VAConfigAttribType,
    pub value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VAPictureH264 {
    pub picture_id: VASurfaceID,
    pub frame_idx: u32,
    pub flags: u32,
    pub TopFieldOrderCnt: i32,
    pub BottomFieldOrderCnt: i32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

impl VAPictureH264 {
    /// An unused entry (`VA_INVALID_SURFACE`, `VA_PICTURE_H264_INVALID`).
    pub const INVALID: Self = Self {
        picture_id: VA_INVALID_SURFACE,
        frame_idx: 0,
        flags: VA_PICTURE_H264_INVALID,
        TopFieldOrderCnt: 0,
        BottomFieldOrderCnt: 0,
        va_reserved: [0; VA_PADDING_LOW],
    };
}

/// `VAPictureParameterBufferH264`. `seq_fields` and `pic_fields` are C bit-field unions, stored
/// here as their `value` word (bits allocated from the least significant end, as GCC and Clang do
/// on the little-endian targets libva runs on).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16],
    pub picture_width_in_mbs_minus1: u16,
    pub picture_height_in_mbs_minus1: u16,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames: u8,
    pub seq_fields: u32,
    pub num_slice_groups_minus1: u8,
    pub slice_group_map_type: u8,
    pub slice_group_change_rate_minus1: u16,
    pub pic_init_qp_minus26: i8,
    pub pic_init_qs_minus26: i8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: u32,
    pub frame_num: u16,
    pub va_reserved: [u32; VA_PADDING_MEDIUM],
}

/// `seq_fields` bit positions (`chroma_format_idc` 2 bits, the log2 fields 4, `pic_order_cnt_type` 2).
pub mod seq_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const RESIDUAL_COLOUR_TRANSFORM: u32 = 2;
    pub const GAPS_IN_FRAME_NUM_ALLOWED: u32 = 3;
    pub const FRAME_MBS_ONLY: u32 = 4;
    pub const MB_ADAPTIVE_FRAME_FIELD: u32 = 5;
    pub const DIRECT_8X8_INFERENCE: u32 = 6;
    pub const MIN_LUMA_BI_PRED_SIZE_8X8: u32 = 7;
    pub const LOG2_MAX_FRAME_NUM_MINUS4: u32 = 8;
    pub const PIC_ORDER_CNT_TYPE: u32 = 12;
    pub const LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4: u32 = 14;
    pub const DELTA_PIC_ORDER_ALWAYS_ZERO: u32 = 18;
}

/// `pic_fields` bit positions (`weighted_bipred_idc` 2 bits).
pub mod pic_bits {
    pub const ENTROPY_CODING_MODE: u32 = 0;
    pub const WEIGHTED_PRED: u32 = 1;
    pub const WEIGHTED_BIPRED_IDC: u32 = 2;
    pub const TRANSFORM_8X8_MODE: u32 = 4;
    pub const FIELD_PIC: u32 = 5;
    pub const CONSTRAINED_INTRA_PRED: u32 = 6;
    pub const PIC_ORDER_PRESENT: u32 = 7;
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 8;
    pub const REDUNDANT_PIC_CNT_PRESENT: u32 = 9;
    pub const REFERENCE_PIC: u32 = 10;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAIQMatrixBufferH264 {
    /// 4x4 scaling lists in raster scan order: Intra Y, Cb, Cr, Inter Y, Cb, Cr.
    pub ScalingList4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists in raster scan order: Intra Y, Inter Y.
    pub ScalingList8x8: [[u8; 64]; 2],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VASliceParameterBufferH264 {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    /// Bits from the start of the NAL unit (its header byte included) to `slice_data()`, counted
    /// without emulation prevention bytes; the data buffer itself keeps them.
    pub slice_data_bit_offset: u16,
    pub first_mb_in_slice: u16,
    pub slice_type: u8,
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub RefPicList0: [VAPictureH264; 32],
    pub RefPicList1: [VAPictureH264; 32],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i16; 32],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i16; 2]; 32],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i16; 2]; 32],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VAPictureHEVC {
    pub picture_id: VASurfaceID,
    pub pic_order_cnt: i32,
    pub flags: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

impl VAPictureHEVC {
    /// An unused entry (`VA_INVALID_SURFACE`, `VA_PICTURE_HEVC_INVALID`).
    pub const INVALID: Self = Self { picture_id: VA_INVALID_SURFACE, pic_order_cnt: 0, flags: VA_PICTURE_HEVC_INVALID, va_reserved: [0; VA_PADDING_LOW] };
}

/// `VAPictureParameterBufferHEVC`; `pic_fields` and `slice_parsing_fields` are C bit-field unions
/// stored as their `value` word (see [`VAPictureParameterBufferH264`]).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAPictureParameterBufferHEVC {
    pub CurrPic: VAPictureHEVC,
    pub ReferenceFrames: [VAPictureHEVC; 15],
    pub pic_width_in_luma_samples: u16,
    pub pic_height_in_luma_samples: u16,
    pub pic_fields: u32,
    pub sps_max_dec_pic_buffering_minus1: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub pcm_sample_bit_depth_luma_minus1: u8,
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_transform_block_size_minus2: u8,
    pub log2_diff_max_min_transform_block_size: u8,
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub init_qp_minus26: i8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u16; 19],
    pub row_height_minus1: [u16; 21],
    pub slice_parsing_fields: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pic_sps: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub num_extra_slice_header_bits: u8,
    /// Bits of `short_term_ref_pic_set()` in the slice header (0 when the SPS's set is used),
    /// counted without emulation prevention bytes.
    pub st_rps_bits: u32,
    pub va_reserved: [u32; VA_PADDING_MEDIUM],
}

/// `VAPictureParameterBufferHEVC::pic_fields` bit positions (`chroma_format_idc` 2 bits).
pub mod hevc_pic_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const SEPARATE_COLOUR_PLANE: u32 = 2;
    pub const PCM_ENABLED: u32 = 3;
    pub const SCALING_LIST_ENABLED: u32 = 4;
    pub const TRANSFORM_SKIP_ENABLED: u32 = 5;
    pub const AMP_ENABLED: u32 = 6;
    pub const STRONG_INTRA_SMOOTHING_ENABLED: u32 = 7;
    pub const SIGN_DATA_HIDING_ENABLED: u32 = 8;
    pub const CONSTRAINED_INTRA_PRED: u32 = 9;
    pub const CU_QP_DELTA_ENABLED: u32 = 10;
    pub const WEIGHTED_PRED: u32 = 11;
    pub const WEIGHTED_BIPRED: u32 = 12;
    pub const TRANSQUANT_BYPASS_ENABLED: u32 = 13;
    pub const TILES_ENABLED: u32 = 14;
    pub const ENTROPY_CODING_SYNC_ENABLED: u32 = 15;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 16;
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED: u32 = 17;
    pub const PCM_LOOP_FILTER_DISABLED: u32 = 18;
    pub const NO_PIC_REORDERING: u32 = 19;
    pub const NO_BI_PRED: u32 = 20;
}

/// `VAPictureParameterBufferHEVC::slice_parsing_fields` bit positions.
pub mod hevc_slice_parsing_bits {
    pub const LISTS_MODIFICATION_PRESENT: u32 = 0;
    pub const LONG_TERM_REF_PICS_PRESENT: u32 = 1;
    pub const SPS_TEMPORAL_MVP_ENABLED: u32 = 2;
    pub const CABAC_INIT_PRESENT: u32 = 3;
    pub const OUTPUT_FLAG_PRESENT: u32 = 4;
    pub const DEPENDENT_SLICE_SEGMENTS_ENABLED: u32 = 5;
    pub const PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT: u32 = 6;
    pub const SAMPLE_ADAPTIVE_OFFSET_ENABLED: u32 = 7;
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED: u32 = 8;
    pub const PPS_DISABLE_DEBLOCKING_FILTER: u32 = 9;
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u32 = 10;
    pub const RAP_PIC: u32 = 11;
    pub const IDR_PIC: u32 = 12;
    pub const INTRA_PIC: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VASliceParameterBufferHEVC {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    /// Bytes from the start of the NAL unit (its two-byte header included) to
    /// `slice_segment_data()`, counted without emulation prevention bytes; the data buffer itself
    /// keeps them.
    pub slice_data_byte_offset: u32,
    pub slice_segment_address: u32,
    /// Indices into `ReferenceFrames` (0xFF: none).
    pub RefPicList: [[u8; 15]; 2],
    pub LongSliceFlags: u32,
    pub collocated_ref_idx: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    pub slice_beta_offset_div2: i8,
    pub slice_tc_offset_div2: i8,
    pub luma_log2_weight_denom: u8,
    pub delta_chroma_log2_weight_denom: i8,
    pub delta_luma_weight_l0: [i8; 15],
    pub luma_offset_l0: [i8; 15],
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    pub ChromaOffsetL0: [[i8; 2]; 15],
    pub delta_luma_weight_l1: [i8; 15],
    pub luma_offset_l1: [i8; 15],
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    pub ChromaOffsetL1: [[i8; 2]; 15],
    pub five_minus_max_num_merge_cand: u8,
    pub num_entry_point_offsets: u16,
    pub entry_offset_to_subset_array: u16,
    /// Emulation prevention bytes in the slice segment header.
    pub slice_data_num_emu_prevn_bytes: u16,
    pub va_reserved: [u32; VA_PADDING_LOW - 2],
}

/// `VASliceParameterBufferHEVC::LongSliceFlags` bit positions (`slice_type` and `color_plane_id` 2 bits).
pub mod hevc_slice_bits {
    pub const LAST_SLICE_OF_PIC: u32 = 0;
    pub const DEPENDENT_SLICE_SEGMENT: u32 = 1;
    pub const SLICE_TYPE: u32 = 2;
    pub const COLOR_PLANE_ID: u32 = 4;
    pub const SLICE_SAO_LUMA: u32 = 6;
    pub const SLICE_SAO_CHROMA: u32 = 7;
    pub const MVD_L1_ZERO: u32 = 8;
    pub const CABAC_INIT: u32 = 9;
    pub const SLICE_TEMPORAL_MVP_ENABLED: u32 = 10;
    pub const SLICE_DEBLOCKING_FILTER_DISABLED: u32 = 11;
    pub const COLLOCATED_FROM_L0: u32 = 12;
    pub const SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAIQMatrixBufferHEVC {
    /// `ScalingList[sizeId][matrixId][j]` of the spec for sizeId 0..3, raster order.
    pub ScalingList4x4: [[u8; 16]; 6],
    pub ScalingList8x8: [[u8; 64]; 6],
    pub ScalingList16x16: [[u8; 64]; 6],
    pub ScalingList32x32: [[u8; 64]; 2],
    pub ScalingListDC16x16: [u8; 6],
    pub ScalingListDC32x32: [u8; 2],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAImageFormat {
    pub fourcc: u32,
    pub byte_order: u32,
    pub bits_per_pixel: u32,
    pub depth: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub alpha_mask: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAImage {
    pub image_id: VAImageID,
    pub format: VAImageFormat,
    pub buf: VABufferID,
    pub width: u16,
    pub height: u16,
    pub data_size: u32,
    pub num_planes: u32,
    pub pitches: [u32; 3],
    pub offsets: [u32; 3],
    pub num_palette_entries: i32,
    pub entry_bytes: i32,
    pub component_order: [i8; 4],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

/// The `libva.so.2` entry points used (all `VAStatus`-returning unless noted).
pub type vaInitialize = unsafe extern "C" fn(dpy: VADisplay, major: *mut c_int, minor: *mut c_int) -> VAStatus;
pub type vaTerminate = unsafe extern "C" fn(dpy: VADisplay) -> VAStatus;
pub type vaErrorStr = unsafe extern "C" fn(status: VAStatus) -> *const c_char;
pub type vaQueryVendorString = unsafe extern "C" fn(dpy: VADisplay) -> *const c_char;
pub type vaSetInfoCallback = unsafe extern "C" fn(dpy: VADisplay, callback: VAMessageCallback, user_context: *mut c_void) -> VAMessageCallback;
pub type vaSetErrorCallback = unsafe extern "C" fn(dpy: VADisplay, callback: VAMessageCallback, user_context: *mut c_void) -> VAMessageCallback;
pub type vaGetConfigAttributes =
    unsafe extern "C" fn(dpy: VADisplay, profile: VAProfile, entrypoint: VAEntrypoint, attrib_list: *mut VAConfigAttrib, num_attribs: c_int) -> VAStatus;
pub type vaCreateConfig = unsafe extern "C" fn(
    dpy: VADisplay,
    profile: VAProfile,
    entrypoint: VAEntrypoint,
    attrib_list: *mut VAConfigAttrib,
    num_attribs: c_int,
    config_id: *mut VAConfigID,
) -> VAStatus;
pub type vaDestroyConfig = unsafe extern "C" fn(dpy: VADisplay, config_id: VAConfigID) -> VAStatus;
/// The last two arguments are `VASurfaceAttrib *attrib_list, unsigned int num_attribs`; this
/// backend passes none, so the attribute structure is not declared.
pub type vaCreateSurfaces = unsafe extern "C" fn(
    dpy: VADisplay,
    format: c_uint,
    width: c_uint,
    height: c_uint,
    surfaces: *mut VASurfaceID,
    num_surfaces: c_uint,
    attrib_list: *mut c_void,
    num_attribs: c_uint,
) -> VAStatus;
pub type vaDestroySurfaces = unsafe extern "C" fn(dpy: VADisplay, surfaces: *mut VASurfaceID, num_surfaces: c_int) -> VAStatus;
pub type vaCreateContext = unsafe extern "C" fn(
    dpy: VADisplay,
    config_id: VAConfigID,
    picture_width: c_int,
    picture_height: c_int,
    flag: c_int,
    render_targets: *mut VASurfaceID,
    num_render_targets: c_int,
    context: *mut VAContextID,
) -> VAStatus;
pub type vaDestroyContext = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID) -> VAStatus;
pub type vaCreateBuffer = unsafe extern "C" fn(
    dpy: VADisplay,
    context: VAContextID,
    type_: VABufferType,
    size: c_uint,
    num_elements: c_uint,
    data: *mut c_void,
    buf_id: *mut VABufferID,
) -> VAStatus;
pub type vaDestroyBuffer = unsafe extern "C" fn(dpy: VADisplay, buffer_id: VABufferID) -> VAStatus;
pub type vaBeginPicture = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID, render_target: VASurfaceID) -> VAStatus;
pub type vaRenderPicture = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID, buffers: *mut VABufferID, num_buffers: c_int) -> VAStatus;
pub type vaEndPicture = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID) -> VAStatus;
pub type vaSyncSurface = unsafe extern "C" fn(dpy: VADisplay, render_target: VASurfaceID) -> VAStatus;
pub type vaCreateImage = unsafe extern "C" fn(dpy: VADisplay, format: *mut VAImageFormat, width: c_int, height: c_int, image: *mut VAImage) -> VAStatus;
pub type vaDestroyImage = unsafe extern "C" fn(dpy: VADisplay, image: VAImageID) -> VAStatus;
pub type vaGetImage =
    unsafe extern "C" fn(dpy: VADisplay, surface: VASurfaceID, x: c_int, y: c_int, width: c_uint, height: c_uint, image: VAImageID) -> VAStatus;
pub type vaPutImage = unsafe extern "C" fn(
    dpy: VADisplay,
    surface: VASurfaceID,
    image: VAImageID,
    src_x: c_int,
    src_y: c_int,
    src_width: c_uint,
    src_height: c_uint,
    dest_x: c_int,
    dest_y: c_int,
    dest_width: c_uint,
    dest_height: c_uint,
) -> VAStatus;
pub type vaMapBuffer = unsafe extern "C" fn(dpy: VADisplay, buf_id: VABufferID, pbuf: *mut *mut c_void) -> VAStatus;
pub type vaUnmapBuffer = unsafe extern "C" fn(dpy: VADisplay, buf_id: VABufferID) -> VAStatus;
/// `libva-drm.so.2`: the display of a DRM device (render node) file descriptor.
pub type vaGetDisplayDRM = unsafe extern "C" fn(fd: c_int) -> VADisplay;
