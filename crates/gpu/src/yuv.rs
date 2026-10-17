//! Export frames as 8-bit BT.709 limited-range 4:2:0 straight from the linear premultiplied
//! accumulator: what `filmcraft_export::rgba_to_yuv420_8` makes of `Image::over_black_rgba8`, done
//! on the GPU so only the planes (1.5 bytes a pixel instead of the accumulator's 8) are read back.
//! The sRGB step uses the same 4096-entry table and index as `filmcraft_color::linear_to_srgb_u8`;
//! the Y'CbCr step the same formulas, accumulation order and rounding. The contract with the CPU
//! path is ±1 code per byte (WGSL division and fused multiply-adds may differ in the last ulp).

use wgpu::util::DeviceExt;

/// Where the planes are in the output buffer: Y rows of `y_stride` bytes (`groups_x` × 8), then
/// U and V rows of `c_stride` bytes (`groups_x` × 4), `groups_y` = ceil(h / 2) chroma rows each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub w: u32,
    pub h: u32,
    pub y_stride: u32,
    pub c_stride: u32,
    pub u_off: u32,
    pub v_off: u32,
    pub groups_x: u32,
    pub groups_y: u32,
    pub bytes: u64,
}

/// The layout of a `w` × `h` frame, or None when it is empty, overflows the shader's u32 offsets
/// or needs a larger storage binding than `max_binding`.
pub(crate) fn layout(w: u32, h: u32, max_binding: u64) -> Option<Layout> {
    if w == 0 || h == 0 {
        return None;
    }
    let (groups_x, groups_y) = (w.div_ceil(8), h.div_ceil(2));
    let y_stride = groups_x.checked_mul(8)?;
    let c_stride = groups_x.checked_mul(4)?;
    let u_off = y_stride.checked_mul(h)?;
    let v_off = u_off.checked_add(c_stride.checked_mul(groups_y)?)?;
    let end = v_off.checked_add(c_stride.checked_mul(groups_y)?)?;
    let bytes = u64::from(end);
    (bytes <= max_binding).then_some(Layout { w, h, y_stride, c_stride, u_off, v_off, groups_x, groups_y, bytes })
}

/// The tight Y, U and V planes (`w` × `h`, `ceil(w/2)` × `ceil(h/2)`) of a read-back buffer.
pub(crate) fn unpack(data: &[u8], l: &Layout) -> Option<[Vec<u8>; 3]> {
    let (w, h) = (l.w as usize, l.h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (ys, cs) = (l.y_stride as usize, l.c_stride as usize);
    let rows = |off: usize, stride: usize, width: usize, n: usize| -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(width.checked_mul(n)?);
        for r in 0..n {
            let at = off.checked_add(r.checked_mul(stride)?)?;
            out.extend_from_slice(data.get(at..at.checked_add(width)?)?);
        }
        Some(out)
    };
    Some([rows(0, ys, w, h)?, rows(l.u_off as usize, cs, cw, ch)?, rows(l.v_off as usize, cs, cw, ch)?])
}

/// The compute pass, its sRGB table and the (grown, reused) output and staging buffers.
pub(crate) struct YuvPass {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut: wgpu::Buffer,
    out: Option<(wgpu::Buffer, u64)>,
    staging: Option<(wgpu::Buffer, u64)>,
}

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None }
}

fn grown(device: &wgpu::Device, slot: &mut Option<(wgpu::Buffer, u64)>, size: u64, usage: wgpu::BufferUsages, label: &str) {
    if slot.as_ref().is_none_or(|(_, s)| *s < size) {
        *slot = Some((device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false }), size));
    }
}

impl YuvPass {
    /// The pass, or None when this device cannot build it.
    pub(crate) fn try_new(device: &wgpu::Device) -> Option<Self> {
        Self::try_new_from(device, include_str!("yuv.wgsl"))
    }

