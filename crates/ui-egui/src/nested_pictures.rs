//! Bounded clipped inner pictures, owned by one frame server, never by a global renderer.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use filmcraft_engine::MediaPool;
use filmcraft_project::Project;
use filmcraft_render::{Image, NestedPictureRequest};

const PICTURES: usize = 2;
const BYTES: usize = 64 << 20;

struct Entry {
    request: NestedPictureRequest,
    project: Arc<Project>,
    revision: u64,
    pool: Weak<MediaPool>,
    generation: u64,
    catch_up: Option<filmcraft_time::Tick>,
    image: Arc<Image>,
    bytes: usize,
}
pub(crate) struct Pictures {
    entries: VecDeque<Entry>,
    budget: usize,
    limit: usize,
}
impl Default for Pictures {
    fn default() -> Self {
        Self { entries: VecDeque::new(), budget: BYTES, limit: PICTURES }
    }
}
fn image_bytes(image: &Image) -> Option<usize> {
    if image.w == 0 || image.h == 0 || image.w.checked_mul(image.h)?.checked_mul(4)? != image.px.len() {
        return None;
    }
    image.px.len().checked_mul(size_of::<f32>())
}
impl Pictures {
    fn get(&mut self, context: &PictureContext, request: NestedPictureRequest) -> Option<Arc<Image>> {
        let pool = Arc::downgrade(&context.pool);
        let index = self.entries.iter().position(|entry| {
            entry.request == request
                && entry.catch_up == context.catch_up
                && entry.generation == context.generation
                && Weak::ptr_eq(&entry.pool, &pool)
                && if Arc::ptr_eq(&entry.project, &context.project) {
                    entry.revision == context.revision
                } else {
                    crate::thumbnail_revision::dependencies_unchanged(&entry.project, &context.project, request.sequence)
                }
        })?;
        let mut entry = self.entries.remove(index)?;
        entry.project = context.project.clone();
        entry.revision = context.revision;
        let image = entry.image.clone();
        self.entries.push_back(entry);
        Some(image)
    }
    fn insert(&mut self, context: &PictureContext, request: NestedPictureRequest, image: Arc<Image>) {
        let Some(bytes) = image_bytes(&image).filter(|n| *n <= self.budget) else { return };
        if self.limit == 0 {
            return;
        }
        self.entries.retain(|e| e.request != request);
        while self.entries.len() >= self.limit || self.entries.iter().map(|e| e.bytes).sum::<usize>().saturating_add(bytes) > self.budget {
            if self.entries.pop_front().is_none() {
                return;
            }
        }
        self.entries.push_back(Entry {
            request,
            project: context.project.clone(),
            revision: context.revision,
            pool: Arc::downgrade(&context.pool),
            generation: context.generation,
            catch_up: context.catch_up,
            image,
            bytes,
        });
    }
}

