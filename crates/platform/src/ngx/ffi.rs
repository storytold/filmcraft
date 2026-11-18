// The declarations in this file are transcribed from NVIDIA's MIT-licensed Streamline SDK
// (`include/sl_dlss.h`, `source/plugins/sl.dlss/dlssEntry.cpp` and
// `source/plugins/sl.common/commonEntry.cpp` in https://github.com/NVIDIAGameWorks/Streamline),
// which carries this notice, reproduced as it is:
//
// Copyright (c) 2022-2023 NVIDIA CORPORATION. All rights reserved
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! NVIDIA NGX Direct3D 11 interface declarations as called by NVIDIA's MIT-licensed Streamline
//! `sl.common` and `sl.dlss` plugins against the driver's `_nvngx.dll`.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): plain `repr(C)` data, no logic.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_double, c_float, c_int, c_uint, c_ulonglong, c_void};

pub type NVSDK_NGX_Result = u32;
pub const NVSDK_NGX_Result_Success: NVSDK_NGX_Result = 0x1;
pub const NVSDK_NGX_Result_Fail: NVSDK_NGX_Result = 0xBAD0_0000;
pub const NVSDK_NGX_Result_FAIL_FeatureNotSupported: NVSDK_NGX_Result = NVSDK_NGX_Result_Fail | 1;
pub const NVSDK_NGX_Result_FAIL_FeatureNotFound: NVSDK_NGX_Result = NVSDK_NGX_Result_Fail | 11;

pub const NVSDK_NGX_VERSION_API: u32 = 0x0000_0015;

pub type NVSDK_NGX_Feature = c_int;
pub const NVSDK_NGX_Feature_SuperSampling: NVSDK_NGX_Feature = 1;

pub type NVSDK_NGX_EngineType = c_int;
pub const NVSDK_NGX_ENGINE_TYPE_CUSTOM: NVSDK_NGX_EngineType = 0;
pub const NVSDK_NGX_ENGINE_TYPE_UNREAL: NVSDK_NGX_EngineType = 1;

pub type NVSDK_NGX_PerfQuality_Value = c_int;
pub const NVSDK_NGX_PerfQuality_Value_MaxPerf: NVSDK_NGX_PerfQuality_Value = 0;
pub const NVSDK_NGX_PerfQuality_Value_Balanced: NVSDK_NGX_PerfQuality_Value = 1;
pub const NVSDK_NGX_PerfQuality_Value_MaxQuality: NVSDK_NGX_PerfQuality_Value = 2;
pub const NVSDK_NGX_PerfQuality_Value_UltraPerformance: NVSDK_NGX_PerfQuality_Value = 3;
pub const NVSDK_NGX_PerfQuality_Value_UltraQuality: NVSDK_NGX_PerfQuality_Value = 4;
pub const NVSDK_NGX_PerfQuality_Value_DLAA: NVSDK_NGX_PerfQuality_Value = 5;

/// `sl::DLSSPreset::ePresetK` (`11`): Transformer-based DLAA / Quality preset.
pub const NVSDK_NGX_DLSS_Hint_Render_Preset_J: u32 = 10;
pub const NVSDK_NGX_DLSS_Hint_Render_Preset_K: u32 = 11;

/// `NVSDK_NGX_DLSS_Feature_Flags` bit flags.
pub const NVSDK_NGX_DLSS_Feature_Flags_None: c_int = 0;
pub const NVSDK_NGX_DLSS_Feature_Flags_IsHDR: c_int = 1 << 0;
pub const NVSDK_NGX_DLSS_Feature_Flags_MVLowRes: c_int = 1 << 1;
pub const NVSDK_NGX_DLSS_Feature_Flags_MVJittered: c_int = 1 << 2;
pub const NVSDK_NGX_DLSS_Feature_Flags_DepthInverted: c_int = 1 << 3;
pub const NVSDK_NGX_DLSS_Feature_Flags_DoSharpening: c_int = 1 << 5;
pub const NVSDK_NGX_DLSS_Feature_Flags_AutoExposure: c_int = 1 << 6;

