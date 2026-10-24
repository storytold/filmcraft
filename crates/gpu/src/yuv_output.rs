//! GPU-resident BT.709 limited-range NV12 conversion from a linear RGBA accumulator.
//!
//! Output stays in a storage buffer. Callers record the pass into their command encoder and keep
//! the buffer on the GPU; this module does not map or copy pixels to the CPU.

use wgpu::util::DeviceExt;

/// Packed NV12 output layout. Row strides meet wgpu's 256-byte buffer-to-texture copy alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nv12Layout {
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub uv_offset: u64,
    pub size: u64,
}

impl Nv12Layout {
    /// Return a checked layout for an 8-bit 4:2:0 image.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Self::with_coded_size(width, height, width, height)
    }

    /// Return a checked NV12 layout for a visible picture inside a coded surface.
    pub fn with_coded_size(width: u32, height: u32, coded_width: u32, coded_height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        if coded_width < width || coded_height < height {
            return None;
        }
        let y_stride = coded_width.checked_add(255)? & !255;
        let uv_width = coded_width.div_ceil(2).checked_mul(2)?;
        let uv_stride = uv_width.checked_add(255)? & !255;
        let uv_offset = u64::from(y_stride).checked_mul(u64::from(coded_height))?;
        let uv_bytes = u64::from(uv_stride).checked_mul(u64::from(coded_height.div_ceil(2)))?;
        let size = uv_offset.checked_add(uv_bytes)?;
        Some(Self { width, height, coded_width, coded_height, y_stride, uv_stride, uv_offset, size })
    }
}

/// Records linear RGBA to BT.709 limited NV12 conversion into a caller-owned GPU storage buffer.
pub struct Nv12Output {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut: wgpu::Buffer,
}

impl Nv12Output {
    pub fn new(device: &wgpu::Device) -> Self {
        use wgpu::util::DeviceExt;

        let lut_bytes: Vec<u8> = filmcraft_color::linear_to_srgb_u8_table().iter().flat_map(|v| u32::from(*v).to_le_bytes()).collect();
        let lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("filmcraft-yuv-srgb-lut"),
            contents: &lut_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("filmcraft-yuv-output"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage_entry(2, true),
                storage_entry(3, false),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-yuv-output"),
            source: wgpu::ShaderSource::Wgsl(include_str!("yuv_output.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("filmcraft-yuv-output"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("filmcraft-yuv-output"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, bgl, lut }
    }

    /// Encode conversion for a full-size `Rgba16Float` view. `output` must fit the NV12 layout.
    pub fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> Option<Nv12Layout> {
        self.record_coded(device, encoder, input, output, width, height, width, height)
    }

    /// Encode conversion and edge replication for a visible picture in a coded-size NV12 buffer.
    pub fn record_coded(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::Buffer,
        width: u32,
        height: u32,
        coded_width: u32,
        coded_height: u32,
    ) -> Option<Nv12Layout> {
        let layout = Nv12Layout::with_coded_size(width, height, coded_width, coded_height)?;
        let groups_x = coded_width.div_ceil(8);
        let groups_y = coded_height.div_ceil(2);
        let limits = device.limits();
        if layout.size > output.size()
            || layout.size > limits.max_storage_buffer_binding_size
            || groups_x.div_ceil(8) > limits.max_compute_workgroups_per_dimension
            || groups_y.div_ceil(8) > limits.max_compute_workgroups_per_dimension
        {
            return None;
        }
        let params = [width, height, coded_width, coded_height, layout.y_stride, layout.uv_stride, u32::try_from(layout.uv_offset).ok()?, groups_x];
        let param_bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("filmcraft-yuv-output-params"),
            contents: &param_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("filmcraft-yuv-output"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(input) },
                wgpu::BindGroupEntry { binding: 2, resource: self.lut.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: output, offset: 0, size: std::num::NonZeroU64::new(layout.size) }),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("filmcraft-yuv-output"), timestamp_writes: None });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(groups_x.div_ceil(8), groups_y.div_ceil(8), 1);
        Some(layout)
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
