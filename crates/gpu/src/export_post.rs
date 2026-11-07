//! The export's post-render work on the GPU, for a frame that stays there (ADR 0003 Step 2):
//! [`PostPass`] turns the compositor's accumulator into the Y'CbCr planes the ProRes and DNxHR
//! encoders read (`export_post.wgsl`: placement and scaling, the output transfer function, the
//! video limiter, the matrix, chroma subsampling, the alpha plane), into a buffer on the same
//! device. [`ResidentPlanes`] is that buffer with its geometry: the handle the export passes from
//! the renderer to the encoder without reading it back (`SliceBackend::load_resident`,
//! `MacroblockBackend::load_resident`). The CPU export (`filmcraft_export::Pipeline::frame` and the
//! encoders' conversion) is the reference this follows step for step.

use crate::intra::{bind, storage, uniform};
use crate::intra_enc::{le_bytes, max_binding};

/// The widest filter the scaling pass runs: a picture reduced more than this many times in either
/// direction is post-processed on the CPU (each output sample would read too many).
pub const MAX_REDUCTION: f32 = 32.0;

/// What the export does to a rendered frame before the encoder codes it (the GPU side's copy of
/// `filmcraft_export::PostSettings`, which this crate cannot name; the apps fill one from the other).
#[derive(Clone, Debug, PartialEq)]
pub struct PostConfig {
    /// Size the renderer draws the frame at.
    pub rendered: (u32, u32),
    /// The output frame.
    pub width: u32,
    pub height: u32,
    /// The picture's size in the output frame, resampled to it (`None`: as rendered).
    pub content: Option<(u32, u32)>,
    /// The picture's top-left in the output frame (negative: cropped).
    pub offset: (i64, i64),
    /// The video limiter's luma floor and ceiling in percent.
    pub limiter: Option<(f32, f32)>,
    /// An HDR signal (PQ / HLG): not run here.
    pub hdr: bool,
    /// The encoded signal kept as floats (`true`), or quantised to 8 bits first.
    pub floats: bool,
    /// Straight colour and an alpha plane (`false`: the colour over black, no alpha plane).
    pub alpha: bool,
    /// The Y'CbCr matrix's luma coefficients for red and blue.
    pub kr: f32,
    pub kb: f32,
    /// 4:2:2 (`true`) or 4:4:4 chroma.
    pub chroma_422: bool,
    /// Bits per sample of the planes (10 only).
    pub bit_depth: u8,
    /// An image, name or timecode overlay is drawn over the frame: not run here.
    pub overlays: bool,
}

impl PostConfig {
    /// Chroma samples per row.
    fn cwidth(&self) -> u32 {
        if self.chroma_422 { self.width.div_ceil(2) } else { self.width }
    }

    /// Samples of the planes: Y, Cb, Cr and alpha, one after the other (saturating: a size this
    /// large is refused by [`Self::supported`]).
    fn samples(&self) -> u64 {
        let luma = u64::from(self.width).saturating_mul(u64::from(self.height));
        let chroma = u64::from(self.cwidth()).saturating_mul(u64::from(self.height));
        luma.saturating_add(chroma.saturating_mul(2)).saturating_add(if self.alpha { luma } else { 0 })
    }

    /// Bytes of the planes buffer (16-bit samples, padded to a word; saturating).
    pub fn plane_bytes(&self) -> u64 {
        self.samples().saturating_mul(2).checked_next_multiple_of(4).unwrap_or(u64::MAX)
    }

    /// The picture's size in the output frame.
    fn content_size(&self) -> (u32, u32) {
        self.content.unwrap_or(self.rendered)
    }

    /// Whether [`PostPass`] runs every step of this configuration on a device with `limits`:
    /// no overlay, no HDR signal, 10-bit planes, a picture reduced at most [`MAX_REDUCTION`]
    /// times, and buffers and textures within the limits.
    pub fn supported(&self, limits: &wgpu::Limits) -> bool {
        let (cw, ch) = self.content_size();
        let (rw, rh) = self.rendered;
        let sizes = [self.width, self.height, cw, ch, rw, rh];
        let tex = limits.max_texture_dimension_2d;
        let reduction_ok = rw as f32 / cw.max(1) as f32 <= MAX_REDUCTION && rh as f32 / ch.max(1) as f32 <= MAX_REDUCTION;
        let offset_ok = i32::try_from(self.offset.0).is_ok() && i32::try_from(self.offset.1).is_ok();
        let groups = |n: u32| n.div_ceil(8) <= limits.max_compute_workgroups_per_dimension;
        let bytes = self.plane_bytes();
        !self.overlays
            && !self.hdr
            && self.bit_depth == 10
            && sizes.iter().all(|&s| s > 0 && s <= tex)
            && reduction_ok
            && offset_ok
            && groups(self.width.div_ceil(2))
            && groups(self.height)
            && bytes <= max_binding(limits)
            // (the shader addresses samples with 32-bit indices)
            && self.samples() <= u64::from(u32::MAX)
            && self.limiter.is_none_or(|(lo, hi)| lo.is_finite() && hi.is_finite())
    }

