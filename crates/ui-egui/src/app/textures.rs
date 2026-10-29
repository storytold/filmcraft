use super::*;

impl FilmcraftApp {
    // ---------------------------------------------------------------- textures

    /// Upload a rendered frame into a named texture (only when the key changed), opaque: the
    /// picture over black.
    pub fn texture_for(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        self.upload_frame(ctx, name, key, img, false)
    }

    /// Like [`Self::texture_for`], keeping the frame's alpha, so what is painted under the
    /// texture (the monitors' Transparency Grid) shows through its transparent areas.
    pub fn texture_with_alpha(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        self.upload_frame(ctx, name, key, img, true)
    }

    /// Fetch or create the named texture and upload only when `key` changed; `alpha` selects
    /// transparent or opaque conversion. Returns the texture's stable id.
    fn upload_frame(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba, alpha: bool) -> egui::TextureId {
        let image = || img.color_image(alpha);
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                tex.set(image(), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let tex = ctx.load_texture(name, image(), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    /// Like [`Self::texture_for`], uploading `map(img)` (computed only when the key changed).
    pub fn texture_for_mapped(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        key: FrameKey,
        img: &frames::Rgba,
        map: impl FnOnce(&frames::Rgba) -> frames::Rgba,
    ) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                let m = map(img);
                tex.set(egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let m = map(img);
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    pub fn texture_existing(&self, name: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        self.textures.get(name).map(|(_, t)| (t.id(), t.size_vec2()))
    }

    /// The frame a named texture shows.
    pub fn texture_key(&self, name: &str) -> Option<FrameKey> {
        self.textures.get(name).map(|(k, _)| *k)
    }

    /// The frame whose picture the Program monitor drew last: the frame due at the playhead, or
    /// the stand-in shown while that one renders (`perf.stats`, tests).
    pub fn program_picture(&self) -> Option<FrameKey> {
        self.program_shown
    }

    /// Get a thumbnail texture for an item at a media time (requested at low priority).
    /// Cache revision of a project item's own frames: media changes only when its file does
    /// (relink, Make Offline, proxies on/off), so its frames survive unrelated edits; other items
    /// (sequences) follow the project revision.
    pub fn item_revision(&self, item: filmcraft_project::ItemId) -> u64 {
        let p = &self.session.project;
        let target = match p.item(item).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Subclip { parent, .. }) => *parent,
            _ => item,
        };
        match p.item(target).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Media(m)) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                filmcraft_engine::media_pool::media_key(m).hash(&mut h);
                (m.proxy.is_some() && self.session.media.use_proxies()).hash(&mut h);
                h.finish()
            }
            _ => self.session.revision,
        }
    }

    pub fn thumbnail(&mut self, ctx: &egui::Context, item: filmcraft_project::ItemId, t: Tick, width: u32) -> Option<(egui::TextureId, egui::Vec2)> {
        let pi = self.session.project.item(item)?;
        let src_w = match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref()?.width,
            filmcraft_project::ItemKind::Sequence(s) => s.settings.width,
            _ => return None,
        };
        let rate = pi.frame_rate();
        let frame = rate.frame_at(t);
        let rev = self.item_revision(item);
        let key = FrameKey { target: Target::Item(item), frame, size: width, revision: rev, draft: false };
        let name = format!("thumb-{}-{}-{}", item.0, frame, width);
        if let Some(img) = self.frames.get(&key) {
            let id = self.texture_for(ctx, &name, key, &img);
            self.thumb_touched(ctx, &name);
            return Some((id, egui::vec2(img.w as f32, img.h as f32)));
        }
        let scale = width as f32 / src_w.max(1) as f32;
        let project = self.session.project.clone();
        self.frames.request(key, rate.tick_of(frame), scale, &project, 50);
        let shown = self.texture_existing(&name);
        if shown.is_some() {
            self.thumb_touched(ctx, &name);
        }
        shown
    }

    /// Note that thumbnail texture `name` is used in this pass, and free the least recently used
    /// thumbnails once there are more than [`MAX_THUMB_TEXTURES`]: every frame shown in a timeline
    /// tile or a hover scrub got a texture of its own that was never freed (#1682).
    fn thumb_touched(&mut self, ctx: &egui::Context, name: &str) {
        let pass = ctx.cumulative_pass_nr();
        if self.thumb_used.insert(name.to_string(), pass).is_some() || self.thumb_used.len() <= MAX_THUMB_TEXTURES {
            return;
        }
        // down to three quarters, never a thumbnail already drawn in this pass
        let mut old: Vec<(u64, String)> = self.thumb_used.iter().filter(|(_, p)| **p < pass).map(|(n, p)| (*p, n.clone())).collect();
        old.sort_unstable();
        let excess = self.thumb_used.len().saturating_sub(MAX_THUMB_TEXTURES * 3 / 4);
        for (_, n) in old.into_iter().take(excess) {
            self.thumb_used.remove(&n);
            self.textures.remove(&n);
        }
    }

    /// The number of thumbnail textures held (tests, diagnostics).
    pub fn thumbnail_textures(&self) -> usize {
        self.textures.keys().filter(|n| n.starts_with("thumb-")).count()
    }
}