/// Writes stay local to the job until its complete plan was successfully prepared.
/// A lookup holds metadata locks only; no lock is held during rasterization.
pub(crate) struct PictureContext {
    cache: Arc<Mutex<Pictures>>,
    staged: Mutex<Pictures>,
    project: Arc<Project>,
    revision: u64,
    pool: Arc<MediaPool>,
    generation: u64,
    catch_up: Option<filmcraft_time::Tick>,
    cancel: Arc<AtomicBool>,
    pub(crate) source_failed: Arc<AtomicBool>,
}
impl PictureContext {
    pub fn new(
        cache: Arc<Mutex<Pictures>>,
        project: Arc<Project>,
        revision: u64,
        pool: Arc<MediaPool>,
        cancel: Arc<AtomicBool>,
        catch_up: Option<filmcraft_time::Tick>,
    ) -> Self {
        let generation = pool.content_generation();
        Self {
            cache,
            staged: Mutex::new(Pictures::default()),
            project,
            revision,
            pool,
            generation,
            catch_up,
            cancel,
            source_failed: Arc::new(AtomicBool::new(false)),
        }
    }
    fn valid(&self) -> bool {
        !self.cancel.load(Ordering::Relaxed)
            && !self.source_failed.load(Ordering::Relaxed)
            && self.pool.content_generation() == self.generation
            && !filmcraft_media::pending::is_set()
    }
    pub fn get(&self, request: NestedPictureRequest) -> Option<Arc<Image>> {
        if !self.valid() {
            return None;
        }
        if let Some(image) = self.staged.lock().unwrap_or_else(|e| e.into_inner()).get(self, request) {
            return Some(image);
        }
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(self, request)
    }
    pub fn retain(&self, request: NestedPictureRequest, image: Arc<Image>) {
        if self.valid() {
            self.staged.lock().unwrap_or_else(|e| e.into_inner()).insert(self, request, image);
        }
    }
    pub fn publish(&self) {
        if !self.valid() {
            return;
        }
        let staged = std::mem::take(&mut *self.staged.lock().unwrap_or_else(|e| e.into_inner()));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for entry in staged.entries {
            if self.valid() {
                cache.insert(self, entry.request, entry.image);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::ItemId;
    use filmcraft_time::Tick;
    fn request(t: i64) -> NestedPictureRequest {
        NestedPictureRequest { sequence: ItemId(1), time: Tick(t), scale_bits: 1.0f32.to_bits(), effects: true, depth: 1 }
    }
    fn context(cache: Arc<Mutex<Pictures>>, project: Arc<Project>, pool: Arc<MediaPool>, catch_up: Option<Tick>) -> PictureContext {
        PictureContext::new(cache, project, 1, pool, Arc::new(AtomicBool::new(false)), catch_up)
    }
    fn image() -> Arc<Image> {
        Arc::new(Image::filled(2, 2, [0.2, 0.8, 0.1, 1.0]))
    }
    #[test]
    fn staged_pictures_are_private_until_successful_publication() {
        let cache = Arc::new(Mutex::new(Pictures::default()));
        let project = Arc::new(Project::new("staging"));
        let pool = Arc::new(MediaPool::default());
        let writer = context(cache.clone(), project.clone(), pool.clone(), None);
        let reader = context(cache, project, pool, None);
        let pixels = image();
        writer.retain(request(0), pixels.clone());
        assert!(Arc::ptr_eq(&writer.get(request(0)).unwrap(), &pixels));
        assert!(reader.get(request(0)).is_none());
        writer.publish();
        assert!(Arc::ptr_eq(&reader.get(request(0)).unwrap(), &pixels));
    }
    #[test]
    fn cancelled_pending_failed_or_changed_pool_jobs_publish_nothing() {
        for failure in 0..4 {
            let cache = Arc::new(Mutex::new(Pictures::default()));
            let project = Arc::new(Project::new("invalid completion"));
            let pool = Arc::new(MediaPool::default());
            let writer = context(cache.clone(), project.clone(), pool.clone(), None);
            writer.retain(request(0), image());
            match failure {
                0 => writer.cancel.store(true, Ordering::Relaxed),
                1 => filmcraft_media::pending::mark(),
                2 => writer.source_failed.store(true, Ordering::Relaxed),
                _ => pool.set_use_proxies(true),
            }
            writer.publish();
            let _ = filmcraft_media::pending::take();
            assert!(cache.lock().unwrap().entries.is_empty(), "invalid work entered shared cache");
            assert!(context(cache, project, pool, None).get(request(0)).is_none());
        }
    }
    #[test]
    fn pool_identity_generation_and_catch_up_context_cannot_alias() {
        let cache = Arc::new(Mutex::new(Pictures::default()));
        let project = Arc::new(Project::new("contexts"));
        let pool = Arc::new(MediaPool::default());
        let writer = context(cache.clone(), project.clone(), pool.clone(), Some(Tick(1)));
        writer.retain(request(0), image());
        writer.publish();
        assert!(context(cache.clone(), project.clone(), pool.clone(), Some(Tick(1))).get(request(0)).is_some());
        assert!(context(cache.clone(), project.clone(), pool.clone(), Some(Tick(2))).get(request(0)).is_none());
        assert!(context(cache.clone(), project.clone(), Arc::new(MediaPool::default()), Some(Tick(1))).get(request(0)).is_none());
        pool.set_use_proxies(true);
        assert!(context(cache, project, pool, Some(Tick(1))).get(request(0)).is_none());
    }
    #[test]
    fn byte_and_entry_limits_evict_actual_pictures() {
        for (budget, limit, retained) in [(128, 2, 2), (64, 2, 1), (128, 1, 1)] {
            let cache = Arc::new(Mutex::new(Pictures { entries: VecDeque::new(), budget, limit }));
            let project = Arc::new(Project::new("bounded"));
            let pool = Arc::new(MediaPool::default());
            for t in 0..3 {
                let writer = context(cache.clone(), project.clone(), pool.clone(), None);
                writer.retain(request(t), image());
                writer.publish();
            }
            let reader = context(cache.clone(), project, pool, None);
            assert!(reader.get(request(0)).is_none());
            assert!(reader.get(request(2)).is_some());
            let store = cache.lock().unwrap();
            assert_eq!(store.entries.len(), retained);
            assert!(store.entries.iter().map(|e| e.bytes).sum::<usize>() <= budget);
        }
    }
    #[test]
    fn invalid_shapes_overflow_and_oversized_images_are_rejected() {
        let cache = Arc::new(Mutex::new(Pictures { entries: VecDeque::new(), budget: 64, limit: 2 }));
        let writer = context(cache.clone(), Arc::new(Project::new("hostile shapes")), Arc::new(MediaPool::default()), None);
        for image in
            [Image { w: 0, h: 1, px: vec![] }, Image { w: usize::MAX, h: 2, px: vec![] }, Image { w: 2, h: 2, px: vec![0.0] }, Image::filled(3, 3, [1.0; 4])]
        {
            writer.retain(request(0), Arc::new(image));
            writer.publish();
            assert!(cache.lock().unwrap().entries.is_empty());
        }
    }
}
