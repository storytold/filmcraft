//! The session's plug-ins, and the plug-ins' part of the preferences.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use craft_plugin::fetch::Fetch;
use craft_plugin::{Manifest, Source};
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::install::Pending;
use super::{MAX_CATALOGS, MAX_SETTING_KEY, MAX_SETTINGS_BYTES, MAX_SETTINGS_PLUGINS};

/// An installed plug-in.
#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    pub manifest: Manifest,
    /// The SHA-256 of its bundle (lower-case hex).
    pub sha256: String,
    /// Where it came from.
    pub source: Source,
    /// Kept in the user's `Plug-ins` folder; otherwise for this session only (the CLI, MCP, the web).
    pub stored: bool,
    pub(crate) wasm: Arc<Vec<u8>>,
}

impl InstalledPlugin {
    /// What `plugin.list` says about it.
    pub fn describe(&self) -> Value {
        let mut v = serde_json::to_value(&self.manifest).unwrap_or_else(|_| json!({"id": self.manifest.id}));
        v["kind"] = json!(self.manifest.kind);
        v["sha256"] = json!(self.sha256);
        v["source"] = serde_json::to_value(&self.source).unwrap_or(Value::Null);
        v["stored"] = json!(self.stored);
        v
    }
}

/// The plug-ins' part of the preferences (`pluginData` in `preferences.json`): each plug-in's own
/// settings and the catalogs added. Read leniently: what is malformed or over the limits is
/// dropped, never the rest of the preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize, serde::Deserialize)]
#[serde(from = "Value")]
pub struct PluginPrefs {
    /// Each plug-in's settings, by plug-in id ([`MAX_SETTINGS_BYTES`] each).
    pub settings: BTreeMap<String, Map<String, Value>>,
    /// Plug-in catalogs added with `plugin.catalog.add` (`index.json` URLs).
    pub catalogs: Vec<String>,
}

impl From<Value> for PluginPrefs {
    fn from(v: Value) -> Self {
        let mut out = PluginPrefs::default();
        if let Some(all) = v.get("settings").and_then(Value::as_object) {
            for (id, mine) in all.iter().take(MAX_SETTINGS_PLUGINS) {
                let Some(mine) = mine.as_object() else { continue };
                let mut kept = Map::new();
                let mut size = 0usize;
                for (k, v) in mine {
                    let len = k.len().saturating_add(v.to_string().len());
                    if valid_setting_key(k) && size.saturating_add(len) <= MAX_SETTINGS_BYTES {
                        size = size.saturating_add(len);
                        kept.insert(k.clone(), v.clone());
                    }
                }
                if !id.is_empty() && !kept.is_empty() {
                    out.settings.insert(id.clone(), kept);
                }
            }
        }
        for url in v.get("catalogs").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if out.catalogs.len() < MAX_CATALOGS && !out.catalogs.iter().any(|u| u == url) && super::install::check_catalog_url(url).is_ok() {
                out.catalogs.push(url.to_string());
            }
        }
        out
    }
}

/// Whether `key` may name a plug-in setting: 1–[`MAX_SETTING_KEY`] characters, none a control
/// character.
pub(super) fn valid_setting_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= MAX_SETTING_KEY && !key.chars().any(char::is_control)
}

/// What settings take up: their keys and JSON values, in bytes.
pub(super) fn settings_size<'a>(entries: impl Iterator<Item = (&'a String, &'a Value)>) -> usize {
    entries.map(|(k, v)| k.len().saturating_add(v.to_string().len())).fold(0, usize::saturating_add)
}

/// The HTTPS client installs download with, shared with the workers the UI runs downloads on.
/// Without one, only files and `file://` catalogs install.
#[derive(Clone, Default)]
pub struct Fetcher(Arc<Mutex<Option<Box<dyn Fetch + Send>>>>);

impl Fetcher {
    /// Run `f` with the client (or [`NoNetwork`]).
    pub fn with<T>(&self, f: impl FnOnce(&mut dyn Fetch) -> T) -> T {
        let mut client = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match client.as_deref_mut() {
            Some(http) => f(http),
            None => f(&mut NoNetwork),
        }
    }

