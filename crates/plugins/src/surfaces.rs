//! Panels and data plane contributions the plugin host hands to the app.

use std::sync::{Arc, Mutex, PoisonError};

use craft_command::Owner;
use craft_plugin_abi::manifest::Kind;
use craft_plugin_host::contrib::format::FormatBook;
use craft_plugin_host::contrib::surface::{Surface, SurfaceSink};

/// Every live panel, effect and filter, by owner. A reload calls `remove` and then `add`.
#[derive(Default)]
pub struct Surfaces {
    items: Mutex<Vec<Surface>>,
    /// Gets every `format` surface too: [`crate::formats`] opens files through it.
    pub formats: Arc<FormatBook>,
}

impl Surfaces {
    pub fn new() -> Arc<Surfaces> {
        Arc::new(Surfaces::default())
    }

    fn items(&self) -> std::sync::MutexGuard<'_, Vec<Surface>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// All live surfaces of `kind`.
    pub fn of_kind(&self, kind: &Kind) -> Vec<Surface> {
        self.items().iter().filter(|s| &s.contribution.kind == kind).cloned().collect()
    }

    pub fn find(&self, plugin_id: &str, contribution_id: &str) -> Option<Surface> {
        self.items().iter().find(|s| &*s.owner.plugin_id == plugin_id && s.contribution.id == contribution_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.items().len()
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }
}

impl SurfaceSink for Surfaces {
    fn add(&self, surface: Surface) {
        self.formats.add(surface.clone());
        if let Err(why) = crate::effect::register(&surface) {
            log::warn!("effect {} of {} is left out: {why}", surface.contribution.id, surface.owner.plugin_id);
        }
        let mut items = self.items();
        items.retain(|s| !(s.owner == surface.owner && s.contribution.id == surface.contribution.id));
        items.push(surface);
    }

    fn remove(&self, owner: &Owner) {
        self.formats.remove(owner);
        for surface in self.items().iter().filter(|s| &s.owner == owner && s.contribution.kind == Kind::Effect) {
            crate::effect::unregister(owner, &surface.contribution.id);
        }
        self.items().retain(|s| &s.owner != owner);
    }
}
