//! Native orchestration of exported neural graphs, bounded temporal memory and alpha caching.
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use filmcraft_project::raster_mask::{RasterFrame, RasterMask};
use rten::ValueView;
use sha2::{Digest, Sha256};

use crate::{
    Input, Kind,
    models::{Floats, Graph, Models},
    pixels,
};

struct Cache<T> {
    entries: VecDeque<([u8; 32], Arc<T>, usize)>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
}

impl<T> Cache<T> {
    /// Create an LRU cache bounded by both entry count and retained bytes.
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self { entries: VecDeque::new(), max_entries, max_bytes, bytes: 0 }
    }
    /// Return a cached value and move its entry to the most-recently-used end.
    fn get(&mut self, key: &[u8; 32]) -> Option<Arc<T>> {
        let index = self.entries.iter().position(|e| &e.0 == key)?;
        let entry = self.entries.remove(index)?;
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    /// Replace an entry and evict oldest values until both cache budgets are satisfied.
    fn put(&mut self, key: [u8; 32], value: Arc<T>, bytes: usize) {
        if bytes > self.max_bytes {
            return;
        }
        if let Some(index) = self.entries.iter().position(|e| e.0 == key)
            && let Some(old) = self.entries.remove(index)
        {
            self.bytes -= old.2;
        }
        self.entries.push_back((key, value, bytes));
        self.bytes += bytes;
        while self.bytes > self.max_bytes || self.entries.len() > self.max_entries {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.2;
            } else {
                break;
            }
        }
    }
}

struct Features {
    high0: Floats,
    high1: Floats,
    image: Floats,
}
struct Prediction {
    mask: Floats,
    pointer: Floats,
}
struct Memory {
    features: Floats,
    position: Floats,
    pointer: Floats,
    mask: Floats,
}

struct MattePositions {
    base: Floats,
    cache: Cache<Floats>,
}

struct MatteTile {
    core: (usize, usize, usize, usize),
    origin: (usize, usize),
    pixels: Floats,
    position: Arc<Floats>,
}

/// Join every worker before returning, including on cancellation, spawn failure,
/// model error or panic. The model kernels share the job's existing thread pool.
fn tile_map<T: Sync, R: Send>(items: &[T], workers: usize, operation: impl Fn(&T) -> Result<R, String> + Sync) -> Result<Vec<R>, String> {
    if !(1..=4).contains(&workers) || items.len() > 4 {
        return Err("Matte tile worker budget exceeded".into());
    }
    let guarded =
        |item: &T| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(item))).map_err(|_| "Matte tile inference failed safely".to_string())?;
    if workers == 1 || items.len() < 2 {
        return items.iter().map(guarded).collect();
    }
    let mut results = Vec::with_capacity(items.len());
    for chunk in items.chunks(workers) {
        std::thread::scope(|scope| -> Result<(), String> {
            let mut jobs = Vec::with_capacity(chunk.len());
            for item in chunk {
                let guarded = &guarded;
                jobs.push(std::thread::Builder::new().name("filmcraft-matte".into()).spawn_scoped(scope, move || guarded(item)).map_err(|e| e.to_string())?);
            }
            for job in jobs {
                results.push(job.join().map_err(|_| "Matte worker failed safely".to_string())??);
            }
            Ok(())
        })?;
    }
    Ok(results)
}

impl MattePositions {
    /// Validate the checkpoint positional tensor and initialize the bounded tile-shape cache.
    fn new(base: Floats) -> Result<Self, String> {
        Ok(Self { base: base.checked(&[1, 197, 384])?, cache: Cache::new(4, 6 * 1024 * 1024) })
    }

    /// Return or compute positional embeddings for a validated matte tile grid.
    fn get(&mut self, height: usize, width: usize) -> Result<Arc<Floats>, String> {
        if !(1..=32).contains(&height) || !(1..=32).contains(&width) {
            return Err("Invalid matte position dimensions".into());
        }
        let mut key = [0; 32];
        key[0] = height as u8;
        key[1] = width as u8;
        if let Some(position) = self.cache.get(&key) {
            return Ok(position);
        }
        // Tile shapes repeat throughout a clip. These embeddings depend only on
        // the pinned model and tile dimensions, never the image or brush.
        let position = Arc::new(Floats { shape: vec![1, height, width, 384], data: pixels::absolute_position(&self.base.data, height, width)? });
        self.cache.put(key, position.clone(), position.data.len() * 4);
        Ok(position)
    }
}

