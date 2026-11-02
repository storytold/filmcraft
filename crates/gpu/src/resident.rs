use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use filmcraft_render::plan::{EligibilityFailure, GpuPlanError, ResidentRequiredFramePlan, ResidentSourceId, ResidentSourceKind};

/// Borrowed repacked NV12 source planes for one receipt-local decoded source.
/// The compositor creates and binds views only after checking texture formats and extents.
pub struct ResidentNv12Binding<'a> {
    pub id: ResidentSourceId,
    pub layout: crate::yuv_output::Nv12Layout,
    pub y: &'a wgpu::Texture,
    pub uv: &'a wgpu::Texture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResidentBindingError {
    Missing,
    Duplicate,
    Unexpected,
}

pub(crate) fn validate_exact_source_ids<T>(required: impl IntoIterator<Item = T>, bound: impl IntoIterator<Item = T>) -> Result<(), ResidentBindingError>
where
    T: Copy + Eq + Hash,
{
    let required: HashSet<_> = required.into_iter().collect();
    let mut bound_ids = HashSet::with_capacity(required.len());
    for id in bound {
        if !required.contains(&id) {
            return Err(ResidentBindingError::Unexpected);
        }
        if !bound_ids.insert(id) {
            return Err(ResidentBindingError::Duplicate);
        }
    }
    if bound_ids.len() != required.len() {
        return Err(ResidentBindingError::Missing);
    }
    Ok(())
}

fn validate_decoded_source_ids<T: Copy + Eq + Hash>(
    required: impl IntoIterator<Item = (T, ResidentSourceKind)>,
    bound: impl IntoIterator<Item = T>,
) -> Result<(), ResidentBindingError> {
    validate_exact_source_ids(required.into_iter().filter_map(|(id, kind)| (kind == ResidentSourceKind::Decoded).then_some(id)), bound)
}

