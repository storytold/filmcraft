//! The plugin host of one FilmCraft session: manager, hook layer, surfaces, the bridge to the
//! session, and the install flow the manager view and the control channel share.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use craft_command::Registry;
use craft_plugin_abi::{HostPolicy, Permission};
use craft_plugin_host::budget::BudgetConfig;
use craft_plugin_host::contrib::hook::HookHost;
use craft_plugin_host::dispatch::DispatchConfig;
use craft_plugin_host::engine::wasmi::WasmiEngine;
use craft_plugin_host::engine::{Clock, SystemClock, Trust};
use craft_plugin_host::lifecycle::{Decision, InstallOutcome, LifecycleError, ManagerConfig, PluginManager, PluginState};
use craft_plugin_host::link::LinkClient;
use craft_plugin_host::store::{DirStore, MemoryStore, PackageStore, Source, StateStore};
use filmcraft_engine::project::Project;
use filmcraft_engine::{EngineError, Session};

use crate::bridge::Bridge;
use crate::services::AppServices;
use crate::surfaces::Surfaces;
use crate::ui::UiState;

/// How the host starts.
pub struct HostConfig {
    /// Where packages, grants and `state.*` data live. `None` keeps them in memory (tests).
    pub store_dir: Option<PathBuf>,
    /// Craft Link access for `link.call`. `None` answers those calls with an error.
    pub link: Option<LinkClient>,
    /// How long a worker thread waits for the session thread to serve a host call.
    pub wait: Duration,
    /// Time source for guest deadlines and hook budgets. `None` is the system clock. Tests inject a fixed one.
    pub clock: Option<Arc<dyn Clock>>,
}

/// A Craft Link client for this app, or `None` when the runtime directory cannot be resolved.
pub fn system_link_client() -> Option<LinkClient> {
    let runtime = craft_ipc::RuntimeDir::resolve().ok()?;
    LinkClient::for_app(runtime, "filmcraft", env!("CARGO_PKG_VERSION")).ok()
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig { store_dir: None, link: None, wait: Duration::from_secs(5), clock: None }
    }
}

pub type Manager = PluginManager<Session, EngineError>;

pub struct PluginHost {
    pub manager: Arc<Manager>,
    pub bridge: Arc<Bridge>,
    pub services: Arc<AppServices>,
    pub hook_host: Arc<HookHost<Session, Arc<Project>>>,
    pub surfaces: Arc<Surfaces>,
    pub registry: Arc<Registry<Session, EngineError>>,
    pub(crate) ui: Mutex<UiState>,
    /// What `restore` found at start: id and state of every stored plug-in.
    pub restored: Vec<(String, PluginState)>,
}

impl PluginHost {
    /// Create the host for `session` and load the stored plug-ins. Call on the thread that owns
    /// the session. Registers the host's own commands, attaches the
    /// hook layer and the surfaces.
    pub fn start(session: &mut Session, cfg: HostConfig) -> Result<Arc<PluginHost>, LifecycleError> {
        let bridge = Bridge::new(cfg.wait);
        let registry = Arc::clone(&session.registry);
        let services = AppServices::new(Arc::clone(&bridge), Arc::clone(&registry), cfg.link);
        let clock: Arc<dyn Clock> = cfg.clock.unwrap_or_else(|| Arc::new(SystemClock::new()));
        let engine = Arc::new(WasmiEngine::new(Arc::clone(&clock)));
        let (packages, state): (Arc<dyn PackageStore>, Arc<dyn StateStore>) = match cfg.store_dir {
            Some(dir) => {
                let store = Arc::new(DirStore::new(dir));
                (store.clone(), store)
            }
            None => {
                let store = Arc::new(MemoryStore::new());
                (store.clone(), store)
            }
        };
        let manager_cfg =
            ManagerConfig { policy: HostPolicy::native(), trust: Trust::Untrusted, budgets: BudgetConfig::default(), dispatch: DispatchConfig::default() };
        let manager = PluginManager::new(engine, manager_cfg, Arc::clone(&registry), packages, state, services.clone(), Arc::clone(&clock));
        let hook_host = HookHost::<Session, Arc<Project>>::new(session.hooks.clone(), Arc::clone(manager.router()), services.clone(), clock);
        manager.attach_hooks(hook_host.clone());
        let surfaces = Surfaces::new();
        manager.attach_surfaces(surfaces.clone());
        crate::formats::attach(session, &surfaces.formats);
        let restored = manager.restore()?;
        let host = Arc::new(PluginHost { manager, bridge, services, hook_host, surfaces, registry, ui: Mutex::new(UiState::default()), restored });
        crate::commands::register(&host).map_err(LifecycleError::Manifest)?;
        host.note_restored();
        Ok(host)
    }

    pub(crate) fn ui_state(&self) -> MutexGuard<'_, UiState> {
        self.ui.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Once per frame, on the session thread: serve the jobs other threads queued and show the
    /// notices plug-ins and the host raised.
    pub fn pump(&self, session: &mut Session) {
        self.bridge.pump(session);
        for n in self.bridge.drain_notices() {
            let who = if n.plugin_id.is_empty() { "plugin".to_owned() } else { format!("plugin:{}", n.plugin_id) };
            session.error_toast(&who, n.message);
        }
    }

    fn note_restored(&self) {
        let broken: Vec<&str> = self.restored.iter().filter(|(_, s)| matches!(s, PluginState::Faulted { .. })).map(|(id, _)| id.as_str()).collect();
        if !broken.is_empty() {
            self.ui_state().message = Some(format!("could not load: {}. Open Plug-ins to see why.", broken.join(", ")));
        }
    }

    // ---- install flow ---------------------------------------------------------------------

    /// Gate and compile `bytes`. A plug-in that needs permissions waits for the user's answer
    /// (the prompt is queued for the UI, and [`PluginHost::answer`] settles it). Nothing of it runs
    /// before that.
    pub fn install_bytes(&self, bytes: &[u8], source: Source) -> Result<InstallOutcome, LifecycleError> {
        let outcome = self.manager.install(bytes, source);
        let mut ui = self.ui_state();
        match &outcome {
            Ok(InstallOutcome::NeedsGrant(request)) => {
                ui.queue_prompt(request);
                ui.message = Some(format!("{} {} waits for your permission", request.plugin_id, request.version));
            }
            Ok(done) => ui.message = Some(format!("installed: {done:?}")),
            Err(e) => ui.message = Some(format!("install failed: {e}")),
        }
        outcome
    }

    /// Install from a `.wasm` file.
    pub fn install_path(&self, path: &Path) -> Result<InstallOutcome, InstallError> {
        let bytes = std::fs::read(path).map_err(|e| InstallError::Read(format!("{}: {e}", path.display())))?;
        self.install_bytes(&bytes, Source::File).map_err(InstallError::Lifecycle)
    }

    /// The user's answer to a permission prompt. `optional` lists the optional permissions allowed.
    pub fn answer(&self, plugin_id: &str, accept_required: bool, optional: Vec<Permission>) -> Result<InstallOutcome, LifecycleError> {
        let outcome = self.manager.grant(plugin_id, Decision { accept_required, optional });
        let mut ui = self.ui_state();
        ui.drop_prompt(plugin_id);
        ui.message = Some(match &outcome {
            Ok(done) => format!("{plugin_id}: {done:?}"),
            Err(e) => format!("{plugin_id}: {e}"),
        });
        outcome
    }
}

/// Why `install_path` failed.
#[derive(Debug)]
pub enum InstallError {
    Read(String),
    Lifecycle(LifecycleError),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Read(m) => f.write_str(m),
            InstallError::Lifecycle(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstallError {}
