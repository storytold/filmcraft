//! GPU-only repack for imported LINEAR VA NV12 buffers.
//!
//! `record` reads source bytes at VA plane offsets/pitches, writes FilmCraft's checked 256-byte
//! row layout, then copies the planes into `R8Unorm` and `Rg8Unorm` textures. It never maps pixels
//! to CPU. Pass `ResidentNv12Buffer::buffer()` returned by platform's `import_va_nv12_surface`,
//! with fields from its `layout()`. Keep the imported buffer alive through the caller's submission.
//! The caller supplies a Task6-backed allocation budget; each repack takes one non-reusable hold
//! before allocation. Keep `RepackedNv12` alive through its last GPU submission.
//!
//! `ExportRenderer::render_resident_nv12_buffer_coded` binds these Y/RG views directly to plan
//! layers. Keep the source `VideoFrame` only for dimensions and colour metadata; pixel data is not read.

use std::num::NonZeroU64;

use wgpu::util::DeviceExt;

use crate::yuv_output::Nv12Layout;

const MAX_REPACK_BYTES: u64 = 256 * 1024 * 1024;
const PARAM_BUFFER_BYTES: u64 = 56;
const MAX_CODED_SIDE: u32 = 32_768;
const WORKGROUP_SIZE: u32 = 64;

/// Validated object and plane layout from the platform's imported linear NV12 lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nv12SourceLayout {
    width: u32,
    height: u32,
    object_size: u64,
    y_offset: u32,
    y_pitch: u32,
    uv_offset: u32,
    uv_pitch: u32,
}

impl Nv12SourceLayout {
    pub fn new(width: u32, height: u32, object_size: u64, y_offset: u32, y_pitch: u32, uv_offset: u32, uv_pitch: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || width > MAX_CODED_SIDE || height > MAX_CODED_SIDE {
            return Err("resident NV12 dimensions must be nonzero and even".into());
        }
        if object_size == 0 || object_size > MAX_REPACK_BYTES || !object_size.is_multiple_of(4) {
            return Err("resident NV12 object size exceeds repack bounds or is not word aligned".into());
        }
        if y_pitch < width || uv_pitch < width {
            return Err("resident NV12 plane pitch is smaller than coded row".into());
        }
        let y_end = plane_end(y_offset, y_pitch, height, width)?;
        let uv_end = plane_end(uv_offset, uv_pitch, height / 2, width)?;
        if y_end > object_size || uv_end > object_size {
            return Err("resident NV12 plane range exceeds source object".into());
        }
        if planes_overlap(y_offset, y_pitch, height, uv_offset, uv_pitch, height / 2, width) {
            return Err("resident NV12 plane rows overlap".into());
        }
        Ok(Self { width, height, object_size, y_offset, y_pitch, uv_offset, uv_pitch })
    }

