//! Source reconstruction into reusable linear premultiplied working images; no compute requirement.

use std::collections::{HashMap, HashSet};

pub(crate) const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

pub(crate) struct SourceStage {
    pipeline: wgpu::RenderPipeline,
    pool: HashMap<(u32, u32), wgpu::Texture>,
    used: HashSet<(u32, u32)>,
    storage: bool,
}

pub(crate) struct SourceJob {
    bg: wgpu::BindGroup,
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
}

impl SourceStage {
    pub(crate) fn supported(device: &wgpu::Device) -> bool {
        let usages = WORKING_FORMAT.guaranteed_format_features(device.features()).allowed_usages;
        usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
            && device.limits().max_color_attachment_bytes_per_sample >= std::mem::size_of::<[f32; 4]>() as u32
    }

    pub(crate) fn new(device: &wgpu::Device, shader: &wgpu::ShaderModule, bgl: &wgpu::BindGroupLayout, storage: bool) -> Self {
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("source"), bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("source"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_source"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: WORKING_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, pool: HashMap::new(), used: HashSet::new(), storage }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.used.clear();
    }

    pub(crate) fn end_frame(&mut self) {
        self.pool.retain(|size, _| self.used.contains(size));
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        self.pool.values().fold(0u64, |sum, t| sum.saturating_add(u64::from(t.width()) * u64::from(t.height()) * std::mem::size_of::<[f32; 4]>() as u64))
    }

    pub(crate) fn job(&mut self, device: &wgpu::Device, size: (u32, u32), bg: wgpu::BindGroup) -> Result<SourceJob, String> {
        if size.0 == 0 || size.1 == 0 || size.0.max(size.1) > device.limits().max_texture_dimension_2d {
            return Err("GPU working image exceeds texture limits".into());
        }
        self.used.insert(size);
        let texture = self
            .pool
            .entry(size)
            .or_insert_with(|| {
                let storage = if self.storage { wgpu::TextureUsages::STORAGE_BINDING } else { wgpu::TextureUsages::empty() };
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("filmcraft-source"),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: WORKING_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | storage,
                    view_formats: &[],
                })
            })
            .clone();
        let view = texture.create_view(&Default::default());
        Ok(SourceJob { bg, texture, view })
    }

    /// Record immediately before this layer's effects/draw; later layers reuse the same size's target.
    pub(crate) fn record(&self, enc: &mut wgpu::CommandEncoder, job: &SourceJob, timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("source"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &job.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &job.bg, &[]);
        pass.draw(0..6, 0..1);
    }

    #[cfg(test)]
    pub(crate) fn texture_count(&self) -> usize {
        self.pool.len()
    }
}

#[cfg(test)]
mod tests {
    use filmcraft_frame::{Chroma, PixelData, VideoFrame};
    use filmcraft_render::plan::{FramePlan, PlanLayer, PlanStep};
    use std::sync::Arc;

    #[test]
    fn transition_substeps_keep_working_images_until_the_whole_frame_ends() {
        let instance = wgpu::Instance::default();
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
            return;
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&Default::default())) else {
            return;
        };
        let frame = Arc::new(VideoFrame {
            width: 8,
            height: 8,
            data: PixelData::Yuv8 {
                planes: [Arc::new(vec![128; 8 * 8]), Arc::new(vec![128; 4 * 4]), Arc::new(vec![128; 4 * 4])],
                chroma: Chroma::C420,
                alpha: None,
            },
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        });
        let layer = PlanLayer::new(frame, filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::Blend::Normal);
        let plan = FramePlan::Composite {
            width: 8,
            height: 8,
            steps: vec![PlanStep::Transition {
                inputs: [vec![layer.clone()], vec![layer]],
                effect: filmcraft_project::find_effect("push").unwrap().instance(),
                progress: 0.5,
                scale: 1.0,
            }],
        };
        let mut compositor = crate::GpuCompositor::new(&device, &queue);
        compositor.composite(&plan).unwrap();
        let texture = compositor.source.as_ref().unwrap().pool.get(&(8, 8)).unwrap().clone();
        compositor.composite(&plan).unwrap();
        assert_eq!(&texture, compositor.source.as_ref().unwrap().pool.get(&(8, 8)).unwrap());
        assert_eq!(compositor.source_draws, 4);
        compositor.composite(&FramePlan::Layers { width: 8, height: 8, layers: vec![] }).unwrap();
        assert_eq!(compositor.working_bytes(), 0);
    }
}
