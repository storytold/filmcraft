/* Original FilmCraft contributor work; SPDX-License-Identifier: MIT OR Apache-2.0.
 * Header-only ABI oracle; never loads a DLL or calls an MFX function.
 * Default mode: link only the C runtime, then print actual header layouts.
 * M0_SIGNATURES_ONLY: compile /c only; ten assignments must type-check with /WX.
 * Public headers pinned in headers.json; see README.md. */
#if !defined(_MSC_VER) || !defined(_M_X64) || !defined(_WIN64)
#error This oracle is restricted to the actual Windows x64 MSVC target.
#endif
#if defined(MFX_DISPATCHER_EXPOSED_PREFIX) || defined(ONEVPL_EXPERIMENTAL)
#error Dispatcher renaming and experimental declarations are outside this proof.
#endif
#include <stddef.h>
#include <stdio.h>
#include <vpl/mfxdispatcher.h>
#include <vpl/mfxvideo.h>

_Static_assert(MFX_VERSION == 2015, "Expected pinned stable VPL API 2.15");
_Static_assert(sizeof(void *) == 8, "Expected 64-bit pointers");
_Static_assert((char)-1 < 0, "Do not use MSVC /J unsigned-char mode");
_Static_assert(MFX_ERR_UNKNOWN < 0, "Status must represent signed errors");

#ifdef M0_SIGNATURES_ONLY
typedef mfxLoader (MFX_CDECL *P_Load)(void);
typedef void (MFX_CDECL *P_Unload)(mfxLoader);
typedef mfxConfig (MFX_CDECL *P_CreateConfig)(mfxLoader);
typedef mfxStatus (MFX_CDECL *P_SetProperty)(mfxConfig, const mfxU8 *, mfxVariant);
typedef mfxStatus (MFX_CDECL *P_Enumerate)(mfxLoader, mfxU32,
                                        mfxImplCapsDeliveryFormat, mfxHDL *);
typedef mfxStatus (MFX_CDECL *P_ReleaseDescription)(mfxLoader, mfxHDL);
typedef mfxStatus (MFX_CDECL *P_CreateSession)(mfxLoader, mfxU32, mfxSession *);
typedef mfxStatus (MFX_CDECL *P_QueryVersion)(mfxSession, mfxVersion *);
typedef mfxStatus (MFX_CDECL *P_GetHandle)(mfxSession, mfxHandleType, mfxHDL *);
typedef mfxStatus (MFX_CDECL *P_Close)(mfxSession);

/* Object-only mode intentionally has no main and must never be linked to VPL. */
void m0_check_signatures(void)
{
    P_Load p_load = &MFXLoad;
    P_Unload p_unload = &MFXUnload;
    P_CreateConfig p_config = &MFXCreateConfig;
    P_SetProperty p_property = &MFXSetConfigFilterProperty;
    P_Enumerate p_enum = &MFXEnumImplementations;
    P_ReleaseDescription p_release = &MFXDispReleaseImplDescription;
    P_CreateSession p_session = &MFXCreateSession;
    P_QueryVersion p_version = &MFXQueryVersion;
    P_GetHandle p_handle = &MFXVideoCORE_GetHandle;
    P_Close p_close = &MFXClose;
    (void)p_load; (void)p_unload; (void)p_config; (void)p_property;
    (void)p_enum; (void)p_release; (void)p_session; (void)p_version;
    (void)p_handle; (void)p_close;
}
#else
#define TYPE(T) printf("\"" #T "\":{\"size\":%zu,\"align\":%zu},\n", \
                       sizeof(T), (size_t)_Alignof(T))
#define OFFSET(T, F) printf("\"" #T "." #F "\":%zu,\n", offsetof(T, F))
#define FIELD_SIZE(T, F) printf("\"sizeof(" #T "." #F ")\":%zu,\n", \
                                sizeof(((T *)0)->F))
#define VALUE(V) printf("\"" #V "\":%lld,\n", (long long)(V))

