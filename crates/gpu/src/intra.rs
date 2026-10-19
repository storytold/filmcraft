//! Intra-frame codecs decoded on the GPU ([`PixelData::Coded`](filmcraft_frame::PixelData::Coded)):
//! the compositor gets a picture as its bitstream (a few megabytes) and decodes it in compute
//! shaders into the same R16Float Y / Cb / Cr / A textures the upload path makes from CPU-decoded
//! planes, so everything after (colour management, effects, blending) is unchanged.
//!
//! ProRes (`prores.wgsl`): the headers, slice tables and slice headers are parsed and checked on
//! the CPU (`filmcraft_prores::frame_layout`, the CPU decoder's own parsing); every slice
//! component is then entropy-decoded by one invocation and every 8×8 block inverse-transformed by
//! another. Slices run in batches whose coefficient buffer fits the device's storage-binding limit.
//! A frame the device cannot hold (size limits), or whose layout does not parse, is decoded on the
//! CPU instead; one the GPU finds corrupt inside a slice is marked on the picture, which then
//! shows its CPU decode from the next composite on.

use std::sync::Arc;

use filmcraft_frame::{Chroma, CodedFormat, CodedImage};

/// Output planes are R16Float, rows padded to wgpu's copy alignment.
const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// Words of the constants buffer before the code → half table.
const F16_TABLE: usize = 320;

/// `Params` in `prores.wgsl` (24 four-byte fields).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    slice_base: u32,
    slice_count: u32,
    max_blocks: u32,
    chroma444: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    interlaced: u32,
    scale: f32,
    bias: f32,
    out_min: i32,
    out_max: i32,
    off_y: u32,
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    pitch_y: u32,
    pitch_c: u32,
    alpha: u32,
    depth: u32,
    raw_codes: u32,
    comps: u32,
    _pad: [u32; 2],
}