    pub fn coded_size(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

fn plane_end(offset: u32, pitch: u32, rows: u32, row_bytes: u32) -> Result<u64, String> {
    u64::from(offset)
        .checked_add(u64::from(pitch).checked_mul(u64::from(rows.saturating_sub(1))).ok_or("NV12 plane range overflow")?)
        .and_then(|end| end.checked_add(u64::from(row_bytes)))
        .ok_or_else(|| "NV12 plane range overflow".into())
}

fn planes_overlap(y_offset: u32, y_pitch: u32, y_rows: u32, uv_offset: u32, uv_pitch: u32, uv_rows: u32, row_bytes: u32) -> bool {
    let (mut y, mut uv) = (0u32, 0u32);
    while y < y_rows && uv < uv_rows {
        let y_start = u64::from(y_offset) + u64::from(y_pitch) * u64::from(y);
        let uv_start = u64::from(uv_offset) + u64::from(uv_pitch) * u64::from(uv);
        let y_end = y_start + u64::from(row_bytes);
        let uv_end = uv_start + u64::from(row_bytes);
        if y_start < uv_end && uv_start < y_end {
            return true;
        }
        if y_end <= uv_start {
            y += 1;
        } else {
            uv += 1;
        }
    }
    false
}

/// Owned repacked storage and plane textures. Keep these alive through their last GPU consumer.
pub struct RepackedNv12<H> {
    pub layout: Nv12Layout,
    pub storage: wgpu::Buffer,
    pub y: wgpu::Texture,
    pub uv: wgpu::Texture,
    _allocation: H,
}

impl<H> RepackedNv12<H> {
    pub fn y_view(&self) -> wgpu::TextureView {
        self.y.create_view(&Default::default())
    }

    pub fn uv_view(&self) -> wgpu::TextureView {
        self.uv.create_view(&Default::default())
    }
}

/// Narrow bridge to the export frame's one-use allocation budget.
pub trait Nv12AllocationBudget {
    type Hold: Send;
    fn take(&mut self, bytes: u64) -> Result<Self::Hold, String>;
}

/// Records row repack and buffer-to-texture copies on the caller's encoder.
pub struct Nv12Repack {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl Nv12Repack {
    pub fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("filmcraft-nv12-repack"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage_entry(1, true),
                storage_entry(2, false),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-nv12-repack"),
            source: wgpu::ShaderSource::Wgsl(include_str!("nv12_repack.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("filmcraft-nv12-repack"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("filmcraft-nv12-repack"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, bind_group_layout }
    }

    /// Compute checked visible/coded output layout before taking a frame allocation reservation.
    pub fn output_layout(source: Nv12SourceLayout, source_origin: (u32, u32), visible_size: (u32, u32)) -> Result<Nv12Layout, String> {
        if visible_size.0 == 0 || visible_size.1 == 0 || !visible_size.0.is_multiple_of(2) || !visible_size.1.is_multiple_of(2) {
            return Err("resident NV12 crop size must be nonzero and even".into());
        }
        if !source_origin.0.is_multiple_of(2) || !source_origin.1.is_multiple_of(2) {
            return Err("resident NV12 crop origin must be chroma aligned".into());
        }
        let right = source_origin.0.checked_add(visible_size.0).ok_or("resident NV12 crop overflow")?;
        let bottom = source_origin.1.checked_add(visible_size.1).ok_or("resident NV12 crop overflow")?;
        if right > source.width || bottom > source.height {
            return Err("resident NV12 crop exceeds coded source".into());
        }
        Nv12Layout::new(visible_size.0, visible_size.1).ok_or_else(|| "NV12 output layout is invalid".into())
    }

    /// Total new storage + texture + parameter bytes required for one repack output.
    pub fn allocation_bytes(layout: &Nv12Layout) -> Result<u64, String> {
        let y_texture_bytes = u64::from(layout.coded_width).checked_mul(u64::from(layout.coded_height)).ok_or("NV12 texture size overflow")?;
        let uv_texture_bytes = u64::from(layout.coded_width / 2)
            .checked_mul(u64::from(layout.coded_height / 2))
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or("NV12 texture size overflow")?;
        let bytes = layout
            .size
            .checked_add(y_texture_bytes)
            .and_then(|bytes| bytes.checked_add(uv_texture_bytes))
            .and_then(|bytes| bytes.checked_add(PARAM_BUFFER_BYTES))
            .ok_or("NV12 allocation size overflow")?;
        if bytes > MAX_REPACK_BYTES {
            return Err("NV12 repack allocation exceeds hard cap".into());
        }
        Ok(bytes)
    }

    /// Repack one imported object into padded NV12 and record copies to R8/RG8 textures.
    /// `visible_size` is the output crop; source dimensions are coded surface dimensions. Takes
    /// one allocation hold before creating uniform, storage, or texture buffers.
    pub fn record<B: Nv12AllocationBudget>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Buffer,
        source_layout: Nv12SourceLayout,
        source_origin: (u32, u32),
        visible_size: (u32, u32),
        budget: &mut B,
    ) -> Result<RepackedNv12<B::Hold>, String> {
        if !source.usage().contains(wgpu::BufferUsages::STORAGE) || source.size() < source_layout.object_size {
            return Err("imported NV12 source lacks storage usage or does not cover object".into());
        }
        let layout = Self::output_layout(source_layout, source_origin, visible_size)?;
        let new_bytes = Self::allocation_bytes(&layout)?;
        let limits = device.limits();
        let output_bytes = layout.size;
        if output_bytes > MAX_REPACK_BYTES
            || source_layout.object_size > limits.max_storage_buffer_binding_size
            || output_bytes > limits.max_storage_buffer_binding_size
            || output_bytes > limits.max_buffer_size
            || source_layout.width > limits.max_texture_dimension_2d
            || source_layout.height > limits.max_texture_dimension_2d
        {
            return Err("NV12 repack exceeds device storage or memory budget".into());
        }
        let words = u32::try_from(output_bytes / 4).map_err(|_| "NV12 repack word count exceeds shader range")?;
        let groups = words.div_ceil(WORKGROUP_SIZE);
        if groups > limits.max_compute_workgroups_per_dimension {
            return Err("NV12 repack exceeds device workgroup limit".into());
        }
        let uv_offset = u32::try_from(layout.uv_offset).map_err(|_| "NV12 UV output offset exceeds shader range")?;
        let output_size = u32::try_from(output_bytes).map_err(|_| "NV12 output size exceeds shader range")?;
        let params = [
            visible_size.0,
            visible_size.1,
            source_layout.y_offset,
            source_layout.y_pitch,
            source_layout.uv_offset,
            source_layout.uv_pitch,
            layout.y_stride,
            layout.uv_stride,
            uv_offset,
            output_size,
            source_origin.0,
            source_origin.1,
            0,
            0,
        ];
        let params_bytes: [u8; 56] = std::array::from_fn(|i| (params[i / 4] >> ((i % 4) * 8)) as u8);
        let allocation = budget.take(new_bytes)?;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("filmcraft-nv12-repack-params"),
            contents: &params_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("filmcraft-nv12-repacked-buffer"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        // Validation scope turns cross-device/import mismatches into a returned error before
        // recording any compute or texture-copy commands.
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let binding = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("filmcraft-nv12-repack"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: source,
                            offset: 0,
                            size: NonZeroU64::new(source_layout.object_size),
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: storage.as_entire_binding() },
                ],
            })
        }));
        let validation = pollster::block_on(validation_scope.pop());
        if let Some(error) = validation {
            return Err(format!("NV12 source/device binding rejected: {error}"));
        }
        let bind_group = binding.map_err(|_| "NV12 source/device binding panicked".to_string())?;

