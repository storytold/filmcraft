//! NVDEC on Windows is lazy: registering the platform's decoders, and opening streams that Media
//! Foundation decodes, load none of NVIDIA's NVDEC driver library (`nvcuvid.dll`) and so cannot wake
//! a hybrid-graphics laptop's discrete GPU; only a stream that needs NVDEC does.
#![cfg(target_os = "windows")]
// one module-handle lookup (an FFI call, as in the platform crate's own FFI modules)
#![allow(unsafe_code)]

mod common;

use common::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::core::w;

fn nvcuvid_loaded() -> bool {
    // SAFETY: a lookup of an already-loaded module by name; it loads nothing.
    unsafe { GetModuleHandleW(w!("nvcuvid.dll")) }.is_ok()
}

#[test]
fn nothing_of_nvdec_is_loaded_until_a_stream_needs_it() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    filmcraft_platform::register();
    assert!(!nvcuvid_loaded(), "register() loaded nvcuvid.dll");
    // a stream Media Foundation decodes (or that neither decoder is asked about) never reaches NVDEC
    let src = "testsrc2=s=320x180:r=24:d=1";
    let args = ["-f", "lavfi", "-i", src, "-c:v", "libx264", "-profile:v", "high", "-pix_fmt", "yuv420p"];
    let Some(path) = fixture(&ff, "lazy_h264_420_8.mp4", &args) else { panic!("fixture") };
    let s = read_stream(&path);
    let _decoder = filmcraft_codecs::make_video_decoder(&s.entry).unwrap();
    assert!(!nvcuvid_loaded(), "opening an 8-bit 4:2:0 H.264 stream loaded nvcuvid.dll");
    let args = ["-f", "lavfi", "-i", src, "-c:v", "libx265", "-tag:v", "hvc1", "-x265-params", "log-level=error", "-pix_fmt", "yuv420p10le"];
    if let Some(path) = fixture(&ff, "lazy_hevc_420_10.mp4", &args) {
        let s = read_stream(&path);
        let _decoder = filmcraft_codecs::make_video_decoder(&s.entry);
        assert!(!nvcuvid_loaded(), "opening a 10-bit 4:2:0 HEVC stream loaded nvcuvid.dll");
    }
    // (one test: the library stays loaded for the rest of the process once a stream has needed it)
    a_camera_format_stream_reaches_nvdec_when_the_gpu_decodes_it();
}

/// And a stream that does need it (4:2:2 10-bit H.264, which Media Foundation declines) is taken by
/// NVDEC through the registered factories on a GPU that decodes it (Blackwell), loading the driver
/// library at that point; elsewhere it stays with the software decoder.
fn a_camera_format_stream_reaches_nvdec_when_the_gpu_decodes_it() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    filmcraft_platform::register();
    let args =
        ["-f", "lavfi", "-i", "testsrc2=s=320x180:r=24:d=1", "-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le", "-x264-params", "keyint=1"];
    let Some(path) = fixture(&ff, "lazy_h264_422_10.mp4", &args) else { panic!("fixture") };
    let s = read_stream(&path);
    let decoder = filmcraft_codecs::make_video_decoder(&s.entry).unwrap();
    if decoder.name().starts_with("NVDEC") {
        assert!(nvcuvid_loaded(), "NVDEC decodes the stream but nvcuvid.dll is not loaded?");
    } else {
        eprintln!("this GPU does not decode 4:2:2 H.264: {}", decoder.name());
        assert!(decoder.name().contains("FilmCraft"), "{}", decoder.name());
    }
}