impl Params {
    fn bytes(&self) -> [u8; 96] {
        let w: [u32; 24] = [
            self.slice_base,
            self.slice_count,
            self.max_blocks,
            self.chroma444,
            self.width,
            self.height,
            self.cwidth,
            self.interlaced,
            self.scale.to_bits(),
            self.bias.to_bits(),
            self.out_min as u32,
            self.out_max as u32,
            self.off_y,
            self.off_cb,
            self.off_cr,
            self.off_a,
            self.pitch_y,
            self.pitch_c,
            self.alpha,
            self.depth,
            self.raw_codes,
            self.comps,
            0,
            0,
        ];
        let mut out = [0u8; 96];
        for (o, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(w) {
            o.copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// A frame decoded into textures.
pub(crate) struct Decoded {
    /// Y, Cb, Cr and A (`None` without alpha).
    pub(crate) planes: [wgpu::TextureView; 3],
    pub(crate) alpha: Option<wgpu::TextureView>,
    /// Chroma plane size.
    pub(crate) chroma: (u32, u32),
    /// Code bits of every plane.
    pub(crate) bits: u32,
    /// The textures behind the views (Y, Cb, Cr, then A when present).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) textures: Vec<wgpu::Texture>,
}

/// A buffer kept between frames, grown as needed.
struct Pooled {
    buf: Option<wgpu::Buffer>,
    usage: wgpu::BufferUsages,
    label: &'static str,
}

impl Pooled {
    fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Pooled { buf: None, usage, label }
    }
    fn get(&mut self, device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        let size = size.max(16).next_multiple_of(16);
        match &self.buf {
            Some(b) if b.size() >= size => b.clone(),
            _ => {
                let b = device.create_buffer(&wgpu::BufferDescriptor { label: Some(self.label), size, usage: self.usage, mapped_at_creation: false });
                self.buf = Some(b.clone());
                b
            }
        }
    }
}

pub(crate) struct IntraStage {
    entropy: wgpu::ComputePipeline,
    idct: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    data: Pooled,
    slices: Pooled,
    consts: Pooled,
    coeffs: Pooled,
    out: Pooled,
    /// Tests: write codes instead of half floats.
    pub(crate) raw_codes: bool,
    /// Frames decoded here / handed to the CPU instead (stats).
    pub(crate) decoded: u64,
    pub(crate) fallbacks: u64,
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn bind(buf: &wgpu::Buffer, binding: u32, size: Option<u64>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: buf, offset: 0, size: size.and_then(std::num::NonZeroU64::new) }),
    }
}

/// What the stage needs of a device.
pub(crate) fn supported(device: &wgpu::Device) -> bool {
    let l = device.limits();
    l.max_storage_buffers_per_shader_stage >= 6
        && l.max_compute_invocations_per_workgroup >= 64
        && l.max_compute_workgroup_size_x >= 64
        && l.max_compute_workgroups_per_dimension >= 1024
}

/// Bytes per row of a plane `w` samples wide (two bytes each), as wgpu copies require.
fn pitch_bytes(w: u32) -> u32 {
    (w * 2).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// 2-D workgroup counts covering `n` invocations of 64 (the shaders flatten `gid.y`).
fn groups(n: u64, max: u32) -> Option<(u32, u32)> {
    let g = n.div_ceil(64);
    if g == 0 {
        return Some((1, 1));
    }
    let x = g.min(u64::from(max));
    let y = g.div_ceil(x);
    (y <= u64::from(max)).then_some((x as u32, y as u32))
}

impl IntraStage {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-prores"),
            source: wgpu::ShaderSource::Wgsl(include_str!("prores.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("intra"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, false),
                storage(5, false),
                storage(6, false),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("intra"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("intra"),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let storage_dst = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        IntraStage {
            entropy: pipeline("entropy"),
            idct: pipeline("idct"),
            bgl,
            data: Pooled::new("intra-data", storage_dst),
            slices: Pooled::new("intra-slices", storage_dst),
            consts: Pooled::new("intra-consts", storage_dst),
            coeffs: Pooled::new("intra-coeffs", storage_dst),
            out: Pooled::new("intra-out", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC),
            raw_codes: false,
            decoded: 0,
            fallbacks: 0,
        }
    }

    /// Decode `img` (a `w`×`h` picture) into textures, or `None` when it has to be decoded on the
    /// CPU (counted in [`IntraStage::fallbacks`]).
    pub(crate) fn decode(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, img: &Arc<CodedImage>, w: u32, h: u32) -> Option<Decoded> {
        let r = match img.format() {
            CodedFormat::ProRes => self.prores(device, queue, img, w, h),
        };
        match r {
            Some(d) => {
                self.decoded += 1;
                Some(d)
            }
            None => {
                self.fallbacks += 1;
                None
            }
        }
    }

    fn prores(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, img: &Arc<CodedImage>, w: u32, h: u32) -> Option<Decoded> {
        if img.gpu_failed() {
            return None;
        }
        let data = img.data();
        let layout = match filmcraft_prores::frame_layout(data) {
            Ok(l) => l,
            Err(e) => {
                log::debug!("ProRes frame not decodable on the GPU ({e}); decoding it on the CPU");
                return None;
            }
        };
        let hdr = &layout.header;
        let chroma444 = hdr.chroma == filmcraft_prores::ChromaFormat::Yuv444;
        let depth = u32::from(layout.coded_depth);
        // the picture must be what the frame says it is (its CPU decode is the reference)
        let chroma = if chroma444 { Chroma::C444 } else { Chroma::C422 };
        if (u32::from(hdr.width), u32::from(hdr.height)) != (w, h) || img.chroma() != chroma || img.bits() != depth || img.has_alpha() != layout.has_alpha() {
            return None;
        }
        let limits = device.limits();
        if w == 0 || h == 0 || w > limits.max_texture_dimension_2d || h > limits.max_texture_dimension_2d || layout.slices.is_empty() {
            return None;
        }
        let cwidth = if chroma444 { w } else { w.div_ceil(2) };
        let (py, pc) = (pitch_bytes(w), pitch_bytes(cwidth));
        let (pyw, pcw) = (py / 4, pc / 4);
        let plane_words = |pitch_w: u32| u64::from(pitch_w) * u64::from(h);
        let off_cb = plane_words(pyw);
        let off_cr = off_cb + plane_words(pcw);
        let off_a = off_cr + plane_words(pcw);
        let out_words = off_a + if layout.has_alpha() { plane_words(pyw) } else { 0 };
        let max_binding = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let out_bytes = out_words * 4;
        if out_bytes > max_binding || u32::try_from(out_words).is_err() {
            return None;
        }
        // coefficient slots per slice: its widest macroblock count × (4 luma + 2 chroma blocks of 2 or 4)
        let max_blocks = layout.max_slice_mbs() * if chroma444 { 12 } else { 8 };
        let slot_bytes = u64::from(max_blocks) * 64 * 4;
        let nslices = layout.slices.len() as u64;
        let mut batch = (max_binding / slot_bytes).min(nslices).max(1);
        let comps: u32 = if layout.has_alpha() { 4 } else { 3 };
        let maxg = limits.max_compute_workgroups_per_dimension;
        while batch > 1 && (groups(batch * u64::from(max_blocks), maxg).is_none() || groups(batch * u64::from(comps), maxg).is_none()) {
            batch /= 2;
        }
        if slot_bytes > max_binding || data.len() as u64 + 16 > max_binding {
            return None;
        }

        // inputs
        let mut bytes = data.get(..layout.frame_size)?.to_vec();
        bytes.resize((bytes.len() + 8).next_multiple_of(4), 0);
        let mut slice_words = Vec::with_capacity(layout.slices.len() * 16);
        for s in &layout.slices {
            slice_words.extend_from_slice(&[s.mb_row, s.mb_x, u32::from(s.log2_mbs), u32::from(s.parity), s.qscale]);
            for (o, n) in s.comps {
                slice_words.extend_from_slice(&[o, n]);
            }
            slice_words.extend_from_slice(&[0, 0, 0]);
        }
        let mut consts = vec![0u32; F16_TABLE + (1usize << depth)];
        let scan = if layout.interlaced() { &filmcraft_prores::layout::tables::SCAN_INTERLACED } else { &filmcraft_prores::layout::tables::SCAN_PROGRESSIVE };
        for i in 0..64 {
            consts[i] = u32::from(hdr.qmat_luma[i]);
            consts[64 + i] = u32::from(hdr.qmat_chroma[i]);
            consts[128 + i] = u32::from(scan[i]);
        }
        for (i, b) in filmcraft_prores::layout::tables::basis().iter().flatten().enumerate() {
            consts[192 + i] = b.to_bits();
        }
        let cb = |(rice, exp, rq): (u8, u8, u8)| u32::from(rice) | u32::from(exp) << 8 | u32::from(rq) << 16;
        consts[256] = cb(filmcraft_prores::layout::tables::first_dc());
        for (i, c) in filmcraft_prores::layout::tables::dc().into_iter().enumerate() {
            consts[257 + i] = cb(c);
        }
        for (i, c) in filmcraft_prores::layout::tables::run().into_iter().enumerate() {
            consts[264 + i] = cb(c);
        }
        for (i, c) in filmcraft_prores::layout::tables::level().into_iter().enumerate() {
            consts[280 + i] = cb(c);
        }
        let table = crate::code_table(depth);
        for (c, t) in consts[F16_TABLE..].iter_mut().zip(table.iter()) {
            *c = u32::from(u16::from_le_bytes(*t));
        }

        let words = |v: &[u32]| v.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let data_buf = self.data.get(device, bytes.len() as u64);
        let slices_buf = self.slices.get(device, slice_words.len() as u64 * 4);
        let consts_buf = self.consts.get(device, consts.len() as u64 * 4);
        let coeff_buf = self.coeffs.get(device, batch * slot_bytes);
        let out_buf = self.out.get(device, out_bytes);
        queue.write_buffer(&data_buf, 0, &bytes);
        queue.write_buffer(&slices_buf, 0, &words(&slice_words));
        queue.write_buffer(&consts_buf, 0, &words(&consts));
        let status = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("intra-status"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("intra-status-read"),
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // OutConv of `filmcraft-prores` at the coded depth
        let reserved: i32 = if depth >= 10 { 4 } else { 1 };
        let base = Params {
            slice_base: 0,
            slice_count: 0,
            max_blocks,
            chroma444: chroma444 as u32,
            width: w,
            height: h,
            cwidth,
            interlaced: layout.interlaced() as u32,
            scale: (2f32).powi(depth as i32 - 12),
            bias: (1u32 << (depth - 1)) as f32 + 0.5,
            out_min: reserved,
            out_max: (1i32 << depth) - 1 - reserved,
            off_y: 0,
            off_cb: off_cb as u32,
            off_cr: off_cr as u32,
            off_a: off_a as u32,
            pitch_y: pyw,
            pitch_c: pcw,
            alpha: match hdr.alpha {
                filmcraft_prores::AlphaType::None => 0,
                filmcraft_prores::AlphaType::Bits8 => 1,
                filmcraft_prores::AlphaType::Bits16 => 2,
            },
            depth,
            raw_codes: self.raw_codes as u32,
            comps,
            _pad: [0; 2],
        };

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("intra") });
        enc.clear_buffer(&status, 0, None);
        let mut s0 = 0u64;
        while s0 < nslices {
            let count = batch.min(nslices - s0);
            let p = Params { slice_base: s0 as u32, slice_count: count as u32, ..base };
            let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("intra-params"),
                size: 96,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&ubuf, 0, &p.bytes());
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("intra"),
                layout: &self.bgl,
                entries: &[
                    bind(&ubuf, 0, None),
                    bind(&data_buf, 1, None),
                    bind(&slices_buf, 2, None),
                    bind(&consts_buf, 3, None),
                    bind(&coeff_buf, 4, Some(count * slot_bytes)),
                    bind(&out_buf, 5, Some(out_bytes.next_multiple_of(16))),
                    bind(&status, 6, None),
                ],
            });
            enc.clear_buffer(&coeff_buf, 0, Some(count * slot_bytes));
            let (ex, ey) = groups(count * u64::from(comps), maxg)?;
            let (ix, iy) = groups(count * u64::from(max_blocks), maxg)?;
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("prores-entropy"), timestamp_writes: None });
                pass.set_pipeline(&self.entropy);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(ex, ey, 1);
            }
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("prores-idct"), timestamp_writes: None });
                pass.set_pipeline(&self.idct);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(ix, iy, 1);
            }
            s0 += count;
        }

        let plane = |pw: u32, ph: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-intra-plane"),
                size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PLANE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let copy = |enc: &mut wgpu::CommandEncoder, t: &wgpu::Texture, offset_words: u64, pitch: u32, pw: u32| {
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &out_buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: offset_words * 4, bytes_per_row: Some(pitch), rows_per_image: Some(h) },
                },
                wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: pw, height: h, depth_or_array_layers: 1 },
            );
        };
        let ty = plane(w, h);
        let tcb = plane(cwidth, h);
        let tcr = plane(cwidth, h);
        copy(&mut enc, &ty, 0, py, w);
        copy(&mut enc, &tcb, off_cb, pc, cwidth);
        copy(&mut enc, &tcr, off_cr, pc, cwidth);
        let ta = layout.has_alpha().then(|| {
            let t = plane(w, h);
            copy(&mut enc, &t, off_a, py, w);
            t
        });
        enc.copy_buffer_to_buffer(&status, 0, &readback, 0, 16);
        queue.submit([enc.finish()]);

        // a corrupt slice: the picture shows its CPU decode from the next composite on
        let img = img.clone();
        let rb = readback.clone();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            if r.is_ok() {
                let bad = match rb.slice(..).get_mapped_range() {
                    Ok(m) => m.get(..4).is_some_and(|b| b != [0; 4]),
                    Err(_) => false,
                };
                rb.unmap();
                if bad {
                    log::warn!("ProRes frame: corrupt slice data on the GPU; showing its CPU decode");
                    img.set_gpu_failed();
                }
            }
        });

        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let alpha = ta.as_ref().map(view);
        let planes = [view(&ty), view(&tcb), view(&tcr)];
        Some(Decoded { planes, alpha, chroma: (cwidth, h), bits: depth, textures: [Some(ty), Some(tcb), Some(tcr), ta].into_iter().flatten().collect() })
    }
}

/// Test support: read a decoded plane back as `u16` values (codes when the stage writes
/// `raw_codes`).
#[cfg(test)]
pub(crate) fn read_plane(device: &wgpu::Device, queue: &wgpu::Queue, view_tex: &wgpu::Texture) -> Vec<u16> {
    let (w, h) = (view_tex.width(), view_tex.height());
    let pitch = pitch_bytes(w);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(pitch) * u64::from(h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: view_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pitch), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let m = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h) as usize);
    for row in m.chunks_exact(pitch as usize) {
        for px in row[..(w * 2) as usize].as_chunks::<2>().0 {
            out.push(u16::from_le_bytes([px[0], px[1]]));
        }
    }
    out
}
