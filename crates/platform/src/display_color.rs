//! The colour space of the window's picture, for the operating system's colour management.
//!
//! FilmCraft draws sRGB-encoded BT.709 pixels (the GPU resolve applies the sRGB curve; egui's UI
//! colours are sRGB). On macOS the window's `CAMetalLayer` has no colour space unless one is set,
//! and an untagged layer is shown without colour matching: its values go to the display as they
//! are, so on a wide-gamut (Display P3) screen BT.709 red is shown as P3 red, oversaturated
//! (#2115). wgpu configures the layer with a nil colour space for its sRGB surface colour space,
//! and does so again whenever it reconfigures the surface (a resize), so [`tag_windows_srgb`]
//! is called every frame and only acts on a layer that has none.
//!
//! The one non-media OS call in this crate after `cursor`: `crates/platform` is the only crate
//! allowed `unsafe` (AGENTS.md §0.3, ADR 0001), and walking a layer's sublayers is an `unsafe`
//! binding.

/// Tag the Metal layers of the app's windows that have no colour space with sRGB, so macOS colour
/// matches FilmCraft's picture to each display. Returns how many layers it tagged; elsewhere (and
/// off the main thread) it does nothing.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn tag_windows_srgb() -> usize {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSColorSpace};
    use objc2_quartz_core::{CALayer, CAMetalLayer};

    let Some(mtm) = MainThreadMarker::new() else { return 0 };
    let Some(srgb) = NSColorSpace::sRGBColorSpace().CGColorSpace() else { return 0 };
    let tag = |layer: &CALayer| -> usize {
        match layer.downcast_ref::<CAMetalLayer>() {
            Some(metal) if metal.colorspace().is_none() => {
                metal.setColorspace(Some(&srgb));
                1
            }
            _ => 0,
        }
    };
    let mut tagged = 0;
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        let Some(root) = window.contentView().and_then(|v| v.layer()) else { continue };
        tagged += tag(&root);
        // wgpu (raw-window-metal) puts its CAMetalLayer as a sublayer of the view's layer.
        // SAFETY: called on the main thread (`mtm`), reading the sublayers of a layer we hold a
        // strong reference to; the array is retained and only read.
        if let Some(subs) = unsafe { root.sublayers() } {
            for layer in subs.iter() {
                tagged += tag(&layer);
            }
        }
    }
    tagged
}

#[cfg(not(target_os = "macos"))]
pub fn tag_windows_srgb() -> usize {
    0
}

#[cfg(test)]
mod tests {
    /// Tests run off the main thread (and without windows elsewhere): nothing to tag, no panic.
    #[test]
    fn off_the_main_thread_it_tags_nothing() {
        assert_eq!(std::thread::spawn(super::tag_windows_srgb).join().ok(), Some(0));
    }
}
