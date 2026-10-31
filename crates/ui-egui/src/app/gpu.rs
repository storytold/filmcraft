use super::{FilmcraftApp, FrameKey, GpuState, frames, gpu_compositor_unsupported, plan_side};

impl FilmcraftApp {
    /// Enable the GPU compositor on the eframe wgpu device.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        if let Some(why) = gpu_compositor_unsupported(&rs.adapter) {
            log::warn!("GPU compositor disabled ({why}); compositing on the CPU");
            // Say so instead of letting the preview quietly crawl: "GPU 0 %, CPU 100 %" with no
            // explanation read like a renderer bug when the machine simply has no GPU access.
            self.ui.status = format!("{} ({why})", tl!("GPU acceleration is unavailable; preview renders on the CPU"));
            return;
        }
        // Error and loss callbacks feed one CPU-fallback flag.
        let broken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = broken.clone();
        rs.device.on_uncaptured_error(std::sync::Arc::new(move |e: eframe::wgpu::Error| {
            log::error!("GPU error, switching to the CPU compositor: {e}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }));
        let flag = broken.clone();
        rs.device.set_device_lost_callback(move |reason, message| {
            log::error!("GPU device lost ({reason:?}), switching to the CPU compositor: {message}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filmcraft_gpu::GpuCompositor::new(&rs.device, &rs.queue)));
        match made {
            Ok(compositor) if !broken.load(std::sync::atomic::Ordering::Relaxed) => {
                let max_texture = rs.device.limits().max_texture_dimension_2d;
                self.gpu = Some(GpuState { render_state: rs, compositor, texture: None, last_key: None, size: (0, 0), last_ms: 0.0, broken, max_texture });
            }
            _ => log::error!("GPU compositor failed to initialise; compositing on the CPU"),
        }
    }

    /// Whether the GPU compositor is running (not disabled by a GPU error or the CPU override).
    pub fn gpu_compositor_active(&self) -> bool {
        self.gpu.is_some()
    }

    /// Drop the GPU compositor after a GPU error; the monitors composite on the CPU from then on.
    fn disable_gpu(&mut self, why: &str) {
        // Decoders stop handing out GPU pictures after preview retires the compositor.
        filmcraft_codecs::hw::set_gpu_frames(false);
        log::error!("GPU compositor disabled: {why}");
        if let Some(g) = self.gpu.take()
            && let Some(id) = g.texture
        {
            g.render_state.renderer.write().free_texture(&id);
        }
        self.ui.status = tl!("Graphics problem: switched to software compositing").into();
    }

    /// Composite a plan on the GPU and return the egui texture showing it.
    pub fn gpu_present(&mut self, key: FrameKey, plan: &frames::GpuPlan) -> Option<(egui::TextureId, (u32, u32))> {
        if self.gpu.as_ref()?.broken.load(std::sync::atomic::Ordering::Relaxed) {
            self.disable_gpu("device error");
            return None;
        }
        let g = self.gpu.as_mut()?;
        if g.last_key == Some(key)
            && let Some(t) = g.texture
        {
            return Some((t, g.size));
        }
        if plan_side(&plan.plan) > g.max_texture as usize {
            self.disable_gpu("frame larger than the GPU's maximum texture size");
            return None;
        }
        let t0 = web_time::Instant::now();
        filmcraft_frame::pending::take();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.compositor.composite_prepared(&plan.plan, Some(&plan.prepared))));
        let pending = filmcraft_frame::pending::take();
        let (view, size) = match ran {
            Ok(Ok(output)) => output,
            Ok(Err(_)) if pending => return None,
            Ok(Err(error)) => {
                self.disable_gpu(&error);
                self.frames.reject_plan(key, error);
                return None;
            }
            Err(_) => {
                self.disable_gpu("compositor panicked");
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
