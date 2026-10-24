//! Dedicated catalog kernel parity; never uses compositor CPU fallback.

use filmcraft_geom::Vec2;
use filmcraft_gpu::transition_catalog::TransitionCatalogStage;
use filmcraft_project::{EffectKind, ParamKind, ParamValue, effect_defs, find_effect};
use filmcraft_render::{
    image::Image,
    transitions::{apply_scaled, gpu_plan::evaluate},
};
use std::time::Duration;

fn texture(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("transition-parity"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn boundary_instances(id: &str) -> [filmcraft_project::EffectInstance; 2] {
    let definition = find_effect(id).unwrap();
    let mut low = definition.instance();
    let mut high = definition.instance();
    for parameter in &definition.params {
        let endpoints = match &parameter.kind {
            ParamKind::Float { min, max, .. } => Some((ParamValue::Float(*min), ParamValue::Float(*max))),
            ParamKind::Angle => Some((ParamValue::Float(-180.0), ParamValue::Float(180.0))),
            ParamKind::Choice(choices) => Some((ParamValue::Choice(0), ParamValue::Choice(choices.len().saturating_sub(1) as u32))),
            ParamKind::Bool => Some((ParamValue::Bool(false), ParamValue::Bool(true))),
            ParamKind::Color => Some((ParamValue::Color([0.0; 4]), ParamValue::Color([1.0; 4]))),
            ParamKind::Point => Some((ParamValue::Vec2(Vec2::new(0.0, 0.0)), ParamValue::Vec2(Vec2::new(16.0, 8.0)))),
            _ => None,
        };
        if let Some((a, b)) = endpoints {
            low.params.get_mut(parameter.id).unwrap().value = a;
            high.params.get_mut(parameter.id).unwrap().value = b;
        }
    }
    [low, high]
}

const GPU_PARITY_IDS: &[&str] = &[
    "blur_dissolve",
    "whip_legacy",
    "whip",
    "spin_3d",
    "slice",
    "spinback_3d",
    "cross_dissolve",
    "cross_dissolve_legacy",
    "additive_dissolve",
    "additive_dissolve_legacy",
    "film_dissolve",
    "film_dissolve_legacy",
    "dip_to_black",
    "dip_to_black_legacy",
    "dip_to_white",
    "dip_to_white_legacy",
    "push",
    "flare",
    "linear_wipe",
    "soft_wipe",
    "wipe",
    "dip_to_color",
    "luma_fade",
    "non_additive_dissolve",
    "clock_wipe_legacy",
    "radial_wipe_legacy",
    "venetian_blinds",
    "checker_wipe",
    "gradient_wipe",
    "clock_wipe",
    "radial_wipe",
    "star_wipe",
    "panel_wipe",
    "plateau_wipe",
    "barn_doors",
    "inset",
    "iris_round",
    "iris_box",
    "iris_diamond",
    "iris_cross",
    "stretch_wipe",
    "neon_wipe",
    "burn_alpha",
    "mosaic_transition",
    "shape_dissolve",
    "shape_flow",
    "vr_iris_wipe",
    "vr_gradient_wipe",
    "burn_chroma",
    "chroma_leak",
    "cross_zoom",
    "directional_blur_transition",
    "flash",
    "lens_blur",
    "light_leak",
    "light_sweep",
    "phosphor",
    "radial_blur",
    "solarize",
    "stripe",
    "zoom_blur",
    "glow",
    "ray",
    "cross_zoom_legacy",
    "motion_camera",
    "roll",
    "film_roll",
    "stretch",
    "slide",
    "split",
    "motion_tween",
    "spring_motion",
    "pop_motion",
    "push_legacy",
    "slide_legacy",
    "split_legacy",
    "page_turn",
    "wave",
    "louver",
    "mirror_transition",
    "center_split",
    "band_slide",
    "text_animator",
    "typewriter",
    "pull_motion",
    "travel_motion",
    "spin_motion",
    "roll_3d",
    "vr_random_blocks",
    "block_motion",
    "flicker",
    "kaleidoscope",
    "liquid_distortion",
    "chaos",
    "earthquake",
    "glass",
    "glitch",
    "grunge",
    "tv_power",
    "vhs_damage",
    "vr_chroma_leaks",
    "vr_light_leaks",
    "vr_light_rays",
    "vr_mobius_zoom",
    "frame",
    "flip_motion",
    "page_peel",
    "vr_spherical_blur",
];

const UNSUPPORTED_TRANSITION_IDS: &[&str] = &["cube_spin", "flip_over", "fold_motion", "morph_cut"];

#[test]
fn every_registry_transition_is_in_parity_matrix_or_explicitly_unsupported() {
    let mut classified = GPU_PARITY_IDS.iter().copied().chain(UNSUPPORTED_TRANSITION_IDS.iter().copied()).collect::<Vec<_>>();
    classified.sort_unstable();
    let mut registered = effect_defs().iter().filter(|effect| effect.kind == EffectKind::VideoTransition).map(|effect| effect.id).collect::<Vec<_>>();
    registered.sort_unstable();
    assert_eq!(classified, registered);
    for &id in UNSUPPORTED_TRANSITION_IDS {
        let effect = find_effect(id).unwrap().instance();
        assert!(evaluate(&effect, 0.5, 1.0).is_err(), "unsupported registry ID {id} must fail before GPU dispatch");
    }
}

#[test]
fn new_family_routes_require_matching_ids_and_opcodes() {
    for id in ["flip_motion", "spin_3d", "spinback_3d", "slice", "blur_dissolve", "vr_spherical_blur", "whip_legacy", "whip"] {
        let plan = evaluate(&find_effect(id).unwrap().instance(), 0.5, 1.0).unwrap();
        assert!(TransitionCatalogStage::validate(&plan).is_ok(), "{id}");
        let mut mismatched = plan;
        mismatched.id = match id {
            "flip_motion" | "spin_3d" | "spinback_3d" | "slice" => "flip_over",
            "blur_dissolve" => "vr_spherical_blur",
            "whip_legacy" | "whip" => "blur_dissolve",
            _ => "blur_dissolve",
        }
        .into();
        assert!(TransitionCatalogStage::validate(&mismatched).is_err(), "mismatched route must fail closed: {id}");
    }
}

#[test]
fn three_d_catalog_routes_reject_other_valid_three_d_opcodes() {
    for (id, other_id) in [("flip_motion", "spin_3d"), ("spin_3d", "spinback_3d"), ("spinback_3d", "slice"), ("slice", "flip_motion")] {
        let mut plan = evaluate(&find_effect(id).unwrap().instance(), 0.5, 1.0).unwrap();
        plan.op = evaluate(&find_effect(other_id).unwrap().instance(), 0.5, 1.0).unwrap().op;
        assert!(TransitionCatalogStage::validate(&plan).is_err(), "{id} must reject {other_id}'s valid GPU payload");
    }
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, w: u32, h: u32, format: wgpu::TextureFormat) -> Vec<f32> {
    let bytes_per_pixel = if format == wgpu::TextureFormat::Rgba16Float { 8 } else { 16 };
    let row = (w * bytes_per_pixel).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("transition-parity-readback"),
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let submission = queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    if let Err(error) = device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(8)) }) {
        buffer.unmap();
        panic!("GPU readback poll failed: {error}");
    }
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            buffer.unmap();
            panic!("GPU readback mapping failed: {error}");
        }
        Err(error) => {
            buffer.unmap();
            panic!("GPU readback mapping callback timed out or disconnected: {error}");
        }
    }
    let bytes = slice.get_mapped_range().unwrap();
    let mut out = Vec::new();
    for y in 0..h {
        let pixels = &bytes[(y * row) as usize..(y * row + w * bytes_per_pixel) as usize];
        if format == wgpu::TextureFormat::Rgba16Float {
            for v in pixels.as_chunks::<2>().0 {
                out.push(filmcraft_gpu::f16_to_f32(u16::from_le_bytes(*v)));
            }
        } else {
            for v in pixels.as_chunks::<4>().0 {
                out.push(f32::from_le_bytes(*v));
            }
        }
    }
    drop(bytes);
    buffer.unmap();
    out
}