    /// [`try_new`](Self::try_new) for a given shader source. The pipeline is built inside a
    /// validation error scope: a device that rejects it (a backend whose shader compiler or
    /// storage-buffer support differs) gives None instead of an uncaptured error, which would
    /// retire the shared export device and with it all GPU export rendering.
    pub(crate) fn try_new_from(device: &wgpu::Device, source: &str) -> Option<Self> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pass = Self::build(device, source);
        match crate::poll_now(scope.pop()) {
            Some(None) => Some(pass),
            Some(Some(e)) => {
                log::warn!("GPU 4:2:0 export output unavailable on this device, converting on the CPU: {e}");
                None
            }
            None => None,
        }
    }

    fn build(device: &wgpu::Device, source: &str) -> Self {
        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("filmcraft-yuv"), source: wgpu::ShaderSource::Wgsl(source.into()) });
        let buf =
            |read_only| wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv"),
            entries: &[
                entry(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                entry(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(2, buf(true)),
                entry(3, buf(false)),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("yuv"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("yuv"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let table: Vec<u8> = filmcraft_color::linear_to_srgb_u8_table().iter().flat_map(|v| u32::from(*v).to_le_bytes()).collect();
        let lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuv-srgb-table"),
            contents: &table,
            usage: wgpu::BufferUsages::STORAGE,
        });
        Self { pipeline, bgl, lut, out: None, staging: None }
    }

    /// Record the conversion of the `w` × `h` accumulator and its copy into the staging buffer.
    pub(crate) fn record(&mut self, device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, accum: &wgpu::TextureView, w: u32, h: u32) -> Option<Layout> {
        let limits = device.limits();
        let l = layout(w, h, limits.max_storage_buffer_binding_size)?;
        let gx = l.groups_x.div_ceil(8);
        let gy = l.groups_y.div_ceil(8);
        if gx > limits.max_compute_workgroups_per_dimension || gy > limits.max_compute_workgroups_per_dimension {
            return None;
        }
        grown(device, &mut self.out, l.bytes, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, "yuv-out");
        grown(device, &mut self.staging, l.bytes, wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, "yuv-staging");
        let params: Vec<u8> = [l.w, l.h, l.y_stride, l.c_stride, l.u_off, l.v_off, l.groups_x, l.groups_y].iter().flat_map(|v| v.to_le_bytes()).collect();
        let ub =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("yuv-params"), contents: &params, usage: wgpu::BufferUsages::UNIFORM });
        let (out, _) = self.out.as_ref()?;
        let (staging, _) = self.staging.as_ref()?;
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(accum) },
                wgpu::BindGroupEntry { binding: 2, resource: self.lut.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: out, offset: 0, size: wgpu::BufferSize::new(l.bytes) }),
                },
            ],
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("yuv"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        enc.copy_buffer_to_buffer(out, 0, staging, 0, l.bytes);
        Some(l)
    }

    /// Wait for the recorded pass (submitted by the caller) and unpack its planes.
    pub(crate) fn read(&self, device: &wgpu::Device, l: &Layout) -> Option<[Vec<u8>; 3]> {
        let (staging, _) = self.staging.as_ref()?;
        let slice = staging.slice(..l.bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        crate::wait_mapped(device)?;
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let planes = unpack(&data, l);
        drop(data);
        staging.unmap();
        planes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_packs_8x2_groups() {
        let l = layout(641, 361, u64::MAX).unwrap();
        assert_eq!((l.groups_x, l.groups_y), (81, 181));
        assert_eq!((l.y_stride, l.c_stride), (648, 324));
        assert_eq!(l.u_off, 648 * 361);
        assert_eq!(l.v_off, l.u_off + 324 * 181);
        assert_eq!(l.bytes, u64::from(l.v_off) + 324 * 181);
        assert_eq!(l.u_off % 4, 0);
        assert_eq!(l.v_off % 4, 0);
        let one = layout(1, 1, u64::MAX).unwrap();
        assert_eq!((one.groups_x, one.groups_y, one.y_stride, one.c_stride, one.bytes), (1, 1, 8, 4, 16));
    }

    #[test]
    fn layout_rejects_what_the_device_cannot_bind() {
        assert!(layout(0, 1080, u64::MAX).is_none());
        assert!(layout(1920, 0, u64::MAX).is_none());
        assert!(layout(3840, 2160, 1 << 20).is_none(), "12 MB of planes over a 1 MB binding limit");
        assert!(layout(u32::MAX, u32::MAX, u64::MAX).is_none(), "offsets overflow u32");
    }

    #[test]
    fn unpack_cuts_the_padding() {
        let l = layout(3, 3, u64::MAX).unwrap(); // y_stride 8, 3 rows; c_stride 4, 2 rows
        let mut data = vec![0u8; l.bytes as usize];
        for r in 0..3 {
            for x in 0..8 {
                data[r * 8 + x] = (10 * r + x) as u8;
            }
        }
        for r in 0..2 {
            for x in 0..4 {
                data[l.u_off as usize + r * 4 + x] = (100 + 10 * r + x) as u8;
                data[l.v_off as usize + r * 4 + x] = (200 + 10 * r + x) as u8;
            }
        }
        let [y, u, v] = unpack(&data, &l).unwrap();
        assert_eq!(y, vec![0, 1, 2, 10, 11, 12, 20, 21, 22]);
        assert_eq!(u, vec![100, 101, 110, 111]);
        assert_eq!(v, vec![200, 201, 210, 211]);
        assert!(unpack(&data[..10], &l).is_none(), "short readback");
    }

    /// CPU reference for an accumulator of linear premultiplied RGBA (as f16 bits): what the
    /// export pipeline makes of it today.
    fn reference(px16: &[u16], w: usize, h: usize) -> [Vec<u8>; 3] {
        let px: Vec<f32> = px16.iter().map(|b| crate::f16_to_f32(*b)).collect();
        let img = filmcraft_render::Image { w, h, px };
        let rgba = img.over_black_rgba8();
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        filmcraft_export::rgba_to_yuv420_8(&rgba, w, h, &mut y, &mut u, &mut v);
        [y, u, v]
    }

    /// Run the pass on an Rgba16Float texture holding `px16`; None without an adapter.
    fn gpu(px16: &[u16], w: u32, h: u32) -> Option<[Vec<u8>; 3]> {
        // one device for every test here: creating Vulkan devices on several test threads at once
        // is what crashes RADV in this crate's tests
        static DEVICE: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();
        let (device, queue) = DEVICE.get_or_init(crate::blend_tests::device).clone()?;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yuv-test-accum"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = px16.iter().flat_map(|b| b.to_le_bytes()).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let view = tex.create_view(&Default::default());
        let mut pass = YuvPass::try_new(&device)?;
        let mut enc = device.create_command_encoder(&Default::default());
        let l = pass.record(&device, &mut enc, &view, w, h)?;
        queue.submit([enc.finish()]);
        pass.read(&device, &l)
    }

    /// Every byte within ±1; returns the share of exact bytes.
    fn compare(px16: &[u16], w: u32, h: u32, what: &str) -> Option<f64> {
        let got = gpu(px16, w, h)?;
        let want = reference(px16, w as usize, h as usize);
        let (mut exact, mut total) = (0usize, 0usize);
        for (k, (g, r)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.len(), r.len(), "{what}: plane {k} size");
            for (i, (a, b)) in g.iter().zip(r).enumerate() {
                assert!(a.abs_diff(*b) <= 1, "{what}: plane {k} byte {i}: GPU {a} CPU {b}");
                exact += usize::from(a == b);
                total += 1;
            }
        }
        let share = exact as f64 / total.max(1) as f64;
        eprintln!("{what}: {:.4}% of bytes exact", 100.0 * share);
        Some(share)
    }

    fn rgba16(px: impl IntoIterator<Item = [f32; 4]>) -> Vec<u16> {
        px.into_iter().flat_map(|p| p.map(crate::f32_to_f16)).collect()
    }

    /// A pass the device cannot build (here: a shader that does not compile) is None, not an
    /// uncaptured validation error, which would retire the shared export device (and panics on
    /// this test device); a valid pass on the same device still works afterwards.
    #[test]
    fn a_pass_the_device_rejects_is_contained() {
        static DEVICE: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();
        let Some((device, _)) = DEVICE.get_or_init(crate::blend_tests::device).clone() else {
            eprintln!("SKIPPED (gpu yuv): no adapter");
            return;
        };
        assert!(YuvPass::try_new_from(&device, "this is not WGSL").is_none());
        assert!(YuvPass::try_new(&device).is_some(), "the real shader builds");
    }

    #[test]
    fn yuv_pass_matches_cpu_reference_on_solid_colours() {
        for (name, c) in [
            ("black", [0.0, 0.0, 0.0, 1.0]),
            ("white", [1.0, 1.0, 1.0, 1.0]),
            ("grey", [0.214, 0.214, 0.214, 1.0]),
            ("red", [1.0, 0.0, 0.0, 1.0]),
            ("green", [0.0, 1.0, 0.0, 1.0]),
            ("blue", [0.0, 0.0, 1.0, 1.0]),
            ("half-transparent orange", [0.5, 0.2, 0.0, 0.5]),
        ] {
            if compare(&rgba16(std::iter::repeat_n(c, 64 * 32)), 64, 32, name).is_none() {
                eprintln!("SKIPPED (gpu yuv): no adapter");
                return;
            }
        }
    }

    #[test]
    fn yuv_pass_matches_cpu_reference_on_the_full_ramp() {
        // every LUT index and then some: linear -0.1 ..= 1.2 across 8192 pixels, per channel offset
        let w = 8192u32;
        let px = (0..w).map(|x| {
            let v = -0.1 + 1.3 * x as f32 / (w - 1) as f32;
            [v, (v * 0.7 + 0.1).min(1.2), 1.1 - v, 1.0]
        });
        let px16 = rgba16(px.clone().chain(px));
        match compare(&px16, w, 2, "ramp") {
            Some(share) => assert!(share > 0.99, "ramp exact share {share}"),
            None => eprintln!("SKIPPED (gpu yuv): no adapter"),
        }
    }

    #[test]
    fn yuv_pass_matches_cpu_reference_at_odd_sizes() {
        for (w, h) in [(1u32, 1u32), (2, 2), (3, 3), (641, 361), (1918, 1080)] {
            let px = (0..w * h).map(|i| {
                let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                [x, y, (x * y).sqrt(), 0.25 + 0.75 * y]
            });
            if compare(&rgba16(px), w, h, &format!("{w}x{h}")).is_none() {
                eprintln!("SKIPPED (gpu yuv): no adapter");
                return;
            }
        }
    }
}
