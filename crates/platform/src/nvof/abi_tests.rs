//! Layout checks for the NVIDIA Optical Flow SDK 2.0 FFI structures (`nvOpticalFlowCommon.h` and
//! `nvOpticalFlowCuda.h`) and the CUDA Driver API's `CUDA_MEMCPY2D_v2` on 64-bit targets.
//!
//! Unlike the NVENC and NVDEC checks, these expectations were not printed by a C compiler: they
//! were worked out from the public headers' field lists (checked against NVIDIA's published
//! `nvOpticalFlowCommon.h` / `nvOpticalFlowCuda.h` on 2026-11-17) with the C layout rules for
//! fixed-width integers, 4-byte enums and 8-byte pointers on x86_64 (Windows LLP64 and Linux LP64
//! agree for every member used: none is a `long`). Regenerating them with a C program built from
//! the headers, as `nvenc/abi_tests.rs` describes, would make them independent of this reading.

use super::ffi::*;
use std::mem::{align_of, offset_of, size_of};

#[test]
fn nv_of_flow_vector_layout_matches_the_header() {
    assert_eq!((size_of::<NV_OF_FLOW_VECTOR>(), align_of::<NV_OF_FLOW_VECTOR>()), (4, 2));
    assert_eq!(offset_of!(NV_OF_FLOW_VECTOR, flowx), 0);
    assert_eq!(offset_of!(NV_OF_FLOW_VECTOR, flowy), 2);
}

#[test]
fn nv_of_init_params_layout_matches_the_header() {
    assert_eq!((size_of::<NV_OF_INIT_PARAMS>(), align_of::<NV_OF_INIT_PARAMS>()), (48, 8));
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, width), 0);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, height), 4);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, outGridSize), 8);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, hintGridSize), 12);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, mode), 16);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, perfLevel), 20);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, enableExternalHints), 24);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, enableOutputCost), 28);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, hPrivData), 32);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, disparityRange), 40);
    assert_eq!(offset_of!(NV_OF_INIT_PARAMS, enableRoi), 44);
}

#[test]
fn nv_of_buffer_descriptor_and_stride_layouts_match_the_header() {
    assert_eq!((size_of::<NV_OF_BUFFER_DESCRIPTOR>(), align_of::<NV_OF_BUFFER_DESCRIPTOR>()), (16, 4));
    assert_eq!(offset_of!(NV_OF_BUFFER_DESCRIPTOR, width), 0);
    assert_eq!(offset_of!(NV_OF_BUFFER_DESCRIPTOR, height), 4);
    assert_eq!(offset_of!(NV_OF_BUFFER_DESCRIPTOR, bufferUsage), 8);
    assert_eq!(offset_of!(NV_OF_BUFFER_DESCRIPTOR, bufferFormat), 12);

    assert_eq!((size_of::<NV_OF_CUDA_BUFFER_STRIDE_INFO>(), align_of::<NV_OF_CUDA_BUFFER_STRIDE_INFO>()), (28, 4));
    assert_eq!(offset_of!(NV_OF_CUDA_BUFFER_STRIDE_INFO, strideInfo), 0);
    assert_eq!(offset_of!(NV_OF_CUDA_BUFFER_STRIDE_INFO, numPlanes), 24);
}

#[test]
fn nv_of_execute_params_and_function_list_layouts_match_the_header() {
    assert_eq!((size_of::<NV_OF_EXECUTE_INPUT_PARAMS>(), align_of::<NV_OF_EXECUTE_INPUT_PARAMS>()), (56, 8));
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, inputFrame), 0);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, referenceFrame), 8);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, externalHints), 16);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, disableTemporalHints), 24);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, padding), 28);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, hPrivData), 32);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, padding2), 40);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, numRois), 44);
    assert_eq!(offset_of!(NV_OF_EXECUTE_INPUT_PARAMS, roiData), 48);

    assert_eq!((size_of::<NV_OF_EXECUTE_OUTPUT_PARAMS>(), align_of::<NV_OF_EXECUTE_OUTPUT_PARAMS>()), (24, 8));
    assert_eq!(offset_of!(NV_OF_EXECUTE_OUTPUT_PARAMS, outputBuffer), 0);
    assert_eq!(offset_of!(NV_OF_EXECUTE_OUTPUT_PARAMS, outputCostBuffer), 8);
    assert_eq!(offset_of!(NV_OF_EXECUTE_OUTPUT_PARAMS, hPrivData), 16);

    assert_eq!((size_of::<NV_OF_CUDA_API_FUNCTION_LIST>(), align_of::<NV_OF_CUDA_API_FUNCTION_LIST>()), (96, 8));
    assert_eq!(offset_of!(NV_OF_CUDA_API_FUNCTION_LIST, nvCreateOpticalFlowCuda), 0);
    assert_eq!(offset_of!(NV_OF_CUDA_API_FUNCTION_LIST, nvOFExecute), 56);
    assert_eq!(offset_of!(NV_OF_CUDA_API_FUNCTION_LIST, nvOFGetCaps), 88);
}

#[test]
fn cuda_memcpy2d_layout_matches_the_driver_api_reference() {
    // `CUDA_MEMCPY2D_v2`: `size_t` / pointer / `CUdeviceptr` members are 8 bytes, `CUmemorytype`
    // is a 4-byte enum padded to the next pointer (64-bit targets).
    use super::session::CudaMemcpy2D;
    assert_eq!((size_of::<CudaMemcpy2D>(), align_of::<CudaMemcpy2D>()), (128, 8));
    assert_eq!(offset_of!(CudaMemcpy2D, src_x_in_bytes), 0);
    assert_eq!(offset_of!(CudaMemcpy2D, src_y), 8);
    assert_eq!(offset_of!(CudaMemcpy2D, src_memory_type), 16);
    assert_eq!(offset_of!(CudaMemcpy2D, src_host), 24);
    assert_eq!(offset_of!(CudaMemcpy2D, src_device), 32);
    assert_eq!(offset_of!(CudaMemcpy2D, src_array), 40);
    assert_eq!(offset_of!(CudaMemcpy2D, src_pitch), 48);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_x_in_bytes), 56);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_y), 64);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_memory_type), 72);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_host), 80);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_device), 88);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_array), 96);
    assert_eq!(offset_of!(CudaMemcpy2D, dst_pitch), 104);
    assert_eq!(offset_of!(CudaMemcpy2D, width_in_bytes), 112);
    assert_eq!(offset_of!(CudaMemcpy2D, height), 120);
}
