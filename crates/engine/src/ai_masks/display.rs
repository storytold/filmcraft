//! Display-only views of the inferred alpha. Paint marks the region to analyze, not opacity.
use std::sync::Arc;

use filmcraft_project::{
    ClipId, ItemId, Mask, MaskPath, ParamValue, Project,
    raster_mask::{RasterFrame, RasterMask},
};

use super::{Draft, Pick};

pub(super) struct Views {
    pub region: Project,
    pub alpha: Project,
    pub composite: Project,
}

/// Build display-only edge-region, alpha and composite projects from validated inferred alpha.
pub(super) fn refine_views(project: &Project, seq: ItemId, clip: ClipId, mask: &Arc<RasterMask>, draft: &Draft) -> Result<Views, String> {
    mask.validate()?;
    let width = mask.width as usize;
    let height = mask.height as usize;
    let item = project.sequence(seq).and_then(|sequence| sequence.find_item(clip)).map(|(_, item)| item).ok_or("Preview clip is gone")?;
    let (source_width, source_height) = filmcraft_render::source_size(project, item.item).ok_or("Preview source size is unavailable")?;
    if source_width == 0 || source_height == 0 {
        return Err("Invalid preview source size".into());
    }
    let mut regions = Vec::new();
    let mut details = Vec::new();
    for (index, frame) in mask.frames.iter().enumerate() {
        let alpha = mask.alpha_at(frame.time).ok_or("Missing preview alpha")?;
        let foreground: Vec<_> = alpha.iter().map(|value| *value > 0.5).collect();
        let trimap = filmcraft_rotoscope::pixels::trimap(&foreground, width, height, draft.band as usize)?;
        let mut region: Vec<_> = trimap.iter().map(|value| *value == 128).collect();
        let end = mask.frames.get(index + 1).map(|frame| frame.time).unwrap_or(mask.end);
        for point in draft.points.iter().filter(|point| point.kind == Pick::Refine && point.time >= frame.time && point.time < end) {
            if !point.x.is_finite()
                || !point.y.is_finite()
                || point.x < 0.0
                || point.y < 0.0
                || point.x >= source_width as f64
                || point.y >= source_height as f64
            {
                return Err("Invalid refine point".into());
            }
            let x = point.x * width as f64 / source_width as f64;
            let y = point.y * height as f64 / source_height as f64;
            let radius = draft.band as f64 * 2.0;
            let x0 = (x - radius).floor().max(0.0) as usize;
            let y0 = (y - radius).floor().max(0.0) as usize;
            let x1 = ((x + radius).ceil() as usize).saturating_add(1).min(width);
            let y1 = ((y + radius).ceil() as usize).saturating_add(1).min(height);
            for row in y0..y1 {
                for column in x0..x1 {
                    let dx = column as f64 - x;
                    let dy = row as f64 - y;
                    if dx * dx + dy * dy <= radius * radius {
                        region[row * width + column] = true;
                    }
                }
            }
        }
        let opaque: Vec<_> = region.iter().map(|region| if *region { u16::MAX } else { 0 }).collect();
        let detail: Vec<_> = alpha.iter().zip(&region).map(|(value, region)| if *region { (*value * 65535.0).round_ties_even() as u16 } else { 0 }).collect();
        regions.push(RasterFrame { time: frame.time, runs: Arc::new(runs(&opaque)) });
        details.push(RasterFrame { time: frame.time, runs: Arc::new(runs(&detail)) });
    }
    let raster = |frames| Arc::new(RasterMask { width: mask.width, height: mask.height, end: mask.end, quality: mask.quality, frames });
    let mut region = project.clone();
    tint(&mut region, seq, clip, Some(raster(regions)), [0.0, 0.0, 0.0, 1.0], "Refine region")?;
    tint(&mut region, seq, clip, Some(raster(details)), [1.0; 4], "Refined alpha details")?;
    let mut alpha = project.clone();
    tint(&mut alpha, seq, clip, None, [0.0, 0.0, 0.0, 1.0], "Alpha background")?;
    tint(&mut alpha, seq, clip, Some(mask.clone()), [1.0; 4], "Inferred alpha")?;
    let mut composite = project.clone();
    let (_, item) = composite.sequence_mut(seq).and_then(|sequence| sequence.find_item_mut(clip)).ok_or("Preview clip is gone")?;
    let opacity = item.effect_mut("opacity").ok_or("Preview opacity effect is unavailable")?;
    let mut matte = Mask::new("Refined alpha preview", MaskPath::default());
    matte.raster = Some(mask.clone());
    opacity.masks.push(matte);
    Ok(Views { region, alpha, composite })
}

/// Append a tint effect, optionally limited by raster alpha, to a preview project copy.
fn tint(project: &mut Project, seq: ItemId, clip: ClipId, raster: Option<Arc<RasterMask>>, color: [f32; 4], name: &str) -> Result<(), String> {
    let (_, item) = project.sequence_mut(seq).and_then(|sequence| sequence.find_item_mut(clip)).ok_or("Preview clip is gone")?;
    let mut tint = filmcraft_project::find_effect("tint").ok_or("Tint effect unavailable")?.instance();
    for key in ["black", "white"] {
        if let Some(param) = tint.param_mut(key) {
            param.value = ParamValue::Color(color);
        }
    }
    if let Some(param) = tint.param_mut("amount") {
        param.value = ParamValue::Float(100.0);
    }
    if let Some(raster) = raster {
        let mut mask = Mask::new(name, MaskPath::default());
        mask.raster = Some(raster);
        tint.masks.push(mask);
    }
    item.effects.push(tint);
    Ok(())
}

/// Encode display alpha as consecutive equal-value runs.
fn runs(alpha: &[u16]) -> Vec<(u32, u16)> {
    let mut runs = Vec::new();
    let mut start = 0;
    while let Some(value) = alpha.get(start) {
        let count = alpha.iter().skip(start).take_while(|next| *next == value).count();
        runs.push((count as u32, *value));
        start += count;
    }
    runs
}