pub(crate) fn validate_plan_bindings(plan: &ResidentRequiredFramePlan, bindings: &[ResidentNv12Binding<'_>]) -> Result<(), GpuPlanError> {
    let invalid = |item_id, parameter: &str| GpuPlanError {
        item_id,
        id: Some("resident NV12 source bindings".into()),
        reason: EligibilityFailure::InvalidParameter { id: "resident NV12 source bindings".into(), parameter: parameter.into() },
    };
    let layers: Vec<_> = plan.layers().collect();
    validate_decoded_source_ids(layers.iter().map(|layer| (layer.source_id(), layer.source_kind())), bindings.iter().map(|binding| binding.id)).map_err(
        |error| match error {
            ResidentBindingError::Missing => invalid(None, "missing receipt source binding"),
            ResidentBindingError::Duplicate => invalid(None, "duplicate receipt source binding"),
            ResidentBindingError::Unexpected => invalid(None, "unrequested receipt source binding"),
        },
    )?;
    let by_id: HashMap<_, _> = bindings.iter().map(|binding| (binding.id, binding)).collect();
    for layer in &layers {
        if layer.source_kind() != ResidentSourceKind::Decoded {
            continue;
        }
        let Some(binding) = by_id.get(&layer.source_id()) else {
            return Err(invalid(Some(layer.evidence().item_id()), "missing receipt source binding"));
        };
        if !layout_matches(binding.layout, layer.size())
            || !source_textures_match(
                binding.layout,
                binding.y.format(),
                binding.y.size(),
                binding.y.usage(),
                binding.uv.format(),
                binding.uv.size(),
                binding.uv.usage(),
            )
        {
            return Err(invalid(Some(layer.evidence().item_id()), "checked NV12 layout must match visible source dimensions"));
        }
    }
    Ok(())
}

fn layout_matches(layout: crate::yuv_output::Nv12Layout, size: (u32, u32)) -> bool {
    crate::yuv_output::Nv12Layout::with_coded_size(layout.width, layout.height, layout.coded_width, layout.coded_height) == Some(layout)
        && layout.width.is_multiple_of(2)
        && layout.height.is_multiple_of(2)
        && layout.coded_width.is_multiple_of(2)
        && layout.coded_height.is_multiple_of(2)
        && (layout.width, layout.height) == size
}

fn source_textures_match(
    layout: crate::yuv_output::Nv12Layout,
    y_format: wgpu::TextureFormat,
    y_size: wgpu::Extent3d,
    y_usage: wgpu::TextureUsages,
    uv_format: wgpu::TextureFormat,
    uv_size: wgpu::Extent3d,
    uv_usage: wgpu::TextureUsages,
) -> bool {
    y_format == wgpu::TextureFormat::R8Unorm
        && y_usage.contains(wgpu::TextureUsages::TEXTURE_BINDING)
        && (y_size.width, y_size.height, y_size.depth_or_array_layers) == (layout.coded_width, layout.coded_height, 1)
        && uv_format == wgpu::TextureFormat::Rg8Unorm
        && uv_usage.contains(wgpu::TextureUsages::TEXTURE_BINDING)
        && (uv_size.width, uv_size.height, uv_size.depth_or_array_layers) == (layout.coded_width / 2, layout.coded_height / 2, 1)
}

#[cfg(test)]
mod tests {
    use super::{ResidentBindingError, ResidentSourceKind, validate_exact_source_ids};

    #[test]
    fn resident_bindings_cover_each_unique_source_once() {
        assert_eq!(validate_exact_source_ids([3, 3, 8], [3, 8]), Ok(()));
    }

    #[test]
    fn resident_bindings_reject_duplicate_ids() {
        assert_eq!(validate_exact_source_ids([3, 8], [3, 3, 8]), Err(ResidentBindingError::Duplicate));
    }

    #[test]
    fn resident_bindings_reject_missing_ids() {
        assert_eq!(validate_exact_source_ids([3, 8], [3]), Err(ResidentBindingError::Missing));
    }

    #[test]
    fn resident_bindings_reject_unrequested_ids() {
        assert_eq!(validate_exact_source_ids([3], [3, 8]), Err(ResidentBindingError::Unexpected));
    }

    #[test]
    fn resident_mixed_sources_bind_only_decoded_ids() {
        let demo = ResidentSourceKind::Demo(filmcraft_media::DemoScene::Aurora);
        assert_eq!(super::validate_decoded_source_ids([(3, ResidentSourceKind::Decoded), (8, demo)], [3]), Ok(()));
        assert_eq!(super::validate_decoded_source_ids([(3, ResidentSourceKind::Decoded), (8, demo)], [3, 8]), Err(ResidentBindingError::Unexpected));
    }

    #[test]
    fn resident_layout_must_be_checked_and_match_source_extent() {
        let layout = crate::yuv_output::Nv12Layout::new(640, 360).unwrap();
        assert!(super::layout_matches(layout, (640, 360)));
        assert!(!super::layout_matches(layout, (320, 180)));
        let malformed = crate::yuv_output::Nv12Layout { y_stride: layout.y_stride + 1, ..layout };
        assert!(!super::layout_matches(malformed, (640, 360)));
    }

    #[test]
    fn resident_source_planes_require_nv12_formats_and_extents() {
        let layout = crate::yuv_output::Nv12Layout::new(640, 360).unwrap();
        let y = wgpu::Extent3d { width: 640, height: 360, depth_or_array_layers: 1 };
        let uv = wgpu::Extent3d { width: 320, height: 180, depth_or_array_layers: 1 };
        let usage = wgpu::TextureUsages::TEXTURE_BINDING;
        assert!(super::source_textures_match(layout, wgpu::TextureFormat::R8Unorm, y, usage, wgpu::TextureFormat::Rg8Unorm, uv, usage));
        assert!(!super::source_textures_match(layout, wgpu::TextureFormat::R8Unorm, y, usage, wgpu::TextureFormat::R8Unorm, uv, usage));
        assert!(!super::source_textures_match(layout, wgpu::TextureFormat::R8Unorm, y, usage, wgpu::TextureFormat::Rg8Unorm, y, usage));
        assert!(!super::source_textures_match(
            layout,
            wgpu::TextureFormat::R8Unorm,
            y,
            usage,
            wgpu::TextureFormat::Rg8Unorm,
            uv,
            wgpu::TextureUsages::COPY_DST
        ));
    }

    #[test]
    fn resident_nv12_uses_limited_range_code_values() {
        let source = super::super::SrcInfo {
            kind: 3,
            code_scale: 255.0,
            code_levels: 256.0,
            alpha: 0.0,
            chroma: (320, 180),
            size: (640, 360),
            color: filmcraft_color::ColorInfo { range: filmcraft_color::Range::Limited, ..Default::default() },
            input: None,
        };
        let uniforms = super::super::GpuCompositor::uniforms(&source, &filmcraft_geom::Affine::IDENTITY, 1.0, filmcraft_render::Blend::Normal, (640, 360));
        assert_eq!(uniforms[13], 3.0);
        assert_eq!(&uniforms[16..20], &[16.0, 219.0, 128.0, 224.0]);
        assert_eq!(uniforms[22], 255.0);
    }
}
