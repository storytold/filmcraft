/// Largest texture side a plan needs on the GPU (output and every layer).
pub(crate) fn plan_side(plan: &filmcraft_render::plan::FramePlan) -> usize {
    plan.max_side()
}

/// A software rasterizer (llvmpipe, SwiftShader, softpipe — VMs and containers without GPU device
/// access, missing or broken drivers) runs wgpu entirely on the CPU. Compositing through one is
/// slower than the native CPU compositor and hides the cause: GPU usage stays at 0 % while the
/// CPU saturates. Returns a description when the adapter is one.
pub fn software_rasterizer(info: &eframe::wgpu::AdapterInfo) -> Option<String> {
    rasterizer_reason(&info.name, info.device_type)
}

fn rasterizer_reason(name: &str, device_type: eframe::wgpu::DeviceType) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if device_type == eframe::wgpu::DeviceType::Cpu || ["llvmpipe", "swiftshader", "softpipe"].iter().any(|s| lower.contains(s)) {
        return Some(format!("software rasterizer ({name}, {device_type:?})"));
    }
    None
}

/// Whether the GPU compositor can run on this adapter: it renders and blends Rgba16Float
/// targets and uploads whole frames as textures. Older or OpenGL-backed GPUs (some Intel Macs,
/// VMs, Linux without Vulkan, WebGL) can't; those use the CPU compositor, which renders the same
/// frames. Returns the reason when unsupported.
pub fn gpu_compositor_unsupported(adapter: &eframe::wgpu::Adapter) -> Option<String> {
    use eframe::wgpu::{TextureFormat, TextureFormatFeatureFlags as F, TextureUsages as U};
    if let Some(why) = software_rasterizer(&adapter.get_info()) {
        return Some(why);
    }
    let f = adapter.get_texture_format_features(TextureFormat::Rgba16Float);
    if !f.allowed_usages.contains(U::RENDER_ATTACHMENT | U::TEXTURE_BINDING) {
        return Some("Rgba16Float is not renderable".into());
    }
    if !f.flags.contains(F::BLENDABLE | F::FILTERABLE) {
        return Some("Rgba16Float is not blendable/filterable".into());
    }
    let max = adapter.limits().max_texture_dimension_2d;
    if max < 4096 {
        return Some(format!("max texture size {max} < 4096"));
    }
    None
}

#[cfg(test)]
mod gpu_fallback_tests {
    use filmcraft_render::plan::{FramePlan, PlanLayer};

