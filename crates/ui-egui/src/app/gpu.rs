use super::{FilmcraftApp, FrameKey, GpuState, frames, gpu_compositor_unsupported};

impl FilmcraftApp {
    /// Enable the GPU compositor on the eframe wgpu device.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        if let Some(why) = gpu_compositor_unsupported(&rs.adapter) {
            log::warn!("GPU compositor unavailable ({why}); preview fails closed");
            // Say why instead of letting the preview quietly crawl on the CPU: the GPU is the only
            // compositor unless the CPU one is chosen explicitly.
            self.ui.status = format!("{} ({why})", tl!("GPU preview unavailable; CPU fallback is disabled"));
            self.gpu_unavailable = Some(why);
            return;
        }
        // wgpu's default reaction to a validation error or lost device can panic; record it and fail closed.
        let broken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = broken.clone();
        rs.device.on_uncaptured_error(std::sync::Arc::new(move |e: eframe::wgpu::Error| {
            log::error!("GPU preview failed: {e}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }));
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filmcraft_gpu::GpuCompositor::new(&rs.device, &rs.queue)));
        match made {
            Ok(compositor) if !broken.load(std::sync::atomic::Ordering::Relaxed) => {
                let max_texture = rs.device.limits().max_texture_dimension_2d;
                self.gpu = Some(GpuState { render_state: rs, compositor, texture: None, last_key: None, size: (0, 0), last_ms: 0.0, broken, max_texture });
            }
            _ => {
                log::error!("GPU compositor failed to initialise; preview unavailable");
                self.gpu_unavailable = Some("GPU compositor failed to initialise".into());
                self.ui.status = tl!("GPU compositor failed to initialise; preview unavailable").into();
            }
        }
    }

    /// Select CPU compositing only when the host explicitly requests it.
    pub fn enable_cpu_composite(&mut self) {
        self.cpu_composite_explicit = true;
        self.ui.status = tl!("CPU compositing explicitly enabled").into();
    }

    /// Composite a plan on the GPU and return the egui texture showing it.
    pub fn gpu_present(&mut self, key: FrameKey, plan: &frames::GpuPlan) -> Option<(egui::TextureId, (u32, u32))> {
        if self.gpu.as_ref()?.broken.load(std::sync::atomic::Ordering::Relaxed) {
            self.ui.status = tl!("GPU preview failed; CPU fallback is disabled").into();
            return None;
        }
        let g = self.gpu.as_mut()?;
        if g.last_key == Some(key)
            && let Some(t) = g.texture
        {
            return Some((t, g.size));
        }
        let (_, max_side) = plan.extent();
        if max_side > g.max_texture as usize {
            self.ui.status = tl!("GPU preview exceeds the device texture limit").into();
            return None;
        }
        let t0 = web_time::Instant::now();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plan.composite_display(&mut g.compositor).map(|(view, size, _)| (view, size))));
        let result = match ran {
            Ok(result) => result,
            Err(_) => {
                self.ui.status = tl!("GPU preview compositor panicked; CPU fallback is disabled").into();
                return None;
            }
        };
        let (view, size) = match result {
            Ok(result) => result,
            Err(error) => {
                self.ui.status = error.to_string();
                return None;
            }
        };
        let g = self.gpu.as_mut()?;
        g.last_ms = t0.elapsed().as_secs_f32() * 1000.0;
        let mut renderer = g.render_state.renderer.write();
        let id = match g.texture {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear, id);
                id
            }
            None => renderer.register_native_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear),
        };
        g.texture = Some(id);
        g.last_key = Some(key);
        g.size = size;
        Some((id, size))
    }
}