int main(void)
{
    mfxVersion minimum = {0};
    minimum.Major = 2;
    minimum.Minor = 6;
    printf("{\n\"oracle\":\"vpl215-msvc-x64\",\n");
    VALUE(_MSC_VER); VALUE(_MSC_FULL_VER); VALUE(MFX_VERSION);
    VALUE(minimum.Version); VALUE(MFX_EXTENDEDDEVICEID_VERSION);
    VALUE(MFX_STRFIELD_LEN);
    TYPE(mfxU8); TYPE(mfxI8); TYPE(mfxU16); TYPE(mfxI16);
    TYPE(mfxU32); TYPE(mfxI32); TYPE(mfxU64); TYPE(mfxI64);
    TYPE(mfxF32); TYPE(mfxF64); TYPE(mfxFP16); TYPE(mfxChar);
    TYPE(mfxStatus); TYPE(mfxVariantType); TYPE(mfxImplCapsDeliveryFormat);
    TYPE(mfxHandleType); TYPE(mfxAccelerationMode); TYPE(mfxIMPL);
    TYPE(mfxHDL); TYPE(mfxLoader); TYPE(mfxConfig); TYPE(mfxSession);
    TYPE(mfxStructVersion); TYPE(mfxVersion); TYPE(mfxVariant);
    TYPE(mfxExtendedDeviceId);
    OFFSET(mfxStructVersion, Minor); OFFSET(mfxStructVersion, Major);
    OFFSET(mfxStructVersion, Version);
    OFFSET(mfxVersion, Minor); OFFSET(mfxVersion, Major);
    OFFSET(mfxVersion, Version);
    OFFSET(mfxVariant, Version); OFFSET(mfxVariant, Type);
    OFFSET(mfxVariant, Data); FIELD_SIZE(mfxVariant, Data);
    OFFSET(mfxVariant, Data.U8); OFFSET(mfxVariant, Data.I8);
    OFFSET(mfxVariant, Data.U16); OFFSET(mfxVariant, Data.I16);
    OFFSET(mfxVariant, Data.U32); OFFSET(mfxVariant, Data.I32);
    OFFSET(mfxVariant, Data.U64); OFFSET(mfxVariant, Data.I64);
    OFFSET(mfxVariant, Data.F32); OFFSET(mfxVariant, Data.F64);
    OFFSET(mfxVariant, Data.FP16); OFFSET(mfxVariant, Data.Ptr);
    OFFSET(mfxExtendedDeviceId, Version); OFFSET(mfxExtendedDeviceId, VendorID);
    OFFSET(mfxExtendedDeviceId, DeviceID); OFFSET(mfxExtendedDeviceId, PCIDomain);
    OFFSET(mfxExtendedDeviceId, PCIBus); OFFSET(mfxExtendedDeviceId, PCIDevice);
    OFFSET(mfxExtendedDeviceId, PCIFunction); OFFSET(mfxExtendedDeviceId, DeviceLUID);
    OFFSET(mfxExtendedDeviceId, LUIDDeviceNodeMask);
    OFFSET(mfxExtendedDeviceId, LUIDValid);
    OFFSET(mfxExtendedDeviceId, DRMRenderNodeNum);
    OFFSET(mfxExtendedDeviceId, DRMPrimaryNodeNum);
    OFFSET(mfxExtendedDeviceId, RevisionID); OFFSET(mfxExtendedDeviceId, reserved1);
    OFFSET(mfxExtendedDeviceId, DeviceName);
    FIELD_SIZE(mfxExtendedDeviceId, DeviceLUID);
    FIELD_SIZE(mfxExtendedDeviceId, reserved1);
    FIELD_SIZE(mfxExtendedDeviceId, DeviceName);
    VALUE(MFX_VARIANT_VERSION); VALUE(MFX_STRUCT_VERSION(1, 0));
    VALUE(MFX_VARIANT_TYPE_U16); VALUE(MFX_VARIANT_TYPE_U32);
    VALUE(MFX_VARIANT_TYPE_PTR); VALUE(MFX_IMPL_HARDWARE);
    VALUE(MFX_ACCEL_MODE_VIA_D3D11); VALUE(MFX_HANDLE_D3D11_DEVICE);
    VALUE(MFX_IMPLCAPS_IMPLPATH); VALUE(MFX_IMPLCAPS_DEVICE_ID_EXTENDED);
    VALUE(MFX_ERR_NONE); VALUE(MFX_ERR_UNKNOWN); VALUE(MFX_ERR_NULL_PTR);
    VALUE(MFX_ERR_UNSUPPORTED); VALUE(MFX_ERR_MEMORY_ALLOC);
    VALUE(MFX_ERR_INVALID_HANDLE); VALUE(MFX_ERR_NOT_INITIALIZED);
    VALUE(MFX_ERR_NOT_FOUND); VALUE(MFX_ERR_DEVICE_LOST);
    VALUE(MFX_ERR_UNDEFINED_BEHAVIOR); VALUE(MFX_ERR_DEVICE_FAILED);
    VALUE(MFX_WRN_IN_EXECUTION); VALUE(MFX_WRN_DEVICE_BUSY);
    VALUE(MFX_WRN_PARTIAL_ACCELERATION); VALUE(MFX_WRN_INCOMPATIBLE_VIDEO_PARAM);
    printf("\"end\":true\n}\n");
    if (ferror(stdout) || fflush(stdout) != 0) return 2;
    return 0;
}
#endif
