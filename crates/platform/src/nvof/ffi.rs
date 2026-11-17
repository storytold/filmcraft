// The declarations in this file are transcribed from NVIDIA's `nvOpticalFlowCommon.h` and
// `nvOpticalFlowCuda.h` (NVIDIA Optical Flow SDK, API 2.0,
// https://github.com/NVIDIA/NVIDIAOpticalFlowSDK), which carry this notice, reproduced as it is:
//
// Copyright(c) 2020, NVIDIA CORPORATION.All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met :
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and / or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its
//    contributors may be used to endorse or promote products derived from
//    this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED.IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! NVIDIA Optical Flow SDK (`nvOpticalFlowCommon.h` and `nvOpticalFlowCuda.h`, API 2.0,
//! BSD-3-Clause header by NVIDIA) as the driver's `nvofapi64.dll` (Windows) or
//! `libnvidia-opticalflow.so.1` (Linux) expects it.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): plain `repr(C)` data, no logic.

#![allow(non_snake_case, non_camel_case_types, dead_code, clippy::upper_case_acronyms)]

use std::ffi::{c_char, c_int, c_void};

pub const NV_OF_API_MAJOR_VERSION: u16 = 2;
pub const NV_OF_API_MINOR_VERSION: u16 = 0;
pub const NV_OF_API_VERSION: u32 = ((NV_OF_API_MAJOR_VERSION << 4) | NV_OF_API_MINOR_VERSION) as u32;
pub const MIN_ERROR_STRING_SIZE: usize = 80;
pub const MAX_NUM_PLANES: usize = 3;

pub type NvOFHandle = *mut c_void;
pub type NvOFGPUBufferHandle = *mut c_void;
pub type NvOFPrivDataHandle = *mut c_void;
pub type CUcontext = *mut c_void;
pub type CUstream = *mut c_void;
pub type CUarray = *mut c_void;
pub type CUdeviceptr = u64;

pub type NV_OF_STATUS = c_int;
pub const NV_OF_SUCCESS: NV_OF_STATUS = 0;
pub const NV_OF_ERR_OF_NOT_AVAILABLE: NV_OF_STATUS = 1;
pub const NV_OF_ERR_UNSUPPORTED_DEVICE: NV_OF_STATUS = 2;
pub const NV_OF_ERR_DEVICE_DOES_NOT_EXIST: NV_OF_STATUS = 3;
pub const NV_OF_ERR_INVALID_PTR: NV_OF_STATUS = 4;
pub const NV_OF_ERR_INVALID_PARAM: NV_OF_STATUS = 5;
pub const NV_OF_ERR_INVALID_CALL: NV_OF_STATUS = 6;
pub const NV_OF_ERR_INVALID_VERSION: NV_OF_STATUS = 7;
pub const NV_OF_ERR_OUT_OF_MEMORY: NV_OF_STATUS = 8;
pub const NV_OF_ERR_NOT_INITIALIZED: NV_OF_STATUS = 9;
pub const NV_OF_ERR_UNSUPPORTED_FEATURE: NV_OF_STATUS = 10;
pub const NV_OF_ERR_GENERIC: NV_OF_STATUS = 11;

pub type NV_OF_BOOL = u32;
pub const NV_OF_FALSE: NV_OF_BOOL = 0;
pub const NV_OF_TRUE: NV_OF_BOOL = 1;

pub type NV_OF_CAPS = u32;
pub const NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES: NV_OF_CAPS = 0;
pub const NV_OF_CAPS_SUPPORTED_HINT_GRID_SIZES: NV_OF_CAPS = 1;
pub const NV_OF_CAPS_SUPPORT_HINT_WITH_OF_MODE: NV_OF_CAPS = 2;
pub const NV_OF_CAPS_SUPPORT_HINT_WITH_ST_MODE: NV_OF_CAPS = 3;
pub const NV_OF_CAPS_WIDTH_MIN: NV_OF_CAPS = 4;
pub const NV_OF_CAPS_HEIGHT_MIN: NV_OF_CAPS = 5;
pub const NV_OF_CAPS_WIDTH_MAX: NV_OF_CAPS = 6;
pub const NV_OF_CAPS_HEIGHT_MAX: NV_OF_CAPS = 7;
pub const NV_OF_CAPS_SUPPORT_ROI: NV_OF_CAPS = 8;
pub const NV_OF_CAPS_SUPPORT_ROI_MAX_NUM: NV_OF_CAPS = 9;

