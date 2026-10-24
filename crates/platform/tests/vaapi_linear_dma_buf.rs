//! Opt-in VA linear-modifier allocation and PRIME_2 export telemetry.
#![cfg(target_os = "linux")]

use filmcraft_platform::vaapi::ffi;
use filmcraft_platform::vaapi::va::{Display, EncodeSession, encode_caps};

#[test]
fn va_linear_modifier_export_telemetry() {
    if std::env::var_os("FILMCRAFT_VA_LINEAR_TELEMETRY").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }

    let (profile, caps) = [ffi::VAProfileH264High, ffi::VAProfileH264Main]
        .into_iter()
        .find_map(|profile| encode_caps(profile).ok().map(|caps| (profile, caps)))
        .expect("explicit VA linear telemetry requested, but no H.264 encode profile is available");
    let rc = if caps.rate_control & ffi::VA_RC_VBR != 0 { ffi::VA_RC_VBR } else { ffi::VA_RC_CBR };
    let node = Display::open().and_then(|display| display.drm_node_id()).expect("VA DRM node");
    let session = EncodeSession::new_linear_diagnostic(node, profile, rc, 640, 360)
        .expect("explicit VA linear telemetry requested, but LINEAR surface allocation failed");
    let exported = session.export_linear_input_surface().expect("explicit VA linear telemetry requested, but exported modifier was not LINEAR");

    eprintln!("VA driver: {}", session.vendor());
    eprintln!("DRM node: {:?}", session.drm_node_id().expect("VA DRM node identity"));
    eprintln!("profile: {profile}, caps: {caps:?}, selected_rate_control: {rc:#x}");
    eprintln!("surface: {:?}, fourcc: {:#x}", exported.size(), exported.fourcc());
    for object in 0..exported.object_count() {
        let (size, modifier) = exported.object_layout(object).expect("validated object metadata");
        eprintln!("object[{object}]: size={size}, modifier={modifier:#x}");
    }
    for layer in 0..exported.layer_count() {
        for plane in 0..exported.plane_count(layer).expect("validated layer metadata") {
            let p = exported.plane(layer, plane).expect("validated plane metadata");
            eprintln!(
                "layer[{layer}] plane[{plane}]: object={}, format={:#x}, offset={}, pitch={}, modifier={:#x}, object_size={}",
                p.object_index, p.layer_format, p.offset, p.pitch, p.modifier, p.object_size
            );
        }
    }
}