    /// Parent waits for the named child test; child returns false to run the body.
    #[cfg(not(target_arch = "wasm32"))]
    fn isolated_gpu_test(name: &str) -> bool {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD_ENV: &str = "FILMCRAFT_UI_GPU_TEST_CHILD";
        // Existing GPU-child watchdog; separate from the monitor's five-second settle budget.
        const CHILD_DEADLINE: Duration = Duration::from_secs(60);
        const CHILD_POLL: Duration = Duration::from_millis(10);
        let module = module_path!().split_once("::").unwrap().1;
        let test = format!("{module}::{name}");
        if std::env::var(CHILD_ENV).ok().as_deref() == Some(test.as_str()) {
            eprintln!("GPU isolation child entered: {test}");
            return false;
        }
        let policy = filmcraft_codecs::hw::gpu_frames();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(CHILD_ENV, &test)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let passed = format!("test {test} ... ok");
        // Drain while the child runs; a full stdout pipe must not masquerade as a GPU hang.
        let reader = std::thread::spawn(move || {
            let mut named_test_passed = false;
            for line in std::io::BufReader::new(stdout).lines() {
                let line = line.unwrap();
                named_test_passed |= line == passed;
                println!("{line}");
            }
            named_test_passed
        });
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let named_test_passed = reader.join().unwrap();
                assert!(status.success(), "GPU child {test}: {status}");
                assert!(named_test_passed, "GPU child did not run the named test: {test}");
                assert_eq!(filmcraft_codecs::hw::gpu_frames(), policy, "GPU child changed its parent's policy");
                eprintln!("GPU isolation parent policy unchanged: {test}, gpu_frames={policy}");
                return true;
            }
            if start.elapsed() >= CHILD_DEADLINE {
                let kill = child.kill();
                let wait = child.wait();
                let _ = reader.join();
                panic!("GPU child {test} exceeded {CHILD_DEADLINE:?}; kill={kill:?}, wait={wait:?}");
            }
            std::thread::sleep(CHILD_POLL);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[should_panic(expected = "GPU child did not run the named test")]
    fn child_isolation_rejects_zero_matching_tests() {
        isolated_gpu_test("missing_gpu_child_probe");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn destroyed_device_program_monitor_outputs_changed_cpu_frames() {
        use eframe::{egui_wgpu, wgpu};
        use filmcraft_media::{Generator, generators::GeneratorSource};
        use filmcraft_project::{Label, Project, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, Tick, TimeRange};
        use std::sync::{Arc, atomic::Ordering};
        use std::time::{Duration, Instant};

        // Same five-second settle budget as screenshot capture; a watchdog, not a speed assertion.
        const FRAME_DEADLINE: Duration = Duration::from_secs(5);
        if isolated_gpu_test("destroyed_device_program_monitor_outputs_changed_cpu_frames") {
            return;
        }

        fn picture(app: &mut crate::FilmcraftApp, ctx: &egui::Context, target: crate::frames::Target, frame: i64) -> (crate::FrameKey, egui::FullOutput) {
            let start = Instant::now();
            loop {
                let mut output = ctx.run_ui(
                    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0))), ..Default::default() },
                    |ui| {
                        app.logic_inner(ui.ctx());
                        let rect = ui.max_rect();
                        crate::panels::monitor::show(app, ui, rect, crate::panels::monitor::Which::Program);
                    },
                );
                if let Some(key) = app.program_picture()
                    && key.target == target
                    && key.frame == frame
                {
                    let texture = match target {
                        crate::frames::Target::SequencePlan(_) => app.gpu.as_ref().and_then(|g| g.texture),
                        _ => app.texture_existing("monitor-program").map(|(id, _)| id),
                    };
                    if let Some(id) = texture {
                        let drawn = ctx
                            .tessellate(output.shapes.clone(), output.pixels_per_point)
                            .iter()
                            .any(|p| matches!(&p.primitive, egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == id && !mesh.indices.is_empty()));
                        assert!(drawn, "exact frame must reach the monitor's draw commands");
                        return (key, output);
                    }
                }
                output.textures_delta.clear();
                assert!(start.elapsed() < FRAME_DEADLINE, "Program monitor did not settle: {:?}", app.program_picture());
                std::thread::yield_now();
            }
        }

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("SKIPPED: no Vulkan adapter");
            return;
        };
        eprintln!("Preview loss test adapter: {:?}", adapter.get_info());
        if let Some(why) = crate::gpu_compositor_unsupported(&adapter) {
            eprintln!("SKIPPED: preview device-loss adapter is unsupported: {why}");
            return;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = egui_wgpu::Renderer::new(&device, format, Default::default());
        let render_state = egui_wgpu::RenderState {
            adapter,
            available_adapters: vec![],
            instance,
            device: device.clone(),
            queue,
            target_format: format,
            renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        };
        let mut session = filmcraft_engine::Session::default();
        let mut project = Project::new("Preview device loss");
        let rate = FrameRate::FPS_24;
        let seq = project.new_sequence("Three pictures", SequenceSettings { width: 64, height: 36, frame_rate: rate, ..Default::default() }, 1, 0, None);
        let mut sources = filmcraft_render::SourceMap::default();
        // Three primary mattes: independent expected pixels catch blank/stale/wrong-source output.
        let colors = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        let expected = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
        for (frame, color) in colors.into_iter().enumerate() {
            let source = GeneratorSource::new(Generator::ColorMatte { color }, 64, 36, rate, rate.tick_of(1));
            let reference = GeneratorSource::new(Generator::ColorMatte { color }, 64, 36, rate, rate.tick_of(1));
            let item = filmcraft_engine::demo::add_generator(&mut project, &session.media, source, &format!("Matte {frame}"), Label::Iris, None);
            sources.0.insert(item, Arc::new(reference));
            let clip = project.make_track_item(item, TrackKind::Video, rate.tick_of(frame as i64), TimeRange::new(Tick::ZERO, rate.tick_of(1)), rate).unwrap();
            project.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
        }
        session.project = Arc::new(project);
        session.state.active_sequence = Some(seq);
        session.set_playhead(Tick::ZERO);
        let mut app = crate::FilmcraftApp::new(session);
        app.set_wgpu(render_state);
        let broken = app.gpu.as_ref().expect("GPU compositor must be active").broken.clone();
        assert!(!broken.load(Ordering::Relaxed), "healthy-device control");
        assert!(!crate::panels::menu_dialogs::software_renderer(&app), "healthy control must select the GPU");
        let ctx = egui::Context::default();
        let (healthy_key, mut healthy_output) = picture(&mut app, &ctx, crate::frames::Target::SequencePlan(seq), 0);
        healthy_output.textures_delta.clear();
        let gpu = app.gpu.as_ref().unwrap();
        assert_eq!(gpu.last_key, Some(healthy_key));
        assert!(!app.textures.contains_key("monitor-program"), "healthy control must not use CPU textures");
        let (w, h, pixels) = gpu.compositor.read_output().expect("healthy monitor GPU pixels");
        assert_eq!((w, h), gpu.size);
        assert!(!pixels.is_empty());
        assert!(pixels.as_chunks::<4>().0.iter().all(|p| p == &expected[0]), "healthy GPU must show the red matte");
        device.destroy();
        let _ = device.poll(wgpu::PollType::Poll);
        assert!(broken.load(Ordering::Relaxed), "device-lost notification must reach preview policy");
        for (frame, color) in expected.iter().enumerate().skip(1) {
            app.session.set_playhead(rate.tick_of(frame as i64));
            let (key, mut output) = picture(&mut app, &ctx, crate::frames::Target::Sequence(seq), frame as i64);
            assert!(!app.gpu_compositor_active(), "monitor must retire the lost compositor");
            assert_eq!(app.texture_key("monitor-program"), Some(key));
            let id = app.texture_existing("monitor-program").unwrap().0;
            let deltas = output.textures_delta.set.get(&id).expect("changed frame must upload a CPU texture");
            let [delta] = deltas.as_slice() else { panic!("monitor must upload one complete picture") };
            assert_eq!(delta.pos, None, "monitor uploads a complete picture");
            let egui::ImageData::Color(image) = &delta.image;
            let img = app.frames.get(&key).expect("CPU frame cache must contain the exact shown frame");
            assert_eq!(image.size, [img.w, img.h]);
            assert!(!image.pixels.is_empty());
            let uploaded: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let cpu = filmcraft_render::render_sequence(
                &app.session.project,
                seq,
                rate.tick_of(frame as i64),
                filmcraft_render::RenderOptions { scale: key.size as f32 / 1000.0, captions: true, ..Default::default() },
                &sources,
            )
            .unwrap();
            assert_eq!((img.w, img.h), (cpu.w, cpu.h));
            assert_eq!(img.px, cpu.over_black_rgba8(), "normal worker must match the independent source-map reference");
            assert_eq!(uploaded, img.px, "egui upload must contain the rendered CPU pixels");
            assert!(uploaded.as_chunks::<4>().0.iter().all(|p| p == color), "post-loss monitor must show the next primary matte");
            output.textures_delta.clear();
        }
        eprintln!("Preview output: healthy GPU red -> destroyed device -> CPU green/blue; reference/cache/egui upload/draw commands match");
    }

    #[test]
    fn review_regression_returned_compositor_error_disables_gpu_and_reports_failure() {
        #[cfg(not(target_arch = "wasm32"))]
        if isolated_gpu_test("review_regression_returned_compositor_error_disables_gpu_and_reports_failure") {
            return;
        }
        use crate::*;
        use filmcraft_frame::{Chroma, GpuPixels, GpuSurface, PixelData, VideoFrame};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        const FAILURE: &str = "injected compositor readback failure";
        #[derive(Debug, Default)]
        struct LostSurface(AtomicUsize);
        impl GpuSurface for LostSurface {
            fn size(&self) -> (u32, u32) {
                (2, 2)
            }
            fn byte_len(&self) -> usize {
                2 * 2 + 2
            }
            fn id(&self) -> u64 {
                u64::MAX
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn download(&self) -> Result<PixelData, String> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Err(FAILURE.into())
            }
        }

        let instance = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
            backends: eframe::wgpu::Backends::PRIMARY,
            ..eframe::wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let rs = match pollster::block_on(eframe::egui_wgpu::RenderState::create(&Default::default(), &instance, None, Default::default())) {
            Ok(rs) => rs,
            Err(error) => {
                eprintln!("SKIPPED (returned compositor error): GPU adapter unavailable: {error}");
                return;
            }
        };
        eprintln!("returned compositor error adapter: {:?}", rs.adapter.get_info());
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.set_wgpu(rs);
        if app.gpu.is_none() {
            eprintln!("SKIPPED (returned compositor error): unsupported compositor adapter");
            return;
        }
        let surface = Arc::new(LostSurface::default());
        let frame =
            Arc::new(VideoFrame { data: PixelData::Gpu(GpuPixels::new(surface.clone(), Chroma::C420, 8)), ..VideoFrame::rgba8(2, 2, vec![0; 2 * 2 * 4]) });
        let plan = FramePlan::Layers {
            width: 2,
            height: 2,
            layers: vec![PlanLayer::new(frame, filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::Blend::Normal)],
        };
        let plan = frames::GpuPlan { prepared: filmcraft_gpu::prepare(&plan), plan };
        let key = FrameKey { target: frames::Target::SequencePlan(filmcraft_project::ItemId(1)), frame: 0, size: 1000, revision: 0, draft: false };
        #[cfg(not(target_arch = "wasm32"))]
        {
            filmcraft_codecs::hw::set_gpu_frames(true);
            assert!(filmcraft_codecs::hw::gpu_frames(), "child policy control must enable GPU frames");
        }
        assert!(app.gpu_present(key, &plan).is_none());
        assert_eq!(surface.0.load(Ordering::Relaxed), 1, "failure probe must execute");
        assert!(app.frames.take_error().unwrap().contains(FAILURE));
        assert!(app.gpu.is_none(), "next monitor pass must select CPU compositing");
        assert!(!filmcraft_codecs::hw::gpu_frames(), "returned error must retire GPU-frame decoding inside the child");
        eprintln!("Returned-error child: failure probe executed, GPU retired, GPU-frame policy disabled");
    }

    /// Software rasterizers must not be treated as a GPU compositor: the preview crawled while
    /// "GPU" usage read 0 % because wgpu had silently fallen back to llvmpipe.
    #[test]
    fn software_rasterizers_are_detected() {
        use eframe::wgpu::DeviceType as D;
        let r = super::rasterizer_reason;
        assert!(r("llvmpipe (LLVM 23.1.1, 256 bits)", D::Cpu).is_some());
        assert!(r("NVIDIA GeForce RTX 4080", D::Cpu).is_some());
        assert!(r("SwiftShader Device", D::Other).is_some());
        assert!(r("softpipe", D::Cpu).is_some());
        assert!(r("AMD Radeon RX 5700 XT", D::DiscreteGpu).is_none());
        assert!(r("Apple M3 Pro", D::IntegratedGpu).is_none());
    }

    #[test]
    fn plan_side_covers_output_and_every_layer() {
        let frame = |w: u32, h: u32| std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba_f32(w, h, vec![0.0; (w * h * 4) as usize]));
        let layer = |w, h| PlanLayer {
            frame: frame(w, h),
            matrix: filmcraft_geom::Affine::IDENTITY,
            opacity: 1.0,
            blend: filmcraft_render::Blend::Normal,
            fx: None,
            input: None,
        };
        // a 4K source in an HD sequence still needs a 3840-wide texture on the GPU
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![layer(64, 64), layer(3840, 2160)] };
        assert_eq!(super::plan_side(&p), 3840);
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![] };
        assert_eq!(super::plan_side(&p), 1920);
        let p = FramePlan::Image(filmcraft_render::Image::new(800, 4000));
        assert_eq!(super::plan_side(&p), 4000);
    }
}
