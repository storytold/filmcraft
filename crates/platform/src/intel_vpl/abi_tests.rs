use super::ffi::{self, ExtendedDeviceId, StructVersion, Variant, Version};
use std::ffi::c_void;
use std::mem::{align_of, offset_of, size_of};

#[test]
fn windows_x64_header_layout_sizes() {
    assert_eq!(size_of::<StructVersion>(), 2);
    assert_eq!(align_of::<StructVersion>(), 2);
    assert_eq!(size_of::<Version>(), 4);
    assert_eq!(align_of::<Version>(), 4);
    assert_eq!(size_of::<Variant>(), 16);
    assert_eq!(align_of::<Variant>(), 8);
    assert_eq!(size_of::<ExtendedDeviceId>(), 196);
    assert_eq!(align_of::<ExtendedDeviceId>(), 4);
}

#[test]
fn windows_x64_header_layout_offsets() {
    assert_eq!(offset_of!(Variant, version), 0);
    assert_eq!(offset_of!(Variant, kind), 4);
    assert_eq!(offset_of!(Variant, data), 8);
    assert_eq!(offset_of!(ExtendedDeviceId, version), 0);
    assert_eq!(offset_of!(ExtendedDeviceId, vendor_id), 2);
    assert_eq!(offset_of!(ExtendedDeviceId, device_id), 4);
    assert_eq!(offset_of!(ExtendedDeviceId, pci_domain), 8);
    assert_eq!(offset_of!(ExtendedDeviceId, pci_bus), 12);
    assert_eq!(offset_of!(ExtendedDeviceId, pci_device), 16);
    assert_eq!(offset_of!(ExtendedDeviceId, pci_function), 20);
    assert_eq!(offset_of!(ExtendedDeviceId, device_luid), 24);
    assert_eq!(offset_of!(ExtendedDeviceId, luid_device_node_mask), 32);
    assert_eq!(offset_of!(ExtendedDeviceId, luid_valid), 36);
    assert_eq!(offset_of!(ExtendedDeviceId, drm_render_node_num), 40);
    assert_eq!(offset_of!(ExtendedDeviceId, drm_primary_node_num), 44);
    assert_eq!(offset_of!(ExtendedDeviceId, revision_id), 48);
    assert_eq!(offset_of!(ExtendedDeviceId, reserved1), 50);
    assert_eq!(offset_of!(ExtendedDeviceId, device_name), 68);
}

#[test]
fn versions_use_header_bit_encoding() {
    assert_eq!(StructVersion::new(1, 1).bits(), 0x0101);
    assert_eq!(StructVersion::new(1, 0).bits(), 0x0100);
    assert_eq!(StructVersion::new(255, 254).bits(), 0xfffe);
    assert_eq!(Version::new(2, 6).bits(), 0x0002_0006);
    assert_eq!(Version::new(u16::MAX, u16::MAX).bits(), u32::MAX);
    assert_eq!(Version::new(2, 6).major(), 2);
    assert_eq!(Version::new(2, 6).minor(), 6);
}

#[test]
fn function_pointer_contracts_are_c_abi_and_preserve_signed_status() {
    fn load(_: ffi::Load) {}
    fn unload(_: ffi::Unload) {}
    fn config(_: ffi::CreateConfig) {}
    fn property(_: ffi::SetConfigFilterProperty) {}
    fn enumerate(_: ffi::EnumImplementations) {}
    fn release(_: ffi::ReleaseImplDescription) {}
    fn create(_: ffi::CreateSession) {}
    fn version(_: ffi::QueryVersion) {}
    fn handle(_: ffi::GetHandle) {}
    fn close(_: ffi::Close) {}

    let _: fn(unsafe extern "C" fn() -> ffi::Loader) = load;
    let _: fn(unsafe extern "C" fn(ffi::Loader)) = unload;
    let _: fn(unsafe extern "C" fn(ffi::Loader) -> ffi::Config) = config;
    let _: fn(unsafe extern "C" fn(ffi::Config, *const u8, Variant) -> i32) = property;
    let _: fn(unsafe extern "C" fn(ffi::Loader, u32, i32, *mut *mut c_void) -> i32) = enumerate;
    let _: fn(unsafe extern "C" fn(ffi::Loader, *mut c_void) -> i32) = release;
    let _: fn(unsafe extern "C" fn(ffi::Loader, u32, *mut ffi::Session) -> i32) = create;
    let _: fn(unsafe extern "C" fn(ffi::Session, *mut Version) -> i32) = version;
    let _: fn(unsafe extern "C" fn(ffi::Session, i32, *mut *mut c_void) -> i32) = handle;
    let _: fn(unsafe extern "C" fn(ffi::Session) -> i32) = close;
    let status: ffi::Status = -17;
    assert_eq!(status, -17);
    assert_eq!(ffi::STATUS_NONE, 0);
    assert_eq!(ffi::STATUS_NOT_FOUND, -9);
    assert_eq!(ffi::CAPS_IMPL_PATH, 3);
    assert_eq!(ffi::CAPS_EXTENDED_DEVICE_ID, 4);
    assert_eq!(ffi::HANDLE_D3D11_DEVICE, 3);
}

#[test]
fn windows_x64_variant_union_members_have_header_layout() {
    use super::ffi::VariantData;
    use std::mem::{align_of, offset_of, size_of};

    assert_eq!(size_of::<VariantData>(), 8);
    assert_eq!(align_of::<VariantData>(), 8);

    assert_eq!(offset_of!(VariantData, u8_value), 0);
    assert_eq!(offset_of!(VariantData, i8_value), 0);
    assert_eq!(offset_of!(VariantData, u16_value), 0);
    assert_eq!(offset_of!(VariantData, i16_value), 0);
    assert_eq!(offset_of!(VariantData, u32_value), 0);
    assert_eq!(offset_of!(VariantData, i32_value), 0);
    assert_eq!(offset_of!(VariantData, u64_value), 0);
    assert_eq!(offset_of!(VariantData, i64_value), 0);
    assert_eq!(offset_of!(VariantData, f32_value), 0);
    assert_eq!(offset_of!(VariantData, f64_value), 0);
    assert_eq!(offset_of!(VariantData, fp16_value), 0);
    assert_eq!(offset_of!(VariantData, pointer), 0);
}
