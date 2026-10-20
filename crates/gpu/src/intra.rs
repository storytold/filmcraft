//! Intra-frame codecs decoded on the GPU ([`PixelData::Coded`](filmcraft_frame::PixelData::Coded)):
//! the compositor gets a picture as its bitstream (a few megabytes) and decodes it in compute
//! shaders into the same R16Float Y / Cb / Cr / A textures the upload path makes from CPU-decoded
//! planes, so everything after (colour management, effects, blending) is unchanged.
//!
//! ProRes (`prores.wgsl`): the headers, slice tables and slice headers are parsed and checked on
//! the CPU (`filmcraft_prores::frame_layout`, the CPU decoder's own parsing); every slice
//! component is then entropy-decoded by one invocation and every 8×8 block inverse-transformed by
//! another. Slices run in batches whose coefficient buffer fits the device's storage-binding limit.
//!
//! DNxHD / DNxHR (`dnx.wgsl`): the coding units and scan indices are parsed and checked on the CPU
//! (`filmcraft_dnx::frame_layout`); every macroblock scan line is cut into segments that are
//! entropy-decoded in parallel and stitched together on the GPU (see the shader and ADR 0002),
//! then every block is inverse-transformed as for ProRes.
//!
//! A frame the device cannot hold (size limits), or whose layout does not parse, is decoded on the
//! CPU instead; one the GPU finds corrupt inside its entropy-coded data is marked on the picture,
//! which then shows its CPU decode from the next composite on.

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
    /// `dnx.wgsl`'s passes, in order (sync, trans, scan, decode, idct), and their layout (the
    /// intra layout plus the work buffer).
    dnx: [wgpu::ComputePipeline; 5],
    bgl: wgpu::BindGroupLayout,
    dnx_bgl: wgpu::BindGroupLayout,
    data: Pooled,
    slices: Pooled,
    consts: Pooled,
    coeffs: Pooled,
    out: Pooled,
    work: Pooled,
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