    /// Whether there is a client (else only local files install).
    pub fn has_network(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }
}

/// Longest part of a URL an error repeats.
const SHOWN_URL_CHARS: usize = 120;

/// The fetcher of sessions without an HTTPS client: `file://` catalogs still read.
struct NoNetwork;

impl Fetch for NoNetwork {
    fn get(&mut self, url: &str, _max: usize) -> std::result::Result<Option<Vec<u8>>, String> {
        let shown: String = url.chars().take(SHOWN_URL_CHARS).collect();
        Err(format!("there is no network access here to read {shown}: install the plug-in from a file, or in the FilmCraft app"))
    }
}

/// The session's plug-ins: the installed ones, where they are kept and the HTTPS client installs
/// download with. Their settings and catalogs are in the preferences (`Session::prefs`). The
/// desktop app opens the store ([`Runtime::open_store`], from `Session::start_autosave`) and sets
/// the client ([`Runtime::set_fetch`]); the CLI, MCP and the web keep plug-ins for the session.
#[derive(Default)]
pub struct Runtime {
    /// The user's `Plug-ins` folder.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) store: Option<craft_plugin::store::Store>,
    plugins: Vec<InstalledPlugin>,
    /// Plug-in folders that couldn't be read, and plug-ins this version can't run.
    problems: Vec<String>,
    fetch: Fetcher,
    pub(crate) pending: Option<Pending>,
    /// The last run (plug-in id, command id, params).
    pub last: Option<(String, String, Value)>,
}

impl Runtime {
    /// Keep plug-ins in `<data_dir>/Plug-ins` and read the ones installed there.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_store(&mut self, data_dir: &std::path::Path) {
        let store = craft_plugin::store::Store::new(craft_plugin::store::plugins_dir(data_dir));
        let listing = store.list_for(&super::APP);
        self.plugins.retain(|p| !p.stored);
        self.problems = listing.problems.into_iter().map(|p| format!("{}: {}", p.dir.display(), p.error)).collect();
        // The store keeps whatever was installed; only plug-ins for this FilmCraft are offered
        self.problems.extend(listing.not_runnable.into_iter().map(|(i, why)| format!("{}: {why}", i.manifest.name)));
        for (i, _) in listing.runnable {
            let plugin =
                InstalledPlugin { manifest: i.manifest, sha256: i.record.sha256, source: i.record.source, stored: true, wasm: Arc::new(i.bundle.wasm) };
            self.keep(plugin);
        }
        self.store = Some(store);
    }

    /// Download catalogs and releases with `client`.
    pub fn set_fetch(&mut self, client: Option<Box<dyn Fetch + Send>>) {
        *self.fetch.0.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }

    /// The HTTPS client, for a worker thread that downloads ([`super::download`]).
    pub fn fetcher(&self) -> Fetcher {
        self.fetch.clone()
    }

    /// The installed plug-ins.
    pub fn list(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    /// Why plug-in folders couldn't be used.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Plug-in `id` (ignoring case, as the store does).
    pub fn get(&self, id: &str) -> Option<&InstalledPlugin> {
        self.plugins.iter().find(|p| p.manifest.id.eq_ignore_ascii_case(id))
    }

    /// Whether plug-ins are kept in the user's folder (else for this session only).
    pub fn has_store(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.store.is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// Put `plugin` in the list, in place of one with the same id.
    pub(crate) fn keep(&mut self, plugin: InstalledPlugin) -> InstalledPlugin {
        match self.plugins.iter_mut().find(|p| p.manifest.id.eq_ignore_ascii_case(&plugin.manifest.id)) {
            Some(slot) => *slot = plugin.clone(),
            None => self.plugins.push(plugin.clone()),
        }
        plugin
    }

    /// Drop plug-in `id` from the list.
    pub(crate) fn forget(&mut self, id: &str) {
        self.plugins.retain(|p| !p.manifest.id.eq_ignore_ascii_case(id));
        if self.last.as_ref().is_some_and(|(last, ..)| last.eq_ignore_ascii_case(id)) {
            self.last = None;
        }
    }
}