#[test]
fn catalog_dissolve_kernels_match_transparent_cpu_oracle() {
    // Only the allowlist must match the CPU oracle: the other ids fail closed in the strict export
    // and are measured one by one by `per_id_parity_report`.
    for id in filmcraft_gpu::TRANSITION_AMD_PARITY {
        assert!(GPU_PARITY_IDS.contains(id), "allowlisted {id} is not in the parity matrix");
    }
    assert_catalog_parity(filmcraft_gpu::TRANSITION_AMD_PARITY, false);
}

/// Per-id AMD parity report (`cargo test -p filmcraft-gpu --test transition_catalog -- --ignored
/// --nocapture per_id_parity_report`): each id runs alone, so one divergence does not hide the rest.
#[test]
#[ignore = "diagnostic report for the strict resident export's transition allowlist"]
fn per_id_parity_report() {
    for &id in GPU_PARITY_IDS {
        let passed = std::panic::catch_unwind(|| assert_catalog_parity(&[id], false)).is_ok();
        eprintln!("PARITY {id} {}", if passed { "PASS" } else { "FAIL" });
    }
}

#[test]
fn user_project_transition_kernels_match_cpu_oracle() {
    assert_catalog_parity(&["flare", "blur_dissolve", "dip_to_black", "dip_to_white"], true);
}