/// `dnx.wgsl`: the VLC words its workgroup table holds and the workgroup memory it takes; the
/// words of a transition record and of a decoder state.
const DNX_VLC_WORDS: usize = 3584;
const DNX_WORKGROUP_BYTES: u32 = 3840 * 4;
const DNX_TRANS_WORDS: u64 = 6;
const DNX_STATE_WORDS: u64 = 8;
/// Segment length (bits) a DNx scan line is cut into for parallel decoding, the warm-up a
/// segment's sync parse starts before it, and how many candidates a transition parse may pass
/// without landing. Measured on DNxHD and DNxHR streams (ffmpeg's and ours, 1080 to UHD, LB to
/// HQX): with 256 bits of warm-up 90–97 % of candidates are true block boundaries, and no chain
/// needed more than 4 candidates.
const DNX_SEG_BITS: u32 = 1024;
const DNX_WARM_BITS: u32 = 256;
const DNX_CAP: u32 = 8;

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
        let dnx_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("intra-dnx"),
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
                storage(7, false),
            ],
        });
        let dnx_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("intra-dnx"),
            bind_group_layouts: &[Some(&dnx_bgl)],
            immediate_size: 0,
        });
        let dnx = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-dnx"),
            source: wgpu::ShaderSource::Wgsl(include_str!("dnx.wgsl").into()),
        });
        let pipeline = |layout: &wgpu::PipelineLayout, module: &wgpu::ShaderModule, entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("intra"),
                layout: Some(layout),
                module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let storage_dst = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        IntraStage {
            entropy: pipeline(&pl, &shader, "entropy"),
            idct: pipeline(&pl, &shader, "idct"),
            dnx: ["sync", "trans", "scan", "decode", "idct"].map(|e| pipeline(&dnx_pl, &dnx, e)),
            bgl,
            dnx_bgl,
            data: Pooled::new("intra-data", storage_dst),
            slices: Pooled::new("intra-slices", storage_dst),
            consts: Pooled::new("intra-consts", storage_dst),
            coeffs: Pooled::new("intra-coeffs", storage_dst),
            out: Pooled::new("intra-out", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC),
            work: Pooled::new("intra-work", storage_dst),
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
            CodedFormat::Dnx => self.dnx(device, queue, img, w, h),
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
        let (status, readback) = status_buffers(device);

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
            let bufs = [&data_buf, &slices_buf, &consts_buf, &coeff_buf, &out_buf, &status];
            let bg = self.bind_group(device, queue, &p.bytes(), bufs, count * slot_bytes, out_bytes);
            enc.clear_buffer(&coeff_buf, 0, Some(count * slot_bytes));
            let entropy = groups(count * u64::from(comps), maxg)?;
            let idct = groups(count * u64::from(max_blocks), maxg)?;
            dispatch(&mut enc, &[(&self.entropy, "prores-entropy", entropy), (&self.idct, "prores-idct", idct)], &bg);
            s0 += count;
        }

        let mut planes = out_planes(device, &mut enc, &out_buf, &[(0, py, w, h), (off_cb, pc, cwidth, h), (off_cr, pc, cwidth, h)]);
        let ta = layout.has_alpha().then(|| out_planes(device, &mut enc, &out_buf, &[(off_a, py, w, h)]).pop()).flatten();
        enc.copy_buffer_to_buffer(&status, 0, &readback, 0, 16);
        queue.submit([enc.finish()]);
        watch_status(readback, img, "ProRes");
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let tcr = planes.pop()?;
        let tcb = planes.pop()?;
        let ty = planes.pop()?;
        let alpha = ta.as_ref().map(view);
        let planes = [view(&ty), view(&tcb), view(&tcr)];
        Some(Decoded { planes, alpha, chroma: (cwidth, h), bits: depth, textures: [Some(ty), Some(tcb), Some(tcr), ta].into_iter().flatten().collect() })
    }

    fn dnx(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, img: &Arc<CodedImage>, w: u32, h: u32) -> Option<Decoded> {
        use filmcraft_dnx::ChromaFormat;
        if img.gpu_failed() {
            return None;
        }
        let data = img.data();
        let layout = match filmcraft_dnx::frame_layout(data) {
            Ok(l) => l,
            Err(e) => {
                log::debug!("DNx frame not decodable on the GPU ({e}); decoding it on the CPU");
                return None;
            }
        };
        if !layout.plain_ycbcr() {
            return None;
        }
        let hdr = &layout.header;
        let depth = u32::from(hdr.bit_depth);
        let (chroma, bpm, cmb_h, cheight) = match hdr.chroma {
            ChromaFormat::Yuv422 => (Chroma::C422, 8u32, 16u32, h),
            // (the shaders handle 4:2:0 too, but neither our encoder nor ffmpeg's writes it, so
            // nothing tests it: decoded on the CPU)
            ChromaFormat::Yuv420 | ChromaFormat::Yuv444 => return None,
        };
        // the picture must be what the frame says it is (its CPU decode is the reference)
        if (layout.width, layout.height) != (w, h) || img.chroma() != chroma || img.bits() != depth || img.has_alpha() || !(8..=12).contains(&depth) {
            return None;
        }
        let tables = filmcraft_dnx::layout::tables::cid(hdr.cid)?;
        // the shader looks codes up in a 32-bit window
        if [tables.amp, tables.run, tables.dc].iter().any(|t| t.max_len() > 32 || t.bits == 0) {
            return None;
        }
        let limits = device.limits();
        if w == 0
            || h == 0
            || w > limits.max_texture_dimension_2d
            || h > limits.max_texture_dimension_2d
            || layout.rows.is_empty()
            || limits.max_storage_buffers_per_shader_stage < 7
        {
            return None;
        }
        let cwidth = w.div_ceil(2);
        let (py, pc) = (pitch_bytes(w), pitch_bytes(cwidth));
        let (pyw, pcw) = (py / 4, pc / 4);
        let off_cb = u64::from(pyw) * u64::from(h);
        let off_cr = off_cb + u64::from(pcw) * u64::from(cheight);
        let out_words = off_cr + u64::from(pcw) * u64::from(cheight);
        let max_binding = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let out_bytes = out_words * 4;
        if out_bytes > max_binding || u32::try_from(out_words).is_err() {
            return None;
        }
        // coefficient slots per scan line: every block of every macroblock
        let nw = layout.mb_width();
        let row_blocks = u64::from(nw) * u64::from(bpm);
        let slot_bytes = row_blocks * 64 * 4;
        let nrows = layout.rows.len() as u64;
        let maxg = limits.max_compute_workgroups_per_dimension;
        let mut batch = (max_binding / slot_bytes.max(1)).min(nrows).max(1);
        while batch > 1 && groups(batch * row_blocks, maxg).is_none() {
            batch /= 2;
        }
        // (the shaders address the stream in bits, in 32-bit words)
        if slot_bytes > max_binding || data.len() as u64 + 16 > max_binding || (data.len() as u64 + 16) * 8 > u64::from(u32::MAX) {
            return None;
        }

        // inputs
        let mut bytes = data.to_vec();
        bytes.resize((bytes.len() + 8).next_multiple_of(4), 0);
        let mut row_words = Vec::with_capacity(layout.rows.len() * 8);
        for r in &layout.rows {
            row_words.extend_from_slice(&[r.data.0, r.data.1, r.mb_row, r.first_line, r.line_step, 0, 0, 0]);
        }
        // every line cut into segments: the line of each segment, the first segment of each line
        let mut row_seg = Vec::with_capacity(layout.rows.len() + 1);
        let mut seg_row = Vec::new();
        for (i, r) in layout.rows.iter().enumerate() {
            row_seg.push(seg_row.len() as u32);
            let n = (u64::from(r.data.1) * 8).div_ceil(u64::from(DNX_SEG_BITS)).max(1);
            seg_row.extend(std::iter::repeat_n(i as u32, n as usize));
        }
        row_seg.push(seg_row.len() as u32);
        let nsegs = seg_row.len() as u64;
        let segrow_off = row_words.len() as u32;
        row_words.extend_from_slice(&seg_row);
        let rowseg_off = row_words.len() as u32;
        row_words.extend_from_slice(&row_seg);
        let w_trans = nsegs;
        let w_state = w_trans + nsegs * u64::from(bpm) * DNX_TRANS_WORDS;
        let work_bytes = (w_state + nsegs * DNX_STATE_WORDS) * 4;
        if work_bytes > max_binding || u32::try_from(work_bytes / 4).is_err() || groups(nsegs * u64::from(bpm), maxg).is_none() {
            return None;
        }
        let mut consts = vec![0u32; 264];
        for (c, z) in consts.iter_mut().zip(filmcraft_dnx::layout::tables::zigzag()) {
            *c = u32::from(z);
        }
        for (c, b) in consts[64..128].iter_mut().zip(filmcraft_dnx::layout::tables::basis().iter().flatten()) {
            *c = b.to_bits();
        }
        for (k, wts) in tables.weights.iter().enumerate() {
            for (c, &wt) in consts[128 + 64 * k..].iter_mut().zip(wts.iter()) {
                *c = u32::from(wt);
            }
        }
        // block order of a macroblock: (plane, x, y), as `filmcraft-dnx` decodes it
        let blocks = [(0, 0, 0), (0, 8, 0), (1, 0, 0), (2, 0, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (2, 0, 8)];
        for (c, &(p, x, y)) in consts[256..].iter_mut().zip(&blocks) {
            *c = p | x << 8 | y << 16;
        }
        // the VLC tables, which the entropy pass copies into workgroup memory
        let vlc_base = consts.len();
        let mut table = |t: &[u32]| {
            let at = (consts.len() - vlc_base) as u32;
            consts.extend_from_slice(t);
            at
        };
        let (amp_p, amp_s) = (table(tables.amp.primary), table(tables.amp.secondary));
        let (run_p, run_s) = (table(tables.run.primary), table(tables.run.secondary));
        let (dc_p, dc_s) = (table(tables.dc.primary), table(tables.dc.secondary));
        let vlc_len = consts.len() - vlc_base;
        if vlc_len > DNX_VLC_WORDS || limits.max_compute_workgroup_storage_size < DNX_WORKGROUP_BYTES {
            return None;
        }
        let f16 = consts.len() as u32;
        consts.extend(crate::code_table(depth).iter().take(1 << depth).map(|t| u32::from(u16::from_le_bytes(*t))));

        let words = |v: &[u32]| v.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let data_buf = self.data.get(device, bytes.len() as u64);
        let rows_buf = self.slices.get(device, row_words.len() as u64 * 4);
        let consts_buf = self.consts.get(device, consts.len() as u64 * 4);
        let coeff_buf = self.coeffs.get(device, batch * slot_bytes);
        let out_buf = self.out.get(device, out_bytes);
        let work_buf = self.work.get(device, work_bytes);
        queue.write_buffer(&data_buf, 0, &bytes);
        queue.write_buffer(&rows_buf, 0, &words(&row_words));
        queue.write_buffer(&consts_buf, 0, &words(&consts));
        let (status, readback) = status_buffers(device);

        // `Params` of `dnx.wgsl`
        let p = cid_p(tables.p)?;
        let params = |row_base: u32, row_count: u32, seg_base: u32, seg_count: u32| -> [u32; 44] {
            [
                row_base,
                row_count,
                nw,
                bpm,
                w,
                h,
                cwidth,
                cheight,
                cmb_h,
                if depth == 8 { 4 } else { 6 },
                (1.0 / (4 * p) as f32).to_bits(),
                if depth == 12 { 4f32 } else { 1.0 }.to_bits(),
                ((1u32 << (depth - 1)) as f32 + 0.5).to_bits(),
                (1u32 << depth) - 1,
                0,
                off_cb as u32,
                off_cr as u32,
                pyw,
                pcw,
                self.raw_codes as u32,
                amp_p,
                amp_s,
                tables.amp.bits,
                run_p,
                run_s,
                tables.run.bits,
                dc_p,
                dc_s,
                tables.dc.bits,
                f16,
                vlc_base as u32,
                vlc_len as u32,
                seg_base,
                seg_count,
                DNX_SEG_BITS,
                DNX_WARM_BITS,
                DNX_CAP,
                segrow_off,
                rowseg_off,
                w_trans as u32,
                w_state as u32,
                0,
                0,
                0,
            ]
        };

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("intra") });
        enc.clear_buffer(&status, 0, None);
        // (a segment the scan does not reach keeps a zeroed state: it decodes nothing)
        enc.clear_buffer(&work_buf, 0, Some(work_bytes.next_multiple_of(4)));
        let mut r0 = 0u64;
        while r0 < nrows {
            let count = batch.min(nrows - r0);
            let seg0 = u64::from(*row_seg.get(r0 as usize)?);
            let segs = u64::from(*row_seg.get((r0 + count) as usize)?) - seg0;
            let p = params(r0 as u32, count as u32, seg0 as u32, segs as u32);
            let ubuf = uniform(device, queue, &words(&p));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("intra-dnx"),
                layout: &self.dnx_bgl,
                entries: &[
                    bind(&ubuf, 0, None),
                    bind(&data_buf, 1, None),
                    bind(&rows_buf, 2, None),
                    bind(&consts_buf, 3, None),
                    bind(&coeff_buf, 4, Some(count * slot_bytes)),
                    bind(&out_buf, 5, Some(out_bytes.next_multiple_of(16))),
                    bind(&status, 6, None),
                    bind(&work_buf, 7, Some(work_bytes.next_multiple_of(16))),
                ],
            });
            enc.clear_buffer(&coeff_buf, 0, Some(count * slot_bytes));
            let [sync, trans, scan, decode, idct] = &self.dnx;
            dispatch(
                &mut enc,
                &[
                    (sync, "dnx-sync", groups(segs, maxg)?),
                    (trans, "dnx-trans", groups(segs * u64::from(bpm), maxg)?),
                    (scan, "dnx-scan", groups(count, maxg)?),
                    (decode, "dnx-decode", groups(segs, maxg)?),
                    (idct, "dnx-idct", groups(count * row_blocks, maxg)?),
                ],
                &bg,
            );
            r0 += count;
        }
        let mut planes = out_planes(device, &mut enc, &out_buf, &[(0, py, w, h), (off_cb, pc, cwidth, cheight), (off_cr, pc, cwidth, cheight)]);
        enc.copy_buffer_to_buffer(&status, 0, &readback, 0, 16);
        queue.submit([enc.finish()]);
        watch_status(readback, img, "DNx");
        let view = |t: &wgpu::Texture| t.create_view(&Default::default());
        let tcr = planes.pop()?;
        let tcb = planes.pop()?;
        let ty = planes.pop()?;
        let planes = [view(&ty), view(&tcb), view(&tcr)];
        Some(Decoded { planes, alpha: None, chroma: (cwidth, cheight), bits: depth, textures: vec![ty, tcb, tcr] })
    }

    /// The bind group of one batch: its parameters, then `bufs` (bitstream, unit records,
    /// constants, coefficients, output, status) with the batch's coefficient and output sizes.
    fn bind_group(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &[u8],
        bufs: [&wgpu::Buffer; 6],
        coeff_bytes: u64,
        out_bytes: u64,
    ) -> wgpu::BindGroup {
        let ubuf = uniform(device, queue, params);
        let [data, units, consts, coeffs, out, status] = bufs;
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("intra"),
            layout: &self.bgl,
            entries: &[
                bind(&ubuf, 0, None),
                bind(data, 1, None),
                bind(units, 2, None),
                bind(consts, 3, None),
                bind(coeffs, 4, Some(coeff_bytes)),
                bind(out, 5, Some(out_bytes.next_multiple_of(16))),
                bind(status, 6, None),
            ],
        })
    }
}

