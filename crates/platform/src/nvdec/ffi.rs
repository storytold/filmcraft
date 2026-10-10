//
// Copyright (c) 2010-2026 NVIDIA Corporation
//
// Permission is hereby granted, free of charge, to any person
// obtaining a copy of this software and associated documentation
// files (the "Software"), to deal in the Software without
// restriction, including without limitation the rights to use,
// copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the software, and to permit persons to whom the
// software is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
// OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
// HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
// WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
// OTHER DEALINGS IN THE SOFTWARE.
//

//! NVDEC declarations transcribed from NVIDIA's MIT headers (NVDEC API 13.1).
//! Linux LP64 only; C `tcu_ulong` is an unsigned long, not a u32.
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]
use std::ffi::{c_int, c_uint, c_ulong, c_void};
pub const cudaVideoCodec_H264: c_int = 4;
pub const cudaVideoCodec_HEVC: c_int = 8;
pub const cudaVideoChromaFormat_420: c_int = 1;
pub const cudaVideoSurfaceFormat_NV12: c_int = 0;
pub const cudaVideoSurfaceFormat_P016: c_int = 1;
pub const cudaVideoDeinterlaceMode_Weave: c_int = 0;
pub const cudaVideoCreate_PreferCUVID: c_ulong = 4;
pub const CUVID_PKT_ENDOFSTREAM: c_ulong = 1;
pub const CUVID_PKT_TIMESTAMP: c_ulong = 2;
pub const CUVID_PKT_ENDOFPICTURE: c_ulong = 8;
/// The start of `CUVIDPICPARAMS`: the fields every codec shares and this backend checks. The rest
/// (reserved words and the codec-specific union) is only ever passed through by pointer.
#[repr(C)]
pub struct CUVIDPICPARAMS {
    pub PicWidthInMbs: c_int,
    pub FrameHeightInMbs: c_int,
    pub CurrPicIdx: c_int,
    pub field_pic_flag: c_int,
    pub bottom_field_flag: c_int,
    pub second_field: c_int,
    pub nBitstreamDataLen: c_uint,
    pub pBitstreamData: *const u8,
    pub nNumSlices: c_uint,
    pub pSliceDataOffsets: *const c_uint,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ShortRect {
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AspectRatio {
    pub x: i32,
    pub y: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoSignal {
    pub flags: u8,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDEOFORMAT {
    pub codec: c_int,
    pub frame_rate: FrameRate,
    pub progressive_sequence: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub min_num_decode_surfaces: u8,
    pub coded_width: u32,
    pub coded_height: u32,
    pub display_area: Rect,
    pub chroma_format: c_int,
    pub bitrate: u32,
    pub display_aspect_ratio: AspectRatio,
    pub video_signal_description: VideoSignal,
    pub seqhdr_data_length: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDDECODECAPS {
    pub eCodecType: c_int,
    pub eChromaFormat: c_int,
    pub nBitDepthMinus8: u32,
    pub reserved1: [u32; 3],
    pub bIsSupported: u8,
    pub nNumNVDECs: u8,
    pub nOutputFormatMask: u16,
    pub nMaxWidth: u32,
    pub nMaxHeight: u32,
    pub nMaxMBCount: u32,
    pub nMinWidth: u16,
    pub nMinHeight: u16,
    pub bIsHistogramSupported: u8,
    pub nCounterBitDepth: u8,
    pub nMaxHistogramBins: u16,
    pub bIsDecodeStatsSupported: u8,
    pub reserved4: [u8; 3],
    pub reserved3: [u32; 9],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDDECODECREATEINFO {
    pub ulWidth: c_ulong,
    pub ulHeight: c_ulong,
    pub ulNumDecodeSurfaces: c_ulong,
    pub CodecType: c_int,
    pub ChromaFormat: c_int,
    pub ulCreationFlags: c_ulong,
    pub bitDepthMinus8: c_ulong,
    pub ulIntraDecodeOnly: c_ulong,
    pub ulMaxWidth: c_ulong,
    pub ulMaxHeight: c_ulong,
    pub Reserved1: c_ulong,
    pub display_area: ShortRect,
    pub OutputFormat: c_int,
    pub DeinterlaceMode: c_int,
    pub ulTargetWidth: c_ulong,
    pub ulTargetHeight: c_ulong,
    pub ulNumOutputSurfaces: c_ulong,
    pub vidLock: *mut c_void,
    pub target_rect: ShortRect,
    pub enableHistogram: c_ulong,
    pub enableDecodeFeatures: c_ulong,
    pub Reserved2: [c_ulong; 3],
}
pub type SequenceCallback = unsafe extern "C" fn(*mut c_void, *mut CUVIDEOFORMAT) -> c_int;
pub type DecodeCallback = unsafe extern "C" fn(*mut c_void, *mut CUVIDPICPARAMS) -> c_int;
pub type DisplayCallback = unsafe extern "C" fn(*mut c_void, *mut CUVIDPARSERDISPINFO) -> c_int;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDPARSERPARAMS {
    pub CodecType: c_int,
    pub ulMaxNumDecodeSurfaces: u32,
    pub ulClockRate: u32,
    pub ulErrorThreshold: u32,
    pub ulMaxDisplayDelay: u32,
    pub flags: u32,
    pub uReserved1: [u32; 4],
    pub pUserData: *mut c_void,
    pub pfnSequenceCallback: Option<SequenceCallback>,
    pub pfnDecodePicture: Option<DecodeCallback>,
    pub pfnDisplayPicture: Option<DisplayCallback>,
    pub pfnGetOperatingPoint: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    pub pfnGetSEIMsg: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    pub pvReserved2: [*mut c_void; 5],
    pub pExtVideoInfo: *mut c_void,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDSOURCEDATAPACKET {
    pub flags: c_ulong,
    pub payload_size: c_ulong,
    pub payload: *const u8,
    pub timestamp: i64,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDPARSERDISPINFO {
    pub picture_index: c_int,
    pub progressive_frame: c_int,
    pub top_field_first: c_int,
    pub repeat_first_field: c_int,
    pub timestamp: i64,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CUVIDPROCPARAMS {
    pub progressive_frame: c_int,
    pub second_field: c_int,
    pub top_field_first: c_int,
    pub unpaired_field: c_int,
    pub reserved_flags: u32,
    pub reserved_zero: u32,
    pub raw_input_dptr: u64,
    pub raw_input_pitch: u32,
    pub raw_input_format: u32,
    pub raw_output_dptr: u64,
    pub raw_output_pitch: u32,
    pub Reserved1: u32,
    pub output_stream: *mut c_void,
    pub Reserved: [u32; 46],
    pub histogram_dptr: *mut u64,
    pub pCuvidProcExt: *mut c_void,
}
pub type GetDecoderCaps = unsafe extern "C" fn(*mut CUVIDDECODECAPS) -> c_int;
pub type CreateDecoder = unsafe extern "C" fn(*mut *mut c_void, *mut CUVIDDECODECREATEINFO) -> c_int;
pub type DestroyDecoder = unsafe extern "C" fn(*mut c_void) -> c_int;
pub type DecodePicture = unsafe extern "C" fn(*mut c_void, *mut CUVIDPICPARAMS) -> c_int;
pub type MapVideoFrame = unsafe extern "C" fn(*mut c_void, c_int, *mut u64, *mut u32, *mut CUVIDPROCPARAMS) -> c_int;
pub type UnmapVideoFrame = unsafe extern "C" fn(*mut c_void, u64) -> c_int;
pub type CreateVideoParser = unsafe extern "C" fn(*mut *mut c_void, *mut CUVIDPARSERPARAMS) -> c_int;
pub type ParseVideoData = unsafe extern "C" fn(*mut c_void, *mut CUVIDSOURCEDATAPACKET) -> c_int;
pub type DestroyVideoParser = unsafe extern "C" fn(*mut c_void) -> c_int;
