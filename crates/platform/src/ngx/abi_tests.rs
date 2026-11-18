//! Layout and constant checks for `ngx::ffi` (64-bit Windows ABI).

use std::mem::{align_of, offset_of, size_of};

use super::ffi::{
    NVSDK_NGX_DLSS_Feature_Flags_AutoExposure, NVSDK_NGX_DLSS_Feature_Flags_IsHDR, NVSDK_NGX_DLSS_Feature_Flags_MVLowRes, NVSDK_NGX_DLSS_Hint_Render_Preset_K,
    NVSDK_NGX_Feature_SuperSampling, NVSDK_NGX_FeatureCommonInfo, NVSDK_NGX_Handle, NVSDK_NGX_LoggingInfo, NVSDK_NGX_PathListInfo,
    NVSDK_NGX_PerfQuality_Value_DLAA, NVSDK_NGX_Result_Fail, NVSDK_NGX_Result_Success, NVSDK_NGX_VERSION_API,
};

#[test]
fn ngx_info_struct_layouts_match_x64_abi() {
    assert_eq!(size_of::<NVSDK_NGX_Handle>(), 4);
    assert_eq!(align_of::<NVSDK_NGX_Handle>(), 4);

    assert_eq!(size_of::<NVSDK_NGX_PathListInfo>(), 16);
    assert_eq!(align_of::<NVSDK_NGX_PathListInfo>(), 8);
    assert_eq!(offset_of!(NVSDK_NGX_PathListInfo, Path), 0);
    assert_eq!(offset_of!(NVSDK_NGX_PathListInfo, Length), 8);

    assert_eq!(size_of::<NVSDK_NGX_LoggingInfo>(), 16);
    assert_eq!(align_of::<NVSDK_NGX_LoggingInfo>(), 8);
    assert_eq!(offset_of!(NVSDK_NGX_LoggingInfo, LoggingCallback), 0);
    assert_eq!(offset_of!(NVSDK_NGX_LoggingInfo, MinimumLoggingLevel), 8);
    assert_eq!(offset_of!(NVSDK_NGX_LoggingInfo, DisableOtherLoggingSinks), 12);

    assert_eq!(size_of::<NVSDK_NGX_FeatureCommonInfo>(), 40);
    assert_eq!(align_of::<NVSDK_NGX_FeatureCommonInfo>(), 8);
    assert_eq!(offset_of!(NVSDK_NGX_FeatureCommonInfo, PathListInfo), 0);
    assert_eq!(offset_of!(NVSDK_NGX_FeatureCommonInfo, InternalData), 16);
    assert_eq!(offset_of!(NVSDK_NGX_FeatureCommonInfo, LoggingInfo), 24);
}

#[test]
fn ngx_constants_match_streamline() {
    assert_eq!(NVSDK_NGX_Result_Success, 1);
    assert_eq!(NVSDK_NGX_Result_Fail, 0xBAD0_0000);
    assert_eq!(NVSDK_NGX_VERSION_API, 0x15);
    assert_eq!(NVSDK_NGX_Feature_SuperSampling, 1);
    assert_eq!(NVSDK_NGX_PerfQuality_Value_DLAA, 5);
    assert_eq!(NVSDK_NGX_DLSS_Hint_Render_Preset_K, 11);
    assert_eq!(NVSDK_NGX_DLSS_Feature_Flags_IsHDR | NVSDK_NGX_DLSS_Feature_Flags_MVLowRes | NVSDK_NGX_DLSS_Feature_Flags_AutoExposure, 67);
}