pub struct Runtime {
    models: Models,
    features: Cache<Features>,
    tracked: Cache<Vec<Vec<bool>>>,
    alpha: Cache<Vec<u16>>,
    positions: MattePositions,
}

/// Convert cooperative cancellation into an actionable error before more model work begins.
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Cancelled: nothing was changed".into()) } else { Ok(()) }
}

impl Runtime {
    /// Load pinned graphs and initialize bounded caches, converting loader panics into errors.
    pub fn load(path: &Path) -> Result<Self, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let models = Models::load(path)?;
            let [position_base]: [Floats; 1] = models.matte.run(vec![], &["position_base"])?.try_into().map_err(|_| "missing matte position embedding")?;
            Ok(Self {
                models,
                positions: MattePositions::new(position_base)?,
                features: Cache::new(2, 192 * 1024 * 1024),
                tracked: Cache::new(4, 40_000_000),
                alpha: Cache::new(120, 80_000_000),
            })
        }))
        .map_err(|_| "Native model loading failed safely".to_string())?
    }

    /// Reuse or compute encoded image features keyed by dimensions and exact RGBA bytes.
    fn features(&mut self, input: &Input<'_>, index: usize) -> Result<Arc<Features>, String> {
        let frame = input.frames.get(index).ok_or("invalid source frame")?;
        let key = image_key(input.width, input.height, frame.rgba);
        if let Some(features) = self.features.get(&key) {
            return Ok(features);
        }
        let image = Floats { shape: vec![1, 3, 1024, 1024], data: pixels::normalized_image(frame.rgba, input.width, input.height)? };
        let outputs = self.models.image.run(vec![("image", image.view()?)], &["high0", "high1", "image_features"])?;
        let [high0, high1, image]: [Floats; 3] = outputs.try_into().map_err(|_| "image encoder output count differs")?;
        let features = Arc::new(Features {
            high0: high0.checked(&[1, 32, 256, 256])?,
            high1: high1.checked(&[1, 64, 128, 128])?,
            image: image.checked(&[1, 256, 64, 64])?,
        });
        let bytes = (features.high0.data.len() + features.high1.data.len() + features.image.data.len()) * 4;
        self.features.put(key, features.clone(), bytes);
        Ok(features)
    }

    /// Run a prompt or temporal decoder and validate the required mask/pointer outputs.
    fn decode<'a>(graph: &Graph, features: &'a Features, mut extra: Vec<(&str, ValueView<'a>)>) -> Result<Prediction, String> {
        let mut inputs = vec![("high0", features.high0.view()?), ("high1", features.high1.view()?), ("image_features", features.image.view()?)];
        inputs.append(&mut extra);
        let [mask, pointer, score]: [Floats; 3] = graph.run(inputs, &["mask", "pointer", "score"])?.try_into().map_err(|_| "decoder output count differs")?;
        score.checked(&[1, 1])?;
        let prediction = Prediction { mask: mask.checked(&[1, 1, 256, 256])?, pointer: pointer.checked(&[1, 256])? };
        if prediction.mask.data.iter().any(|x| x.abs() > 1_000_000.0) {
            return Err("Model returned invalid tracking logits".into());
        }
        Ok(prediction)
    }

    /// Encode a predicted mask into the bounded temporal-memory representation.
    fn memory(&self, features: &Features, prediction: Prediction) -> Result<(Memory, Vec<f32>), String> {
        let output =
            self.models.memory.run(vec![("image_features", features.image.view()?), ("mask", prediction.mask.view()?)], &["memory", "position", "temporal"])?;
        let [mem, position, temporal]: [Floats; 3] = output.try_into().map_err(|_| "memory encoder output count differs")?;
        Ok((
            Memory { features: mem.checked(&[1, 512, 64])?, position: position.checked(&[1, 512, 64])?, pointer: prediction.pointer, mask: prediction.mask },
            temporal.checked(&[7, 64])?.data,
        ))
    }

    /// Track from prompted frames in both directions while reusing cached frame encodings.
    fn track(&mut self, input: &Input<'_>, cancel: &AtomicBool, progress: &mut impl FnMut(usize, &str)) -> Result<Vec<Vec<bool>>, String> {
        let mut groups: BTreeMap<usize, Vec<&crate::Point>> = BTreeMap::new();
        for point in input.points.iter().filter(|p| p.kind != Kind::Refine) {
            groups.entry(point.frame).or_default().push(point);
        }
        let seed = *groups.keys().next().ok_or("Add an Include point")?;
        let mut conditional = BTreeMap::new();
        let mut temporal = Vec::new();
        for (index, points) in groups {
            cancelled(cancel)?;
            let features = self.features(input, index)?;
            let coordinates: Vec<_> = points.iter().flat_map(|p| [p.x * 1024.0 / input.width as f32, p.y * 1024.0 / input.height as f32]).collect();
            let labels: Vec<i32> = points.iter().map(|p| i32::from(p.kind == Kind::Include)).collect();
            let graph = if points.len() == 1 { &self.models.prompt_one } else { &self.models.prompt_many };
            let prediction = Self::decode(
                graph,
                &features,
                vec![
                    ("points", ValueView::from_shape([1, points.len(), 2], &coordinates).map_err(|e| e.to_string())?),
                    ("labels", ValueView::from_shape([1, points.len()], &labels).map_err(|e| e.to_string())?),
                ],
            )?;
            let (memory, tpos) = self.memory(&features, prediction)?;
            conditional.insert(index, memory);
            temporal = tpos;
        }
        let mut other: BTreeMap<usize, Memory> = BTreeMap::new();
        let mut masks: Vec<Option<Vec<bool>>> = vec![None; input.frames.len()];
        for reverse in [false, true] {
            if reverse && seed == 0 {
                break;
            }
            let indices: Vec<_> = if reverse { (0..=seed).rev().collect() } else { (seed..input.frames.len()).collect() };
            for index in indices {
                cancelled(cancel)?;
                if !conditional.contains_key(&index) {
                    let features = self.features(input, index)?;
                    let (memories, positions, pointers) = memory_bank(index, reverse, input.frames.len(), &conditional, &other, &temporal)?;
                    let prediction = Self::decode(
                        &self.models.track,
                        &features,
                        vec![("memories", memories.view()?), ("positions", positions.view()?), ("pointers", pointers.view()?)],
                    )?;
                    let (memory, _) = self.memory(&features, prediction)?;
                    other.insert(index, memory);
                }
                let memory = conditional.get(&index).or_else(|| other.get(&index)).ok_or("Missing tracked frame")?;
                masks[index] = Some(pixels::bilinear(&memory.mask.data, 256, 256, input.width, input.height)?.into_iter().map(|x| x > 0.0).collect());
                progress(masks.iter().filter(|m| m.is_some()).count(), "Tracking with native EdgeTAM");
            }
        }
        masks.into_iter().map(|m| m.ok_or_else(|| "Tracking did not produce every requested frame".into())).collect()
    }

    /// Run validated inference under a panic guard so failed model work cannot crash the editor.
    pub fn infer(&mut self, input: &Input<'_>, cancel: &AtomicBool, mut progress: impl FnMut(usize, &str)) -> Result<RasterMask, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.infer_inner(input, cancel, &mut progress)))
            .map_err(|_| "Native model inference failed safely".to_string())?
    }

    /// Combine tracking, brush trimaps and optional matting into a validated source-timed raster mask.
    fn infer_inner(&mut self, input: &Input<'_>, cancel: &AtomicBool, progress: &mut impl FnMut(usize, &str)) -> Result<RasterMask, String> {
        let pixels = input.validate()?;
        cancelled(cancel)?;
        let matte_workers = self.models.sequence_mode(input.frames.len() > 1);
        let mut digest = Sha256::new();
        digest.update(input.width.to_le_bytes());
        digest.update(input.height.to_le_bytes());
        for frame in &input.frames {
            digest.update(frame.rgba);
        }
        for point in input.points.iter().filter(|p| p.kind != Kind::Refine) {
            digest.update(point.frame.to_le_bytes());
            digest.update(point.x.to_bits().to_le_bytes());
            digest.update(point.y.to_bits().to_le_bytes());
            digest.update([u8::from(point.kind == Kind::Include)]);
        }
        let key: [u8; 32] = digest.finalize().into();
        let masks = if let Some(alpha) = input.base_alpha {
            Arc::new(vec![alpha.iter().map(|value| *value > 32767).collect()])
        } else if let Some(masks) = self.tracked.get(&key) {
            masks
        } else {
            let masks = Arc::new(self.track(input, cancel, progress)?);
            cancelled(cancel)?;
            self.tracked.put(key, masks.clone(), pixels * input.frames.len());
            masks
        };
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for (index, mask) in masks.iter().enumerate() {
            cancelled(cancel)?;
            let frame = input.frames.get(index).ok_or("Missing input frame")?;
            let values = if input.quality {
                let mut trimap = pixels::trimap(mask, input.width, input.height, input.band)?;
                let mut painted = vec![false; pixels];
                for point in input.points.iter().filter(|p| p.frame == index && p.kind == Kind::Refine) {
                    let radius = (input.band * 2) as f32;
                    let x0 = (point.x - radius).floor().max(0.0) as usize;
                    let y0 = (point.y - radius).floor().max(0.0) as usize;
                    let x1 = ((point.x + radius).ceil() as usize).saturating_add(1).min(input.width);
                    let y1 = ((point.y + radius).ceil() as usize).saturating_add(1).min(input.height);
                    for y in y0..y1 {
                        for x in x0..x1 {
                            if (x as f32 - point.x).powi(2) + (y as f32 - point.y).powi(2) <= radius * radius {
                                trimap[y * input.width + x] = 128;
                                painted[y * input.width + x] = true;
                            }
                        }
                    }
                }
                if let Some(base) = input.base_alpha {
                    // Refining a propagated matte is a local operation. Preserve its soft
                    // detail elsewhere exactly, and run tiles only where the brush painted.
                    for ((value, painted), base) in trimap.iter_mut().zip(&painted).zip(base) {
                        if !painted {
                            *value = if *base > 32767 { 255 } else { 0 };
                        }
                    }
                }
                let mut digest = Sha256::new();
                digest.update(input.width.to_le_bytes());
                digest.update(input.height.to_le_bytes());
                digest.update(frame.rgba);
                digest.update(&trimap);
                let key: [u8; 32] = digest.finalize().into();
                let alpha = if let Some(alpha) = self.alpha.get(&key) {
                    alpha
                } else {
                    let alpha = Arc::new(self.refine(frame.rgba, input.width, input.height, &trimap, cancel, matte_workers)?);
                    pending.push((key, alpha.clone()));
                    alpha
                };
                if let Some(base) = input.base_alpha {
                    Arc::new(alpha.iter().zip(base).zip(&painted).map(|((refined, base), painted)| if *painted { *refined } else { *base }).collect())
                } else {
                    alpha
                }
            } else {
                Arc::new(mask.iter().map(|m| if *m { 65535 } else { 0 }).collect())
            };
            frames.push(RasterFrame { time: frame.time, runs: Arc::new(runs(&values)?) });
            progress(input.frames.len() + index + 1, if input.quality { "Refining with native ViTMatte" } else { "Saving native Fast alpha" });
        }
        let result = RasterMask { width: input.width as u32, height: input.height as u32, end: input.end, quality: input.quality, frames };
        result.validate()?;
        cancelled(cancel)?;
        for (key, values) in pending {
            self.alpha.put(key, values, pixels * 2);
        }
        Ok(result)
    }

    /// Run bounded overlapping ViTMatte tiles and preserve definite foreground/background pixels.
    fn refine(&mut self, rgba: &[u8], width: usize, height: usize, trimap: &[u8], cancel: &AtomicBool, workers: usize) -> Result<Vec<u16>, String> {
        let mut alpha: Vec<_> = trimap.iter().map(|p| if *p == 255 { 65535u16 } else { 0 }).collect();
        let mut tiles = Vec::with_capacity(4);
        for y in (0..height).step_by(384) {
            for x in (0..width).step_by(384) {
                cancelled(cancel)?;
                let x1 = (x + 384).min(width);
                let y1 = (y + 384).min(height);
                if !(y..y1).any(|y| trimap[y * width + x..y * width + x1].contains(&128)) {
                    continue;
                }
                let rx = x.saturating_sub(64);
                let ry = y.saturating_sub(64);
                let rw = (x1 + 64).min(width) - rx;
                let rh = (y1 + 64).min(height) - ry;
                let pw = rw.div_ceil(32) * 32;
                let ph = rh.div_ceil(32) * 32;
                let plane = pw * ph;
                let mut pixels = vec![0f32; plane * 4];
                for iy in 0..rh {
                    for ix in 0..rw {
                        let src = (ry + iy) * width + rx + ix;
                        let dest = iy * pw + ix;
                        for (channel, (mean, std)) in [(0.5, 0.5); 3].into_iter().enumerate() {
                            pixels[channel * plane + dest] = (rgba[src * 4 + channel] as f32 / 255.0 - mean) / std;
                        }
                        pixels[3 * plane + dest] = trimap[src] as f32 / 255.0;
                    }
                }
                let pixels = Floats { shape: vec![1, 4, ph, pw], data: pixels };
                let position = self.positions.get(ph / 16, pw / 16)?;
                tiles.push(MatteTile { core: (x, y, x1, y1), origin: (rx, ry), pixels, position });
            }
        }
        let predictions = tile_map(&tiles, workers, |tile| {
            cancelled(cancel)?;
            let [prediction]: [Floats; 1] = self
                .models
                .matte
                .run(vec![("pixels", tile.pixels.view()?), ("position", tile.position.view()?)], &["alpha"])?
                .try_into()
                .map_err(|_| "Matte output count differs")?;
            cancelled(cancel)?;
            prediction.checked(&[1, 1, tile.pixels.shape[2], tile.pixels.shape[3]])
        })?;
        cancelled(cancel)?;
        for (tile, prediction) in tiles.iter().zip(predictions) {
            let (x, y, x1, y1) = tile.core;
            let (rx, ry) = tile.origin;
            let pw = tile.pixels.shape[3];
            for iy in y..y1 {
                for ix in x..x1 {
                    if trimap[iy * width + ix] == 128 {
                        alpha[iy * width + ix] = (prediction.data[(iy - ry) * pw + ix - rx].clamp(0.0, 1.0) * 65535.0).round_ties_even() as u16;
                    }
                }
            }
        }
        Ok(alpha)
    }
}

