//! Vtable slots and struct layouts of `ffi` against the SDK 6.0 Linux `BlackmagicRawAPI.h` and
//! `LinuxCOM.h`, the Windows `BlackmagicRawAPI.idl` and the macOS `BlackmagicRawAPI.h` (same
//! interfaces in the same order). Slot numbers are each
//! method's position in its interface, after the three `IUnknown` entries (one pointer per virtual
//! function, in declaration order). Re-check them against the headers when moving to a newer SDK.
use super::ffi::*;
use std::mem::{align_of, offset_of, size_of};

macro_rules! slots {
    ($vtbl:ty { $($field:ident: $slot:expr),* $(,)? }) => {
        $(assert_eq!(offset_of!($vtbl, $field), $slot * size_of::<usize>(), concat!(stringify!($vtbl), ".", stringify!($field)));)*
    };
}

#[test]
fn vtable_slots_match_the_header() {
    slots!(IUnknownVtbl { query_interface: 0, add_ref: 1, release: 2 });
    slots!(FactoryVtbl { create_codec: 3, create_clip_geometry: 6 });
    slots!(CodecVtbl { open_clip: 3, open_clip_from_file_with_geometry: 6, set_callback: 7, flush_jobs: 10 });
    slots!(ConfigurationVtbl { set_pipeline: 3, set_from_device: 11, get_version: 12, get_camera_support_version: 13 });
    slots!(ClipProcessingAttributesVtbl { get_clip_attribute: 3, get_post_3d_lut: 8, set_embedded_post_3d_lut: 12 });
    slots!(ProcessedImageVtbl {
        get_width: 3,
        get_height: 4,
        get_resource: 5,
        get_resource_type: 6,
        get_resource_format: 7,
        get_resource_size_bytes: 8,
        get_resource_context_and_command_queue: 9,
    });
    slots!(JobVtbl { submit: 3, abort: 4, set_user_data: 5, get_user_data: 6 });
    slots!(CallbackVtbl {
        read_complete: 3,
        read_audio_complete: 4,
        decode_complete: 5,
        process_complete: 6,
        trim_progress: 7,
        trim_complete: 8,
        sidecar_metadata_parse_warning: 9,
        sidecar_metadata_parse_error: 10,
        prepare_pipeline_complete: 11,
    });
    #[cfg(not(windows))]
    slots!(CallbackVtbl { destructor: 12, deleting_destructor: 13 });
    slots!(ClipAudioVtbl {
        get_audio_format: 3,
        get_audio_bit_depth: 4,
        get_audio_channel_count: 5,
        get_audio_sample_rate: 6,
        get_audio_sample_count: 7,
        get_audio_samples: 8,
    });
    slots!(FrameVtbl { set_resolution_scale: 9, set_resource_format: 11, get_sensor_rate: 13, create_job_decode_and_process_frame: 14 });
    slots!(ClipVtbl {
        get_width: 3,
        get_height: 4,
        get_frame_rate: 5,
        get_frame_count: 6,
        get_timecode_for_frame: 7,
        get_camera_type: 11,
        clone_clip_processing_attributes: 12,
        reload_sidecar_file: 17,
        create_job_read_frame: 18,
        create_job_read_audio: 19,
        clone_with_geometry: 22,
    });
    // the callback vtable FilmCraft builds has every entry the API may call, and no more (Linux's
    // and macOS's classes end with a virtual destructor's two entries; Windows' IDL declares none)
    let callback_slots = if cfg!(windows) { 12 } else { 14 };
    assert_eq!(size_of::<CallbackVtbl>(), callback_slots * size_of::<usize>());
}

#[cfg(not(windows))]
#[test]
fn layouts_match_the_header() {
    assert_eq!((size_of::<Variant>(), align_of::<Variant>()), (16, 8), "struct Variant");
    assert_eq!(offset_of!(Variant, value), 8, "Variant's union");
    assert_eq!((size_of::<Iid>(), align_of::<Iid>()), (16, 1), "REFIID");
    #[cfg(target_os = "linux")]
    assert_eq!(size_of::<Ulong>(), 8, "ULONG is unsigned long (LP64)");
    #[cfg(target_os = "macos")]
    assert_eq!(size_of::<Ulong>(), 4, "ULONG is UInt32");
    // BlackmagicRawAPI.h: IID_IBlackmagicRaw = { 0xC2,0x5C,0x15,0xE2,0xD1,0xD0,0x47,0x43,... }
    assert_eq!(IID_IBlackmagicRaw.0, [0xC2, 0x5C, 0x15, 0xE2, 0xD1, 0xD0, 0x47, 0x43, 0x9E, 0xA8, 0xF2, 0x1F, 0x69, 0x75, 0x1A, 0xF3]);
    assert_eq!(IID_IUnknown.0, [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
}

#[cfg(windows)]
#[test]
fn layouts_match_the_header() {
    assert_eq!((size_of::<Variant>(), align_of::<Variant>()), (24, 8), "VARIANT (64-bit)");
    assert_eq!(offset_of!(Variant, value), 8, "VARIANT's union");
    assert_eq!((size_of::<Iid>(), align_of::<Iid>()), (16, 4), "GUID");
    assert_eq!(size_of::<Ulong>(), 4, "ULONG");
    // uuid(C25C15E2-D1D0-4743-9EA8-F21F69751AF3)
    assert_eq!((IID_IBlackmagicRaw.data1, IID_IBlackmagicRaw.data2, IID_IBlackmagicRaw.data3), (0xC25C15E2, 0xD1D0, 0x4743));
    assert_eq!(IID_IBlackmagicRaw.data4, [0x9E, 0xA8, 0xF2, 0x1F, 0x69, 0x75, 0x1A, 0xF3]);
}
