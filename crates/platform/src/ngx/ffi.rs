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

//! NGX public SDK types and C entry points for FilmCraft's optional local DLAA build.
//! Layouts and function signatures use NVIDIA/DLSS include/nvsdk_ngx{,_defs,_params}.h.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_float, c_int, c_uint, c_void};

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

/// Opaque parameter block accessed through the SDK's C setters.
#[repr(C)]
pub struct NVSDK_NGX_Parameter {
    _opaque: [u8; 0],
}

// Public C ABI from NVIDIA/DLSS include/nvsdk_ngx.h and nvsdk_ngx_params.h.
unsafe extern "C" {
    pub fn NVSDK_NGX_D3D11_Init_with_ProjectID(
        project: *const c_char,
        engine: NVSDK_NGX_EngineType,
        version: *const c_char,
        app_data: *const u16,
        device: *mut c_void,
        info: *const NVSDK_NGX_FeatureCommonInfo,
        sdk: u32,
    ) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_Shutdown1(device: *mut c_void) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_GetCapabilityParameters(out: *mut *mut NVSDK_NGX_Parameter) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_DestroyParameters(params: *mut NVSDK_NGX_Parameter) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_CreateFeature(
        context: *mut c_void,
        feature: NVSDK_NGX_Feature,
        params: *const NVSDK_NGX_Parameter,
        out: *mut *mut NVSDK_NGX_Handle,
    ) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_EvaluateFeature(
        context: *mut c_void,
        handle: *const NVSDK_NGX_Handle,
        params: *const NVSDK_NGX_Parameter,
        callback: *const c_void,
    ) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_D3D11_ReleaseFeature(handle: *mut NVSDK_NGX_Handle) -> NVSDK_NGX_Result;
    pub fn NVSDK_NGX_Parameter_SetUI(params: *mut NVSDK_NGX_Parameter, name: *const c_char, value: c_uint);
    pub fn NVSDK_NGX_Parameter_SetI(params: *mut NVSDK_NGX_Parameter, name: *const c_char, value: c_int);
    pub fn NVSDK_NGX_Parameter_SetF(params: *mut NVSDK_NGX_Parameter, name: *const c_char, value: c_float);
    pub fn NVSDK_NGX_Parameter_SetD3d11Resource(params: *mut NVSDK_NGX_Parameter, name: *const c_char, value: *mut c_void);
}