#[test]
fn preview_transition_parameter_ranges_match_cpu_oracle() {
    assert_catalog_parity(&["flare", "blur_dissolve", "dip_to_black", "dip_to_white"], false);
}

fn assert_catalog_parity(ids: &[&str], user_project_parameters: bool) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .find(|adapter| {
            let info = adapter.get_info();
            info.backend == wgpu::Backend::Vulkan
                && info.vendor == 0x1002
                && matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu)
        })
        .unwrap_or_else(|| panic!("GPU transition parity requires a physical AMD Vulkan adapter (vendor 0x1002); no adapter is not a pass"));
    let info = adapter.get_info();
    assert_eq!(info.backend, wgpu::Backend::Vulkan, "parity acceptance requires Vulkan");
    assert_eq!(info.vendor, 0x1002, "parity acceptance requires AMD");
    assert!(
        matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu),
        "parity acceptance requires physical GPU hardware: {info:?}"
    );
    eprintln!("transition parity adapter: {info:?}");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: wgpu::Features::FLOAT32_BLENDABLE, ..Default::default() }))
            .unwrap();
    let (w, h) = (17u32, 9u32);
    let mut a = Image::new(w as usize, h as usize);
    let mut b = a.clone();
    for i in 0..(w * h) as usize {
        let alpha = (i % 5) as f32 / 4.0;
        a.px[i * 4..i * 4 + 4].copy_from_slice(&[alpha * 0.7, alpha * 0.2, alpha * 0.1, alpha]);
        b.px[i * 4..i * 4 + 4].copy_from_slice(&[(1.0 - alpha) * 0.1, (1.0 - alpha) * 0.3, (1.0 - alpha) * 0.8, 1.0 - alpha]);
    }
    for format in [wgpu::TextureFormat::Rgba32Float, wgpu::TextureFormat::Rgba16Float] {
        let stage = TransitionCatalogStage::new(&device, format);
        let inputs = [texture(&device, w, h, format), texture(&device, w, h, format)];
        let mut oracle = [a.clone(), b.clone()];
        for (tex, img) in inputs.iter().zip(oracle.iter_mut()) {
            let bytes: Vec<u8> = if format == wgpu::TextureFormat::Rgba16Float {
                for v in &mut img.px {
                    *v = filmcraft_gpu::f16_to_f32(filmcraft_gpu::f32_to_f16(*v));
                }
                img.px.iter().flat_map(|v| filmcraft_gpu::f32_to_f16(*v).to_le_bytes()).collect()
            } else {
                img.px.iter().flat_map(|v| v.to_le_bytes()).collect()
            };
            let bytes_per_pixel = if format == wgpu::TextureFormat::Rgba16Float { 8 } else { 16 };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &bytes,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bytes_per_pixel), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        let views = [inputs[0].create_view(&Default::default()), inputs[1].create_view(&Default::default())];
        let output = texture(&device, w, h, format);
        let canvas = output.create_view(&Default::default());
        for &id in ids {
            let mut effect = find_effect(id).unwrap().instance();
            let defaults = effect.clone();
            match id {
                "push" => {
                    effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(1);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(73.0);
                }
                "flare" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(137.0);
                    effect.params.get_mut("color").unwrap().value = ParamValue::Color([0.2, 0.65, 0.9, 1.0]);
                }
                "linear_wipe" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(37.0);
                    effect.params.get_mut("feather").unwrap().value = ParamValue::Float(4.0);
                    effect.params.get_mut("border_width").unwrap().value = ParamValue::Float(3.0);
                    effect.params.get_mut("border_color").unwrap().value = ParamValue::Color([0.2, 0.65, 0.9, 0.3]);
                    effect.params.get_mut("antialias").unwrap().value = ParamValue::Choice(2);
                }
                "soft_wipe" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(137.0);
                    effect.params.get_mut("feather").unwrap().value = ParamValue::Float(203.0);
                }
                "dip_to_color" => {
                    effect.params.get_mut("color").unwrap().value = ParamValue::Color([0.3, 0.6, 0.7, 0.2]);
                    effect.params.get_mut("hold").unwrap().value = ParamValue::Float(20.0);
                }
                "luma_fade" => {
                    effect.params.get_mut("source").unwrap().value = ParamValue::Choice(1);
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(73.0);
                    effect.params.get_mut("invert").unwrap().value = ParamValue::Bool(true);
                }
                "gradient_wipe" => effect.params.get_mut("softness").unwrap().value = ParamValue::Float(67.0),
                "clock_wipe" => {
                    effect.params.get_mut("center").unwrap().value = ParamValue::Vec2(Vec2::new(5.25, 2.75));
                    effect.params.get_mut("start").unwrap().value = ParamValue::Float(137.0);
                    effect.params.get_mut("feather").unwrap().value = ParamValue::Float(9.0);
                    effect.params.get_mut("border_width").unwrap().value = ParamValue::Float(4.0);
                    effect.params.get_mut("border_color").unwrap().value = ParamValue::Color([0.2, 0.65, 0.9, 0.3]);
                    effect.params.get_mut("antialias").unwrap().value = ParamValue::Choice(2);
                }
                "radial_wipe" => effect.params.get_mut("corner").unwrap().value = ParamValue::Choice(3),
                "star_wipe" => {
                    effect.params.get_mut("points").unwrap().value = ParamValue::Float(7.0);
                    effect.params.get_mut("inner").unwrap().value = ParamValue::Float(33.0);
                    effect.params.get_mut("rotation").unwrap().value = ParamValue::Float(45.0);
                }
                "panel_wipe" => {
                    effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(2);
                    effect.params.get_mut("panels").unwrap().value = ParamValue::Float(5.0);
                    effect.params.get_mut("feather").unwrap().value = ParamValue::Float(7.0);
                }
                "plateau_wipe" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(37.0);
                    effect.params.get_mut("plateaus").unwrap().value = ParamValue::Float(7.0);
                }
                "barn_doors" => effect.params.get_mut("orientation").unwrap().value = ParamValue::Choice(0),
                "inset" => effect.params.get_mut("corner").unwrap().value = ParamValue::Choice(2),
                "stretch_wipe" => effect.params.get_mut("stretch").unwrap().value = ParamValue::Float(307.0),
                "neon_wipe" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(37.0);
                    effect.params.get_mut("glow").unwrap().value = ParamValue::Float(73.0);
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                    effect.params.get_mut("color").unwrap().value = ParamValue::Color([0.2, 0.65, 0.9, 0.3]);
                }
                "burn_alpha" => {
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(43.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                    effect.params.get_mut("burn_color").unwrap().value = ParamValue::Color([0.9, 0.25, 0.1, 0.2]);
                }
                "mosaic_transition" => {
                    effect.params.get_mut("block_size").unwrap().value = ParamValue::Float(47.0);
                    effect.params.get_mut("dissolve").unwrap().value = ParamValue::Bool(true);
                }
                "shape_dissolve" => {
                    effect.params.get_mut("shape").unwrap().value = ParamValue::Choice(3);
                    effect.params.get_mut("size").unwrap().value = ParamValue::Float(47.0);
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(3.2);
                    effect.params.get_mut("rotation").unwrap().value = ParamValue::Float(29.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "shape_flow" => {
                    effect.params.get_mut("shape").unwrap().value = ParamValue::Choice(5);
                    effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(1);
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(67.0);
                }
                "vr_iris_wipe" => {
                    effect.params.get_mut("center").unwrap().value = ParamValue::Vec2(Vec2::new(6.0, 3.0));
                    effect.params.get_mut("feather").unwrap().value = ParamValue::Float(15.0);
                    effect.params.get_mut("border_width").unwrap().value = ParamValue::Float(2.0);
                }
                "vr_gradient_wipe" => {
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(73.0);
                    effect.params.get_mut("invert").unwrap().value = ParamValue::Bool(true);
                }
                "burn_chroma" | "solarize" => effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0),
                "chroma_leak" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "cross_zoom" | "zoom_blur" => effect.params.get_mut("strength").unwrap().value = ParamValue::Float(73.0),
                "directional_blur_transition" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(37.0);
                    effect.params.get_mut("blur").unwrap().value = ParamValue::Float(73.0);
                }
                "flash" => effect.params.get_mut("intensity").unwrap().value = ParamValue::Float(73.0),
                "lens_blur" | "glow" => effect.params.get_mut("radius").unwrap().value = ParamValue::Float(47.0),
                "light_leak" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "light_sweep" => {
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(137.0);
                    effect.params.get_mut("width").unwrap().value = ParamValue::Float(73.0);
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                }
                "phosphor" => effect.params.get_mut("decay").unwrap().value = ParamValue::Float(73.0),
                "radial_blur" => effect.params.get_mut("angle").unwrap().value = ParamValue::Float(73.0),
                "stripe" => {
                    effect.params.get_mut("stripes").unwrap().value = ParamValue::Float(5.0);
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(37.0);
                }
                "ray" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(61.0);
                    effect.params.get_mut("length").unwrap().value = ParamValue::Float(73.0);
                }
                "motion_camera" => {
                    effect.params.get_mut("zoom").unwrap().value = ParamValue::Float(70.0);
                    effect.params.get_mut("rotation").unwrap().value = ParamValue::Float(18.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0);
                }
                "roll" | "slide" => effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0),
                "film_roll" => {
                    effect.params.get_mut("gap").unwrap().value = ParamValue::Float(40.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0);
                }
                "split" => effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0),
                "motion_tween" => effect.params.get_mut("scale").unwrap().value = ParamValue::Float(25.0),
                "spring_motion" => {
                    effect.params.get_mut("bounce").unwrap().value = ParamValue::Float(50.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0);
                }
                "pop_motion" => {
                    effect.params.get_mut("overshoot").unwrap().value = ParamValue::Float(50.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(50.0);
                }
                "wave" => {
                    effect.params.get_mut("amplitude").unwrap().value = ParamValue::Float(40.0);
                    effect.params.get_mut("wavelength").unwrap().value = ParamValue::Float(80.0);
                }
                "louver" => effect.params.get_mut("slats").unwrap().value = ParamValue::Float(8.0),
                "pull_motion" => effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(60.0),
                "travel_motion" => {
                    effect.params.get_mut("zoom").unwrap().value = ParamValue::Float(60.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(60.0);
                }
                "spin_motion" => {
                    effect.params.get_mut("rotation").unwrap().value = ParamValue::Float(540.0);
                    effect.params.get_mut("spin").unwrap().value = ParamValue::Choice(1);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(60.0);
                }
                "roll_3d" => {
                    effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(2);
                    effect.params.get_mut("perspective").unwrap().value = ParamValue::Float(70.0);
                }
                "vr_random_blocks" => {
                    effect.params.get_mut("block_size").unwrap().value = ParamValue::Float(120.0);
                    effect.params.get_mut("softness").unwrap().value = ParamValue::Float(70.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "block_motion" => {
                    effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(2);
                    effect.params.get_mut("blocks").unwrap().value = ParamValue::Float(10.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(60.0);
                }
                "flicker" => {
                    effect.params.get_mut("flickers").unwrap().value = ParamValue::Float(12.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "kaleidoscope" => {
                    effect.params.get_mut("segments").unwrap().value = ParamValue::Float(7.0);
                    effect.params.get_mut("zoom").unwrap().value = ParamValue::Float(70.0);
                }
                "liquid_distortion" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(100.0);
                    effect.params.get_mut("scale").unwrap().value = ParamValue::Float(300.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "chaos" | "glitch" | "vhs_damage" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(80.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "earthquake" => {
                    effect.params.get_mut("shake").unwrap().value = ParamValue::Float(120.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                    effect.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(60.0);
                }
                "glass" => {
                    effect.params.get_mut("cell_size").unwrap().value = ParamValue::Float(200.0);
                    effect.params.get_mut("refraction").unwrap().value = ParamValue::Float(80.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "grunge" => {
                    effect.params.get_mut("scale").unwrap().value = ParamValue::Float(300.0);
                    effect.params.get_mut("edge_width").unwrap().value = ParamValue::Float(20.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "tv_power" => effect.params.get_mut("line").unwrap().value = ParamValue::Float(12.0),
                "vr_chroma_leaks" | "vr_light_leaks" => {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(60.0);
                    effect.params.get_mut("seed").unwrap().value = ParamValue::Float(317.0);
                }
                "vr_light_rays" => {
                    effect.params.get_mut("center").unwrap().value = ParamValue::Vec2(Vec2::new(6.0, 4.0));
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(60.0);
                }
                "vr_mobius_zoom" => {
                    effect.params.get_mut("zoom").unwrap().value = ParamValue::Float(70.0);
                    effect.params.get_mut("twist").unwrap().value = ParamValue::Float(120.0);
                }
                "frame" => {
                    effect.params.get_mut("frame_width").unwrap().value = ParamValue::Float(36.0);
                    effect.params.get_mut("frame_color").unwrap().value = ParamValue::Color([0.2, 0.5, 0.8, 1.0]);
                    effect.params.get_mut("scale").unwrap().value = ParamValue::Float(45.0);
                }
                "page_peel" => {
                    effect.params.get_mut("corner").unwrap().value = ParamValue::Choice(1);
                    effect.params.get_mut("radius").unwrap().value = ParamValue::Float(80.0);
                    effect.params.get_mut("shadow").unwrap().value = ParamValue::Bool(false);
                }
                "text_animator" | "typewriter" => {
                    effect.params.get_mut("columns").unwrap().value = ParamValue::Float(7.0);
                    effect.params.get_mut("rows").unwrap().value = ParamValue::Float(5.0);
                    if id == "typewriter" {
                        effect.params.get_mut("cursor").unwrap().value = ParamValue::Bool(false);
                    }
                }
                _ => {}
            }
            let cases = if user_project_parameters {
                if id == "flare" {
                    effect.params.get_mut("amount").unwrap().value = ParamValue::Float(80.0);
                    effect.params.get_mut("angle").unwrap().value = ParamValue::Float(20.0);
                    effect.params.get_mut("color").unwrap().value = ParamValue::Color([1.0, 0.8, 0.55, 1.0]);
                }
                if id == "blur_dissolve" {
                    effect.params.get_mut("blur").unwrap().value = ParamValue::Float(40.0);
                }
                vec![effect]
            } else {
                let mut cases = vec![defaults, effect];
                cases.extend(boundary_instances(id));
                cases
            };
            for mut effect in cases {
                let directions: &[u32] = if id == "wipe" || id == "stretch_wipe" { &[0, 1, 2, 3] } else { &[0] };
                for &direction in directions {
                    if id == "wipe" || id == "stretch_wipe" {
                        effect.params.get_mut("direction").unwrap().value = ParamValue::Choice(direction);
                    }
                    for p in [0.0, 0.000001, 0.25, 0.5, 0.75, 0.999999, 1.0] {
                        let planned = evaluate(&effect, p, 0.5).unwrap();
                        assert!(TransitionCatalogStage::validate(&planned).is_ok(), "{id} p={p}: catalog rejected planned transition");
                        let mut clear = device.create_command_encoder(&Default::default());
                        {
                            let _pass = clear.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("transition-parity-clear"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &canvas,
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                        }
                        queue.submit([clear.finish()]);
                        stage.draw(&device, &queue, &canvas, [&views[0], &views[1]], (w, h), &planned).unwrap();
                        let got = read(&device, &queue, &output, w, h, format);
                        let cpu = apply_scaled(&effect, &oracle[0], &oracle[1], p, 0.5);
                        let worst = got.iter().zip(&cpu.px).map(|(g, c)| (g - c).abs()).fold(0.0f32, f32::max);
                        assert!(got.iter().all(|v| v.is_finite()), "{id} p={p}: nonfinite");
                        // The compositor accumulates in Rgba16Float (budget 0.002, about half an 8-bit code). The
                        // Rgba32Float run only checks that the kernel does the same maths, to half that budget.
                        let tolerance = if format == wgpu::TextureFormat::Rgba16Float { 0.002 } else { 1e-3 };
                        assert!(worst < tolerance, "{id} {format:?} p={p}: max RGBA delta {worst} (tolerance {tolerance})");
                    }
                }
            }
        }
    }
}
