//! What the apps register so that exports use the GPU (`filmcraft-gpu`) through the export
//! pipeline's hooks (`filmcraft-export`), which cannot depend on each other (both are L3):
//!
//! - [`GpuFrameRenderer`]: the off-screen compositor as a [`FrameRenderer`] (GPU rendering: Auto),
//!   with the resident path of ADR 0003 Step 2 (the export's post-render work on the GPU, the
//!   planes left there for the encoder);
//! - the ProRes and DNxHR compute-shader encoders as encoder backends (Hardware encoding: Auto);
//! - [`GpuPair`]: renderers and encoders on one device ([`filmcraft_gpu::ExportDevice`]), which the
//!   export takes first, so a rendered frame can stay on the GPU into the encoder.
//!
//! [`register`] registers all of them; the desktop app and the CLI call it at startup.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use filmcraft_export::{FrameRenderer, PlaneFormat, PostSettings, ResidentFrame};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{Image, RenderOptions, SourceProvider};
use filmcraft_time::Tick;

/// The GPU export frame renderer: exports with GPU rendering Auto composite on the GPU and fall
/// back to the CPU reference renderer wherever it cannot. Made by [`GpuPair`], it can also keep
/// frames on the GPU ([`FrameRenderer::render_resident`]).
pub struct GpuFrameRenderer(pub filmcraft_gpu::ExportRenderer);

impl FrameRenderer for GpuFrameRenderer {
    fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
        self.0.render(project, seq, t, opts, sources)
    }

    fn caps(&self) -> filmcraft_render::plan::DeviceCaps {
        self.0.caps()
    }

    fn resident_supported(&self, post: &PostSettings) -> bool {
        self.0.resident_supported(&post_config(post))
    }

    fn render_resident(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
        post: &PostSettings,
    ) -> Option<ResidentFrame> {
        self.0.render_resident(project, seq, t, opts, sources, &post_config(post)).map(|p| Box::new(p) as ResidentFrame)
    }
}

/// The export's post-render settings as `filmcraft-gpu`'s post pass takes them. Every field is
/// named (no `..`): a setting added to [`PostSettings`] does not compile here until the GPU side
/// says how it handles it, instead of being dropped and running an export it does not support.
pub fn post_config(p: &PostSettings) -> filmcraft_gpu::PostConfig {
    let PostSettings { rendered, width, height, content, offset, limiter, hdr, floats, alpha, planes, image_overlay, name_overlay, timecode_overlay } = p;
    let PlaneFormat { kr, kb, chroma_422, alpha: alpha_plane, bit_depth } = planes;
    filmcraft_gpu::PostConfig {
        rendered: *rendered,
        width: *width,
        height: *height,
        content: *content,
        offset: *offset,
        limiter: *limiter,
        hdr: *hdr,
        floats: *floats,
        // (the export pairs only planes whose alpha is the export's: a mismatch is not run here)
        alpha: *alpha && *alpha_plane,
        kr: *kr,
        kb: *kb,
        chroma_422: *chroma_422,
        bit_depth: if alpha == alpha_plane { *bit_depth } else { 0 },
        overlays: *image_overlay || *name_overlay || *timecode_overlay,
    }
}

/// Export renderers and ProRes / DNxHR encoders on one device: with GPU rendering and Hardware
/// encoding both Auto, a rendered frame stays on the GPU into the encoder (ADR 0003 Step 2).
pub struct GpuPair(pub filmcraft_gpu::ExportDevice);

impl filmcraft_export::GpuPair for GpuPair {
    fn renderer(&self) -> Option<Box<dyn FrameRenderer>> {
        self.0.renderer().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn FrameRenderer>)
    }
    fn prores_backend(&self) -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
        self.0.prores_encoder().map(|e| Box::new(e) as Box<dyn filmcraft_prores::SliceBackend>)
    }
    fn dnx_backend(&self) -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
        self.0.dnx_encoder().map(|e| Box::new(e) as Box<dyn filmcraft_dnx::MacroblockBackend>)
    }
}

/// The pair on this host's export device, `None` without a usable adapter.
pub fn gpu_pair() -> Option<Box<dyn filmcraft_export::GpuPair>> {
    filmcraft_gpu::ExportDevice::shared().map(|d| Box::new(GpuPair(d)) as Box<dyn filmcraft_export::GpuPair>)
}

/// A renderer on the renderers' own device (used when no pair can be made).
fn frame_renderer() -> Option<Box<dyn FrameRenderer>> {
    filmcraft_gpu::ExportRenderer::new().map(|r| Box::new(GpuFrameRenderer(r)) as Box<dyn FrameRenderer>)
}

fn prores_backend() -> Option<Box<dyn filmcraft_prores::SliceBackend>> {
    filmcraft_gpu::ProResGpuEncoder::new().map(|e| Box::new(e) as Box<dyn filmcraft_prores::SliceBackend>)
}

fn dnx_backend() -> Option<Box<dyn filmcraft_dnx::MacroblockBackend>> {
    filmcraft_gpu::DnxGpuEncoder::new().map(|e| Box::new(e) as Box<dyn filmcraft_dnx::MacroblockBackend>)
}

/// Register the GPU frame renderer, the ProRes and DNxHR encoder backends (the CPU codes what
/// they decline) and, tried first, the pair that keeps frames on the GPU.
pub fn register() {
    filmcraft_export::register_frame_renderer(frame_renderer);
    filmcraft_export::register_prores_backend(prores_backend);
    filmcraft_export::register_dnx_backend(dnx_backend);
    filmcraft_export::register_gpu_pair(gpu_pair);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> PostSettings {
        PostSettings {
            rendered: (320, 180),
            width: 256,
            height: 160,
            content: Some((256, 144)),
            offset: (0, 8),
            limiter: Some((8.0, 88.0)),
            hdr: false,
            floats: true,
            alpha: true,
            planes: PlaneFormat { kr: 0.2627, kb: 0.0593, chroma_422: false, alpha: true, bit_depth: 10 },
            image_overlay: false,
            name_overlay: false,
            timecode_overlay: false,
        }
    }

    /// Every setting reaches the GPU side; any overlay is one the post pass does not run; planes
    /// whose alpha is not the export's give a configuration the pass refuses.
    #[test]
    fn post_settings_reach_the_gpu_unchanged() {
        let c = post_config(&settings());
        let want = filmcraft_gpu::PostConfig {
            rendered: (320, 180),
            width: 256,
            height: 160,
            content: Some((256, 144)),
            offset: (0, 8),
            limiter: Some((8.0, 88.0)),
            hdr: false,
            floats: true,
            alpha: true,
            kr: 0.2627,
            kb: 0.0593,
            chroma_422: false,
            bit_depth: 10,
            overlays: false,
        };
        assert_eq!(c, want);
        let limits = wgpu::Limits::default();
        assert!(c.supported(&limits));
        for overlay in 0..3 {
            let mut s = settings();
            *[&mut s.image_overlay, &mut s.name_overlay, &mut s.timecode_overlay][overlay] = true;
            assert!(post_config(&s).overlays && !post_config(&s).supported(&limits), "overlay {overlay}");
        }
        let mut s = settings();
        s.hdr = true;
        assert!(!post_config(&s).supported(&limits));
        let mut s = settings();
        s.planes.alpha = false;
        assert!(!post_config(&s).supported(&limits), "planes without the export's alpha");
    }
}