    /// `Params` in `export_post.wgsl` (28 words).
    fn params(&self) -> Option<Vec<u8>> {
        let (cw, ch) = self.content_size();
        let (rw, rh) = self.rendered;
        let luma = u64::from(self.width).checked_mul(u64::from(self.height))?;
        let chroma = u64::from(self.cwidth()).checked_mul(u64::from(self.height))?;
        let resample = (cw, ch) != (rw, rh);
        let flags = u32::from(resample)
            | u32::from(self.floats) << 1
            | u32::from(self.alpha) << 2
            | u32::from(!self.chroma_422) << 3
            | u32::from(self.limiter.is_some()) << 4;
        // `pipeline::weights`: source samples per output sample and the filter's half width
        let axis = |n_src: u32, n_dst: u32| {
            let scale = n_src as f32 / n_dst.max(1) as f32;
            (scale, scale.max(1.0))
        };
        let ((sx, px), (sy, py)) = (axis(rw, cw), axis(rh, ch));
        let (lo, hi) = self.limiter.map_or((0.0, 1.0), |(lo, hi)| (lo.clamp(0.0, 100.0) / 100.0, hi.clamp(0.0, 100.0) / 100.0));
        // the 8-bit 4:2:2 conversion (`rgba_to_yuv422_10`) writes BT.709's constants out; the
        // others derive them from (Kr, Kb) (`Matrix`)
        let (kr, kg, kb, sb, sr) = if !self.floats && self.chroma_422 {
            (0.2126f32, 0.7152f32, 0.0722f32, 1.8556f32, 1.5748f32)
        } else {
            (self.kr, 1.0 - self.kr - self.kb, self.kb, 2.0 * (1.0 - self.kb), 2.0 * (1.0 - self.kr))
        };
        let words = [
            self.width,
            self.height,
            self.cwidth(),
            flags,
            rw,
            rh,
            cw,
            ch,
            i32::try_from(self.offset.0).ok()? as u32,
            i32::try_from(self.offset.1).ok()? as u32,
            u32::try_from(luma).ok()?,
            u32::try_from(luma.checked_add(chroma)?).ok()?,
            u32::try_from(luma.checked_add(chroma.checked_mul(2)?)?).ok()?,
            0,
            0,
            0,
            sx.to_bits(),
            sy.to_bits(),
            px.to_bits(),
            py.to_bits(),
            lo.to_bits(),
            hi.to_bits(),
            kr.to_bits(),
            kg.to_bits(),
            kb.to_bits(),
            sb.to_bits(),
            sr.to_bits(),
            0,
        ];
        Some(le_bytes(&words))
    }
}

/// A frame's Y'CbCr planes left on the GPU by a paired export renderer, for the encoder on the same
/// device: 16-bit samples, two to a little-endian word, Y (`width` × `height`), Cb and Cr
/// (`cwidth` × `height` each) and alpha when present, as the encoders upload a CPU frame.
pub struct ResidentPlanes {
    pub(crate) device: wgpu::Device,
    pub(crate) buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    /// 4:2:2 (`true`) or 4:4:4.
    pub chroma_422: bool,
    pub alpha: bool,
    pub bit_depth: u8,
}

impl ResidentPlanes {
    /// Whether these planes are a frame of `width` × `height` with this chroma, alpha and depth,
    /// held on `device`, in a buffer of at least `bytes`.
    pub(crate) fn fits(&self, device: &wgpu::Device, (width, height): (u32, u32), chroma_422: bool, alpha: bool, bytes: u64) -> bool {
        self.device == *device
            && (self.width, self.height) == (width, height)
            && self.chroma_422 == chroma_422
            && self.alpha == alpha
            && self.bit_depth == 10
            && self.buffer.size() >= bytes
    }
}

/// The post-render pass and what it keeps between frames.
pub(crate) struct PostPass {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut: wgpu::Buffer,
}

impl PostPass {
    /// The pipeline and the 8-bit sRGB table; to be created inside an error scope.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> PostPass {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-export-post"),
            source: wgpu::ShaderSource::Wgsl(include_str!("export_post.wgsl").into()),
        });
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let texture_entry = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("export-post"),
            entries: &[uniform_entry, texture_entry, storage(2, true), storage(3, false)],
        });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("export-post"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("export-post"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("post"),
            compilation_options: Default::default(),
            cache: None,
        });
        let table = filmcraft_color::linear_to_srgb_u8_table();
        let words: Vec<u32> = table.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
        let lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("export-post-lut"),
            size: words.len() as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&lut, 0, &le_bytes(&words));
        PostPass { pipeline, bgl, lut }
    }

    /// Record and submit the pass from `src` (the accumulator, `cfg.rendered` in size) into new
    /// planes; `None` when `cfg` is not one this pass runs. Errors are the caller's error scopes'.
    pub(crate) fn run(&self, device: &wgpu::Device, queue: &wgpu::Queue, src: &wgpu::TextureView, cfg: &PostConfig) -> Option<ResidentPlanes> {
        if !cfg.supported(&device.limits()) {
            return None;
        }
        let params = cfg.params()?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("export-resident-planes"),
            size: cfg.plane_bytes(),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let ubuf = uniform(device, queue, &params);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("export-post"),
            layout: &self.bgl,
            entries: &[
                bind(&ubuf, 0, None),
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                bind(&self.lut, 2, None),
                bind(&buffer, 3, None),
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("export-post") });
        enc.clear_buffer(&buffer, 0, None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("export-post"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(cfg.width.div_ceil(2).div_ceil(8), cfg.height.div_ceil(8), 1);
        }
        queue.submit([enc.finish()]);
        Some(ResidentPlanes {
            device: device.clone(),
            buffer,
            width: cfg.width,
            height: cfg.height,
            chroma_422: cfg.chroma_422,
            alpha: cfg.alpha,
            bit_depth: cfg.bit_depth,
        })
    }
}