/// VC-3's inverse quantisation parameter, as the shader needs it (8 or 32).
fn cid_p(p: u32) -> Option<u32> {
    matches!(p, 8 | 32).then_some(p)
}

/// A uniform buffer holding `params`.
fn uniform(device: &wgpu::Device, queue: &wgpu::Queue, params: &[u8]) -> wgpu::Buffer {
    let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("intra-params"),
        size: params.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&ubuf, 0, params);
    ubuf
}

/// The corrupt-data counter the shaders write and its readback buffer.
fn status_buffers(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::Buffer) {
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
    (status, readback)
}

/// Run the passes of one batch, in order, over `bg` (each with its workgroup counts).
fn dispatch(enc: &mut wgpu::CommandEncoder, passes: &[(&wgpu::ComputePipeline, &str, (u32, u32))], bg: &wgpu::BindGroup) {
    for &(pipeline, label, (x, y)) in passes {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(label), timestamp_writes: None });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bg, &[]);
        pass.dispatch_workgroups(x, y, 1);
    }
}

/// Textures of the packed output planes `(word offset, row pitch in bytes, width, height)`.
fn out_planes(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, out: &wgpu::Buffer, planes: &[(u64, u32, u32, u32)]) -> Vec<wgpu::Texture> {
    planes
        .iter()
        .map(|&(offset_words, pitch, pw, ph)| {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-intra-plane"),
                size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PLANE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: out,
                    layout: wgpu::TexelCopyBufferLayout { offset: offset_words * 4, bytes_per_row: Some(pitch), rows_per_image: Some(ph) },
                },
                wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
            );
            t
        })
        .collect()
}

/// Corrupt data found by the shaders: the picture shows its CPU decode from the next composite on.
fn watch_status(readback: wgpu::Buffer, img: &Arc<CodedImage>, codec: &'static str) {
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
                log::warn!("{codec} frame: corrupt data on the GPU; showing its CPU decode");
                img.set_gpu_failed();
            }
        }
    });
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