pub type NVSDK_NGX_Logging_Level = c_int;
pub const NVSDK_NGX_LOGGING_LEVEL_OFF: NVSDK_NGX_Logging_Level = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_Handle {
    pub Id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_PathListInfo {
    pub Path: *const *const u16,
    pub Length: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_LoggingInfo {
    pub LoggingCallback: *const c_void,
    pub MinimumLoggingLevel: NVSDK_NGX_Logging_Level,
    pub DisableOtherLoggingSinks: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_FeatureCommonInfo {
    pub PathListInfo: NVSDK_NGX_PathListInfo,
    pub InternalData: *mut c_void,
    pub LoggingInfo: NVSDK_NGX_LoggingInfo,
}

/// MSVC x64 C++ vtable of `NVSDK_NGX_Parameter` (overloaded `Set` and `Get` methods are laid out in
/// reverse declaration order within each overload group).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_Parameter_Vtable {
    pub SetVoidPointer: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, *mut c_void)>,
    pub SetD3d12Resource: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, *mut c_void)>,
    pub SetD3d11Resource: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, *mut c_void)>,
    pub SetI: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, c_int)>,
    pub SetUI: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, c_uint)>,
    pub SetD: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, c_double)>,
    pub SetF: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, c_float)>,
    pub SetULL: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter, *const c_char, c_ulonglong)>,
    pub GetVoidPointer: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut *mut c_void) -> NVSDK_NGX_Result>,
    pub GetD3d12Resource: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut *mut c_void) -> NVSDK_NGX_Result>,
    pub GetD3d11Resource: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut *mut c_void) -> NVSDK_NGX_Result>,
    pub GetI: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut c_int) -> NVSDK_NGX_Result>,
    pub GetUI: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut c_uint) -> NVSDK_NGX_Result>,
    pub GetD: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut c_double) -> NVSDK_NGX_Result>,
    pub GetF: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut c_float) -> NVSDK_NGX_Result>,
    pub GetULL: Option<unsafe extern "C" fn(*const NVSDK_NGX_Parameter, *const c_char, *mut c_ulonglong) -> NVSDK_NGX_Result>,
    pub Reset: Option<unsafe extern "C" fn(*mut NVSDK_NGX_Parameter)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NVSDK_NGX_Parameter {
    pub vtable: *const NVSDK_NGX_Parameter_Vtable,
}

pub type PfnNgxD3D11InitProjectId = unsafe extern "C" fn(
    *const c_char,
    NVSDK_NGX_EngineType,
    *const c_char,
    *const u16,
    *mut c_void,
    u32,
    *const NVSDK_NGX_FeatureCommonInfo,
) -> NVSDK_NGX_Result;

pub type PfnNgxD3D11Shutdown1 = unsafe extern "C" fn(*mut c_void) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11GetCapabilityParameters = unsafe extern "C" fn(*mut *mut NVSDK_NGX_Parameter) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11AllocateParameters = unsafe extern "C" fn(*mut *mut NVSDK_NGX_Parameter) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11DestroyParameters = unsafe extern "C" fn(*mut NVSDK_NGX_Parameter) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11CreateFeature =
    unsafe extern "C" fn(*mut c_void, NVSDK_NGX_Feature, *const NVSDK_NGX_Parameter, *mut *mut NVSDK_NGX_Handle) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11EvaluateFeature = unsafe extern "C" fn(*mut c_void, *const NVSDK_NGX_Handle, *const NVSDK_NGX_Parameter, *const c_void) -> NVSDK_NGX_Result;
pub type PfnNgxD3D11ReleaseFeature = unsafe extern "C" fn(*mut NVSDK_NGX_Handle) -> NVSDK_NGX_Result;
