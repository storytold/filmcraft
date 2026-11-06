// Public Intel VPL API declarations, v2.15.0, revision
// c45b5d786bf7cdabbe49ff1bab78693ad78feb78, api/vpl/{mfxdefs,mfxcommon,
// mfxdispatcher,mfxsession,mfxvideo}.h. No implementation source was used.
//
// Copyright (c) 2020 Intel Corporation
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

//! Minimal Windows x64 MSVC loader/session declaration boundary.
//!
//! Status and C enum arguments are raw signed integers, so unknown driver values
//! remain representable. Handles are distinct non-owning pointers; this module
//! performs no native calls and grants no ownership or codec support evidence.

// The next M0 component consumes these declarations; do not broaden this allowance
// to the loader/session logic when that consumer is added.
#![allow(dead_code)]

use std::ffi::{c_char, c_void};

#[repr(C)]
pub(super) struct LoaderOpaque {
    private: [u8; 0],
}

#[repr(C)]
pub(super) struct ConfigOpaque {
    private: [u8; 0],
}

#[repr(C)]
pub(super) struct SessionOpaque {
    private: [u8; 0],
}

pub(super) type Loader = *mut LoaderOpaque;
pub(super) type Config = *mut ConfigOpaque;
pub(super) type Session = *mut SessionOpaque;
pub(super) type Status = i32;

pub(super) const STATUS_NONE: Status = 0;
pub(super) const STATUS_NOT_FOUND: Status = -9;
pub(super) const CAPS_IMPL_PATH: i32 = 3;
pub(super) const CAPS_EXTENDED_DEVICE_ID: i32 = 4;
pub(super) const HANDLE_D3D11_DEVICE: i32 = 3;

/// Word representation of the public union: Minor is its low byte, Major its high byte.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StructVersion(u16);

impl StructVersion {
    /// Pack the major and minor bytes using mfxStructVersion's header layout.
    pub(super) const fn new(major: u8, minor: u8) -> Self {
        Self(((major as u16) << 8) | minor as u16)
    }

    /// Return the packed header word without interpreting it as a Rust enum.
    pub(super) const fn bits(self) -> u16 {
        self.0
    }
}

/// Word representation of mfxVersion; preserves its u32 union alignment.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Version(u32);

impl Version {
    /// Pack the major and minor words using mfxVersion's header layout.
    pub(super) const fn new(major: u16, minor: u16) -> Self {
        Self(((major as u32) << 16) | minor as u32)
    }

    /// Return the complete packed API version for FFI comparisons.
    pub(super) const fn bits(self) -> u32 {
        self.0
    }

    /// Extract the high word of the packed API version.
    pub(super) const fn major(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// Extract the low word of the packed API version.
    pub(super) const fn minor(self) -> u16 {
        self.0 as u16
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) union VariantData {
    pub(super) u8_value: u8,
    pub(super) i8_value: i8,
    pub(super) u16_value: u16,
    pub(super) i16_value: i16,
    pub(super) u32_value: u32,
    pub(super) i32_value: i32,
    pub(super) u64_value: u64,
    pub(super) i64_value: i64,
    pub(super) f32_value: f32,
    pub(super) f64_value: f64,
    pub(super) fp16_value: u16,
    pub(super) pointer: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Variant {
    pub(super) version: StructVersion,
    // Explicitly initialize the public header's two padding bytes.
    pub(super) reserved: [u8; 2],
    pub(super) kind: i32,
    pub(super) data: VariantData,
}

impl Variant {
    /// Initialize a U32 property and zero its reserved and unused payload bytes.
    pub(super) const fn u32(value: u32) -> Self {
        Self { version: StructVersion::new(1, 1), reserved: [0; 2], kind: 5, data: VariantData { u64_value: value as u64 } }
    }

    /// Initialize a U16 property and zero its reserved and unused payload bytes.
    pub(super) const fn u16(value: u16) -> Self {
        Self { version: StructVersion::new(1, 1), reserved: [0; 2], kind: 3, data: VariantData { u64_value: value as u64 } }
    }

    /// Non-owning property value. The caller keeps the pointee alive through its native use.
    pub(super) const fn pointer(value: *mut c_void) -> Self {
        Self { version: StructVersion::new(1, 1), reserved: [0; 2], kind: 11, data: VariantData { pointer: value } }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ExtendedDeviceId {
    pub(super) version: StructVersion,
    pub(super) vendor_id: u16,
    pub(super) device_id: u16,
    pub(super) pci_domain: u32,
    pub(super) pci_bus: u32,
    pub(super) pci_device: u32,
    pub(super) pci_function: u32,
    pub(super) device_luid: [u8; 8],
    pub(super) luid_device_node_mask: u32,
    pub(super) luid_valid: u32,
    pub(super) drm_render_node_num: u32,
    pub(super) drm_primary_node_num: u32,
    pub(super) revision_id: u16,
    pub(super) reserved1: [u8; 18],
    pub(super) device_name: [c_char; 128],
}

pub(super) type Load = unsafe extern "C" fn() -> Loader;
pub(super) type Unload = unsafe extern "C" fn(Loader);
pub(super) type CreateConfig = unsafe extern "C" fn(Loader) -> Config;
pub(super) type SetConfigFilterProperty = unsafe extern "C" fn(Config, *const u8, Variant) -> Status;
pub(super) type EnumImplementations = unsafe extern "C" fn(Loader, u32, i32, *mut *mut c_void) -> Status;
pub(super) type ReleaseImplDescription = unsafe extern "C" fn(Loader, *mut c_void) -> Status;
pub(super) type CreateSession = unsafe extern "C" fn(Loader, u32, *mut Session) -> Status;
pub(super) type QueryVersion = unsafe extern "C" fn(Session, *mut Version) -> Status;
pub(super) type GetHandle = unsafe extern "C" fn(Session, i32, *mut *mut c_void) -> Status;
pub(super) type Close = unsafe extern "C" fn(Session) -> Status;

#[cfg(test)]
mod tests {
    use super::Variant;
    use std::ffi::c_void;

    #[test]
    fn scalar_variants_zero_reserved_and_unused_payload_bytes() {
        for (variant, kind, value) in [(Variant::u32(u32::MAX), 5, u64::from(u32::MAX)), (Variant::u16(u16::MAX), 3, u64::from(u16::MAX))] {
            assert_eq!(variant.version.bits(), 0x0101);
            assert_eq!(variant.reserved, [0, 0]);
            assert_eq!(variant.kind, kind);
            // SAFETY: both scalar constructors initialize the entire union through its u64 member.
            assert_eq!(unsafe { variant.data.u64_value }, value);
        }
    }

    #[test]
    fn pointer_variant_preserves_exact_borrowed_pointer() {
        let mut marker = 41_u64;
        let pointer = std::ptr::from_mut(&mut marker).cast::<c_void>();
        let variant = Variant::pointer(pointer);
        assert_eq!(variant.version.bits(), 0x0101);
        assert_eq!(variant.reserved, [0, 0]);
        assert_eq!(variant.kind, 11);
        // SAFETY: pointer() initializes this union member; reading it does not dereference the pointer.
        assert_eq!(unsafe { variant.data.pointer }, pointer);
        let null = Variant::pointer(std::ptr::null_mut());
        // SAFETY: pointer() initializes this member, including when its value is null.
        assert!(unsafe { null.data.pointer }.is_null());
    }
}