pub type NV_OF_PERF_LEVEL = u32;
pub const NV_OF_PERF_LEVEL_UNDEFINED: NV_OF_PERF_LEVEL = 0;
pub const NV_OF_PERF_LEVEL_SLOW: NV_OF_PERF_LEVEL = 5;
pub const NV_OF_PERF_LEVEL_MEDIUM: NV_OF_PERF_LEVEL = 10;
pub const NV_OF_PERF_LEVEL_FAST: NV_OF_PERF_LEVEL = 20;

pub type NV_OF_OUTPUT_VECTOR_GRID_SIZE = u32;
pub const NV_OF_OUTPUT_VECTOR_GRID_SIZE_UNDEFINED: NV_OF_OUTPUT_VECTOR_GRID_SIZE = 0;
pub const NV_OF_OUTPUT_VECTOR_GRID_SIZE_1: NV_OF_OUTPUT_VECTOR_GRID_SIZE = 1;
pub const NV_OF_OUTPUT_VECTOR_GRID_SIZE_2: NV_OF_OUTPUT_VECTOR_GRID_SIZE = 2;
pub const NV_OF_OUTPUT_VECTOR_GRID_SIZE_4: NV_OF_OUTPUT_VECTOR_GRID_SIZE = 4;

pub type NV_OF_HINT_VECTOR_GRID_SIZE = u32;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED: NV_OF_HINT_VECTOR_GRID_SIZE = 0;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_1: NV_OF_HINT_VECTOR_GRID_SIZE = 1;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_2: NV_OF_HINT_VECTOR_GRID_SIZE = 2;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_4: NV_OF_HINT_VECTOR_GRID_SIZE = 4;
pub const NV_OF_HINT_VECTOR_GRID_SIZE_8: NV_OF_HINT_VECTOR_GRID_SIZE = 8;

pub type NV_OF_MODE = u32;
pub const NV_OF_MODE_UNDEFINED: NV_OF_MODE = 0;
pub const NV_OF_MODE_OPTICALFLOW: NV_OF_MODE = 1;
pub const NV_OF_MODE_STEREODISPARITY: NV_OF_MODE = 2;

pub type NV_OF_BUFFER_USAGE = u32;
pub const NV_OF_BUFFER_USAGE_UNDEFINED: NV_OF_BUFFER_USAGE = 0;
pub const NV_OF_BUFFER_USAGE_INPUT: NV_OF_BUFFER_USAGE = 1;
pub const NV_OF_BUFFER_USAGE_OUTPUT: NV_OF_BUFFER_USAGE = 2;
pub const NV_OF_BUFFER_USAGE_HINT: NV_OF_BUFFER_USAGE = 3;
pub const NV_OF_BUFFER_USAGE_COST: NV_OF_BUFFER_USAGE = 4;

pub type NV_OF_BUFFER_FORMAT = u32;
pub const NV_OF_BUFFER_FORMAT_UNDEFINED: NV_OF_BUFFER_FORMAT = 0;
pub const NV_OF_BUFFER_FORMAT_GRAYSCALE8: NV_OF_BUFFER_FORMAT = 1;
pub const NV_OF_BUFFER_FORMAT_NV12: NV_OF_BUFFER_FORMAT = 2;
pub const NV_OF_BUFFER_FORMAT_ABGR8: NV_OF_BUFFER_FORMAT = 3;
pub const NV_OF_BUFFER_FORMAT_SHORT: NV_OF_BUFFER_FORMAT = 4;
pub const NV_OF_BUFFER_FORMAT_SHORT2: NV_OF_BUFFER_FORMAT = 5;
pub const NV_OF_BUFFER_FORMAT_UINT: NV_OF_BUFFER_FORMAT = 6;
pub const NV_OF_BUFFER_FORMAT_UINT8: NV_OF_BUFFER_FORMAT = 7;