/// Hash image dimensions and exact RGBA contents for inference-cache identity.
fn image_key(width: usize, height: usize, rgba: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(width.to_le_bytes());
    digest.update(height.to_le_bytes());
    digest.update(rgba);
    digest.finalize().into()
}

/// Assemble a bounded temporal bank and positional offsets for one tracking step.
fn memory_bank(
    index: usize,
    reverse: bool,
    frames: usize,
    conditional: &BTreeMap<usize, Memory>,
    other: &BTreeMap<usize, Memory>,
    temporal: &[f32],
) -> Result<(Floats, Floats, Floats), String> {
    if temporal.len() != 7 * 64 {
        return Err("Invalid temporal memory weights".into());
    }
    let mut selected: Vec<_> = conditional.values().map(|m| (m, 6)).collect();
    for t_pos in 1..7 {
        let distance = 7 - t_pos;
        let previous = if reverse { index.checked_add(distance).filter(|i| *i < frames) } else { index.checked_sub(distance) };
        if let Some(memory) = previous.and_then(|i| other.get(&i)) {
            selected.push((memory, 6 - t_pos));
        }
    }
    let mut memory = Vec::new();
    let mut position = Vec::new();
    for (frame, time_index) in &selected {
        memory.extend_from_slice(&frame.features.data);
        position.extend(frame.position.data.iter().enumerate().map(|(i, value)| value + temporal[time_index * 64 + i % 64]));
    }
    let mut pointers = Vec::new();
    for (i, memory) in conditional {
        if if reverse { *i >= index } else { *i <= index } {
            pointers.extend_from_slice(&memory.pointer.data);
        }
    }
    for distance in 1..frames.min(16) {
        let previous = if reverse { index.checked_add(distance).filter(|i| *i < frames) } else { index.checked_sub(distance) };
        let Some(previous) = previous else {
            break;
        };
        if let Some(memory) = other.get(&previous) {
            pointers.extend_from_slice(&memory.pointer.data);
        }
    }
    if pointers.is_empty() || selected.is_empty() || selected.len() > 22 {
        return Err("Invalid temporal memory bank".into());
    }
    let pointer_count = pointers.len() / 256;
    Ok((
        Floats { shape: vec![selected.len(), 512, 64], data: memory },
        Floats { shape: vec![selected.len(), 512, 64], data: position },
        Floats { shape: vec![pointer_count, 256], data: pointers },
    ))
}