        let y = device.create_texture(&plane_texture_desc("filmcraft-nv12-y", layout.coded_width, layout.coded_height, wgpu::TextureFormat::R8Unorm));
        let uv =
            device.create_texture(&plane_texture_desc("filmcraft-nv12-uv", layout.coded_width / 2, layout.coded_height / 2, wgpu::TextureFormat::Rg8Unorm));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("filmcraft-nv12-repack"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &storage,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(layout.y_stride), rows_per_image: Some(layout.coded_height) },
            },
            wgpu::TexelCopyTextureInfo { texture: &y, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: layout.coded_width, height: layout.coded_height, depth_or_array_layers: 1 },
        );
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &storage,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: layout.uv_offset,
                    bytes_per_row: Some(layout.uv_stride),
                    rows_per_image: Some(layout.coded_height / 2),
                },
            },
            wgpu::TexelCopyTextureInfo { texture: &uv, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: layout.coded_width / 2, height: layout.coded_height / 2, depth_or_array_layers: 1 },
        );
        Ok(RepackedNv12 { layout, storage, y, uv, _allocation: allocation })
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn plane_texture_desc(label: &'static str, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::TextureDescriptor<'static> {
    wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::Nv12SourceLayout;

    #[test]
    fn source_layout_checks_plane_ranges_and_word_binding_bounds() {
        assert!(Nv12SourceLayout::new(640, 368, 423_936, 0, 640, 282_624, 640).is_ok());
        let padded = Nv12SourceLayout::new(640, 368, 400_000, 3, 672, 250_003, 704).expect("valid arbitrary plane offsets");
        assert_eq!(padded.coded_size(), (640, 368));
        assert!(Nv12SourceLayout::new(4, 4, 32, 0, 8, 4, 8).is_ok(), "row spans can be disjoint when plane envelopes overlap");
        assert!(Nv12SourceLayout::new(640, 368, 423_936, 0, 640, 100, 640).is_err());
        assert!(Nv12SourceLayout::new(640, 368, 423_935, 0, 640, 282_624, 640).is_err());
        assert!(Nv12SourceLayout::new(641, 368, 423_936, 0, 640, 282_624, 640).is_err());
        assert!(Nv12SourceLayout::new(640, 368, 423_937, 0, 640, 282_624, 640).is_err());
    }

    #[test]
    fn crop_layout_requires_420_alignment_and_bounds() {
        let source = Nv12SourceLayout::new(640, 368, 423_936, 0, 640, 282_624, 640).expect("source layout");
        assert!(super::Nv12Repack::output_layout(source, (2, 4), (320, 180)).is_ok());
        assert!(super::Nv12Repack::output_layout(source, (1, 0), (320, 180)).is_err());
        assert!(super::Nv12Repack::output_layout(source, (400, 0), (320, 180)).is_err());
    }
}