pub type NV_OF_STEREO_DISPARITY_RANGE = u32;
pub const NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED: NV_OF_STEREO_DISPARITY_RANGE = 0;
pub const NV_OF_STEREO_DISPARITY_RANGE_128: NV_OF_STEREO_DISPARITY_RANGE = 128;
pub const NV_OF_STEREO_DISPARITY_RANGE_256: NV_OF_STEREO_DISPARITY_RANGE = 256;

pub type NV_OF_CUDA_BUFFER_TYPE = u32;
pub const NV_OF_CUDA_BUFFER_TYPE_UNDEFINED: NV_OF_CUDA_BUFFER_TYPE = 0;
pub const NV_OF_CUDA_BUFFER_TYPE_CUARRAY: NV_OF_CUDA_BUFFER_TYPE = 1;
pub const NV_OF_CUDA_BUFFER_TYPE_CUDEVICEPTR: NV_OF_CUDA_BUFFER_TYPE = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NV_OF_FLOW_VECTOR {
    pub flowx: i16,
    pub flowy: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_INIT_PARAMS {
    pub width: u32,
    pub height: u32,
    pub outGridSize: NV_OF_OUTPUT_VECTOR_GRID_SIZE,
    pub hintGridSize: NV_OF_HINT_VECTOR_GRID_SIZE,
    pub mode: NV_OF_MODE,
    pub perfLevel: NV_OF_PERF_LEVEL,
    pub enableExternalHints: NV_OF_BOOL,
    pub enableOutputCost: NV_OF_BOOL,
    pub hPrivData: NvOFPrivDataHandle,
    pub disparityRange: NV_OF_STEREO_DISPARITY_RANGE,
    pub enableRoi: NV_OF_BOOL,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_BUFFER_DESCRIPTOR {
    pub width: u32,
    pub height: u32,
    pub bufferUsage: NV_OF_BUFFER_USAGE,
    pub bufferFormat: NV_OF_BUFFER_FORMAT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_ROI_RECT {
    pub start_x: u32,
    pub start_y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_EXECUTE_INPUT_PARAMS {
    pub inputFrame: NvOFGPUBufferHandle,
    pub referenceFrame: NvOFGPUBufferHandle,
    pub externalHints: NvOFGPUBufferHandle,
    pub disableTemporalHints: NV_OF_BOOL,
    pub padding: u32,
    pub hPrivData: NvOFPrivDataHandle,
    pub padding2: u32,
    pub numRois: u32,
    pub roiData: *mut NV_OF_ROI_RECT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_EXECUTE_OUTPUT_PARAMS {
    pub outputBuffer: NvOFGPUBufferHandle,
    pub outputCostBuffer: NvOFGPUBufferHandle,
    pub hPrivData: NvOFPrivDataHandle,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NV_OF_BUFFER_STRIDE {
    pub strideXInBytes: u32,
    pub strideYInBytes: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NV_OF_CUDA_BUFFER_STRIDE_INFO {
    pub strideInfo: [NV_OF_BUFFER_STRIDE; MAX_NUM_PLANES],
    pub numPlanes: u32,
}

pub type PFNNVCREATEOPTICALFLOWCUDA = Option<unsafe extern "system" fn(device: CUcontext, hOf: *mut NvOFHandle) -> NV_OF_STATUS>;
pub type PFNNVOFINIT = Option<unsafe extern "system" fn(hOf: NvOFHandle, initParams: *const NV_OF_INIT_PARAMS) -> NV_OF_STATUS>;
pub type PFNNVOFCREATEGPUBUFFERCUDA = Option<
    unsafe extern "system" fn(
        hOf: NvOFHandle,
        bufferDesc: *const NV_OF_BUFFER_DESCRIPTOR,
        bufferType: NV_OF_CUDA_BUFFER_TYPE,
        hOfGpuBuffer: *mut NvOFGPUBufferHandle,
    ) -> NV_OF_STATUS,
>;
pub type PFNNVOFGPUBUFFERGETCUARRAY = Option<unsafe extern "system" fn(ofGpuBuffer: NvOFGPUBufferHandle) -> CUarray>;
pub type PFNNVOFGPUBUFFERGETCUDEVICEPTR = Option<unsafe extern "system" fn(ofGpuBuffer: NvOFGPUBufferHandle) -> CUdeviceptr>;
pub type PFNVOFGPUBUFFERGETSTRIDEINFO =
    Option<unsafe extern "system" fn(ofGpuBuffer: NvOFGPUBufferHandle, strideInfo: *mut NV_OF_CUDA_BUFFER_STRIDE_INFO) -> NV_OF_STATUS>;
pub type PFNNVOFSETIOCUDASTREAMS = Option<unsafe extern "system" fn(hOf: NvOFHandle, inputStream: CUstream, outputStream: CUstream) -> NV_OF_STATUS>;
pub type PFNNVOFEXECUTE = Option<
    unsafe extern "system" fn(
        hOf: NvOFHandle,
        executeInParams: *const NV_OF_EXECUTE_INPUT_PARAMS,
        executeOutParams: *mut NV_OF_EXECUTE_OUTPUT_PARAMS,
    ) -> NV_OF_STATUS,
>;
pub type PFNNVOFDESTROYGPUBUFFERCUDA = Option<unsafe extern "system" fn(buffer: NvOFGPUBufferHandle) -> NV_OF_STATUS>;
pub type PFNNVOFDESTROY = Option<unsafe extern "system" fn(hOf: NvOFHandle) -> NV_OF_STATUS>;
pub type PFNNVOFGETLASTERROR = Option<unsafe extern "system" fn(hOf: NvOFHandle, lastError: *mut c_char, size: *mut u32) -> NV_OF_STATUS>;
pub type PFNNVOFGETCAPS = Option<unsafe extern "system" fn(hOf: NvOFHandle, capsParam: NV_OF_CAPS, capsVal: *mut u32, size: *mut u32) -> NV_OF_STATUS>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_OF_CUDA_API_FUNCTION_LIST {
    pub nvCreateOpticalFlowCuda: PFNNVCREATEOPTICALFLOWCUDA,
    pub nvOFInit: PFNNVOFINIT,
    pub nvOFCreateGPUBufferCuda: PFNNVOFCREATEGPUBUFFERCUDA,
    pub nvOFGPUBufferGetCUarray: PFNNVOFGPUBUFFERGETCUARRAY,
    pub nvOFGPUBufferGetCUdeviceptr: PFNNVOFGPUBUFFERGETCUDEVICEPTR,
    pub nvOFGPUBufferGetStrideInfo: PFNVOFGPUBUFFERGETSTRIDEINFO,
    pub nvOFSetIOCudaStreams: PFNNVOFSETIOCUDASTREAMS,
    pub nvOFExecute: PFNNVOFEXECUTE,
    pub nvOFDestroyGPUBufferCuda: PFNNVOFDESTROYGPUBUFFERCUDA,
    pub nvOFDestroy: PFNNVOFDESTROY,
    pub nvOFGetLastError: PFNNVOFGETLASTERROR,
    pub nvOFGetCaps: PFNNVOFGETCAPS,
}

pub type MaxSupportedVersionFn = unsafe extern "system" fn(version: *mut u32) -> NV_OF_STATUS;
pub type CreateInstanceCudaFn = unsafe extern "system" fn(apiVer: u32, functionList: *mut NV_OF_CUDA_API_FUNCTION_LIST) -> NV_OF_STATUS;