/// Encode 16-bit alpha with positive, bounded runs or report an exceeded output budget.
fn runs(values: &[u16]) -> Result<Vec<(u32, u16)>, String> {
    let mut runs: Vec<(u32, u16)> = Vec::new();
    for value in values {
        if let Some(last) = runs.last_mut().filter(|last| last.1 == *value) {
            last.0 = last.0.checked_add(1).ok_or("alpha run overflow")?;
        } else {
            runs.push((1, *value));
        }
        if runs.len() > 1_000_000 {
            return Err("Alpha exceeds the run budget".into());
        }
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_workers_preserve_order_and_join_after_errors_and_panics() {
        use std::sync::atomic::AtomicUsize;
        assert_eq!(tile_map(&[3, 2, 1, 0], 4, |n| Ok(n * 7)).unwrap(), [21, 14, 7, 0]);
        for panic in [false, true] {
            let finished = AtomicUsize::new(0);
            let result = tile_map(&[0, 1, 2, 3], 4, |n| {
                if *n == 0 {
                    if panic {
                        panic!("simulated model panic");
                    }
                    return Err("simulated cancellation".into());
                }
                finished.fetch_add(1, Ordering::Relaxed);
                Ok(*n)
            });
            assert!(result.is_err());
            assert_eq!(finished.load(Ordering::Relaxed), 3, "no worker remains after failure");
        }
        assert!(tile_map(&[0; 5], 4, |n| Ok(*n)).is_err());
        assert!(tile_map(&[0], 0, |n| Ok(*n)).is_err());
        assert!(tile_map(&[0], 5, |n| Ok(*n)).is_err());
        assert!(tile_map(&[0], 1, |_| -> Result<(), String> { panic!("serial model panic") }).is_err());
    }

    #[test]
    fn matte_positions_reuse_exact_embeddings_and_bound_changing_tile_sizes() {
        let base = Floats { shape: vec![1, 197, 384], data: (0..197 * 384).map(|i| (i % 113) as f32 / 113.0).collect() };
        let expected = pixels::absolute_position(&base.data, 26, 28).unwrap();
        let mut positions = MattePositions::new(base).unwrap();
        let first = positions.get(26, 28).unwrap();
        assert_eq!(first.data, expected);
        assert!(Arc::ptr_eq(&first, &positions.get(26, 28).unwrap()));
        for size in 1..=32 {
            positions.get(size, size).unwrap();
            assert!(positions.cache.entries.len() <= 4);
            assert!(positions.cache.bytes <= 6 * 1024 * 1024);
        }
        assert_eq!(positions.get(26, 28).unwrap().data, expected);
        assert!(positions.get(0, 28).is_err());
        assert!(positions.get(26, usize::MAX).is_err());
        assert!(MattePositions::new(Floats { shape: vec![1, 197, 384], data: vec![f32::NAN; 197 * 384] }).is_err());
    }

    #[test]
    fn caches_evict_by_bytes_and_entries_and_refresh_lru() {
        let mut cache = Cache::new(2, 10);
        cache.put([1; 32], Arc::new(1), 4);
        cache.put([2; 32], Arc::new(2), 4);
        assert_eq!(*cache.get(&[1; 32]).unwrap(), 1);
        cache.put([3; 32], Arc::new(3), 4);
        assert!(cache.get(&[2; 32]).is_none());
        cache.put([4; 32], Arc::new(4), 11);
        assert!(cache.get(&[4; 32]).is_none());
        cache.put([1; 32], Arc::new(5), 9);
        assert_eq!(*cache.get(&[1; 32]).unwrap(), 5);
        assert!(cache.get(&[3; 32]).is_none());
        assert_eq!(cache.bytes, 9);
    }

    #[test]
    fn cancellation_and_invalid_model_outputs_return_errors() {
        assert!(cancelled(&AtomicBool::new(true)).is_err());
        assert!(Floats { shape: vec![1], data: vec![f32::NAN] }.checked(&[1]).is_err());
        assert!(Floats { shape: vec![2], data: vec![0.0] }.checked(&[2]).is_err());
        assert!(Floats { shape: vec![usize::MAX, 2], data: vec![] }.checked(&[usize::MAX, 2]).is_err());
        assert_eq!(runs(&[0, 0, 32768, 65535, 65535]).unwrap(), [(2, 0), (1, 32768), (2, 65535)]);
    }
}
