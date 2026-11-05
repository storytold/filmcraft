//! `Services` for FilmCraft: what the plugin host asks of the app.
//!
//! | Method | What FilmCraft does |
//! |---|---|
//! | `execute_command` | `Session::execute_as` with `Origin::Plugin` on the session ([`Bridge::execute`]), one undo step |
//! | `lend_session` | moves the session where host calls reach it for the length of a guest call ([`Bridge::lend_any`]) |
//! | `doc_query` | [`crate::doc::query`] on the session |
//! | `begin_call`, `end_call` | open and close a scope; a fault restores the checkpoint |
//! | `list_commands` | the registry, without the host's own management commands |
//! | `notify`, `log`, `progress` | queued for the session thread; [`crate::PluginHost::pump`] shows them |
//! | `link_apps`, `link_call` | the Craft Link client, if the app has one |
//!
//! The management commands (install, grant, remove...) are not in `list_commands`, and
//! `cmd.execute` refuses them. A plug-in that holds `commands.run` must not be able to answer
//! its own permission prompt.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use craft_command::{Owner, Registry};
use craft_plugin_abi::permissions::DocScope;
use craft_plugin_host::link::LinkClient;
use craft_plugin_host::services::{
    CallOutcome, CallScope, CommandFailure, CommandInfo, HostInfo, LinkApp, LinkFailure, LogLevel, QueryFailure, Services, UserNotice,
};
use filmcraft_engine::{EngineError, Session};
use serde_json::Value;

use crate::bridge::Bridge;
use crate::doc;

/// Ids that start with this belong to the host. A plug-in can neither list nor run them.
pub const HOST_PREFIX: &str = "plugin.app.filmcraft.";

/// Last `host.progress` of a plug-in.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub fraction: f64,
    pub message: Option<String>,
}

pub struct AppServices {
    bridge: Arc<Bridge>,
    registry: Arc<Registry<Session, EngineError>>,
    link: Option<LinkClient>,
    progress: Mutex<BTreeMap<String, Progress>>,
}

impl AppServices {
    pub fn new(bridge: Arc<Bridge>, registry: Arc<Registry<Session, EngineError>>, link: Option<LinkClient>) -> Arc<AppServices> {
        Arc::new(AppServices { bridge, registry, link, progress: Mutex::new(BTreeMap::new()) })
    }

    /// The last progress each plug-in reported.
    pub fn progress_of(&self, plugin_id: &str) -> Option<Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner).get(plugin_id).cloned()
    }
}

impl Services for AppServices {
    fn host_info(&self) -> HostInfo {
        HostInfo { app: "filmcraft".into(), version: env!("CARGO_PKG_VERSION").into() }
    }

    fn log(&self, owner: &Owner, level: LogLevel, message: &str) {
        let target = format!("plugin:{}", owner.plugin_id);
        match level {
            LogLevel::Debug => log::debug!(target: &target, "{message}"),
            LogLevel::Info => log::info!(target: &target, "{message}"),
            LogLevel::Warn => log::warn!(target: &target, "{message}"),
            LogLevel::Error => log::error!(target: &target, "{message}"),
        }
    }

    fn progress(&self, owner: &Owner, fraction: f64, message: Option<&str>) {
        let value = Progress { fraction: fraction.clamp(0.0, 1.0), message: message.map(str::to_owned) };
        self.progress.lock().unwrap_or_else(PoisonError::into_inner).insert(owner.plugin_id.to_string(), value);
    }

    fn list_commands(&self) -> Vec<CommandInfo> {
        let mut rows = Vec::new();
        self.registry.for_each(|v| {
            if !v.id.starts_with(HOST_PREFIX) {
                rows.push(CommandInfo { id: v.id.to_owned(), label: v.label.to_owned() });
            }
        });
        rows
    }

    fn execute_command(&self, _owner: &Owner, id: &str, params: &Value) -> Result<Value, CommandFailure> {
        if id.starts_with(HOST_PREFIX) {
            return Err(CommandFailure::Failed("this command manages plug-ins and is not available to plug-ins".into()));
        }
        self.bridge.execute(id, params)
    }

    fn doc_query(&self, _owner: &Owner, scope: DocScope, query: &Value) -> Result<Value, QueryFailure> {
        let query = query.clone();
        match self.bridge.with_session(move |s| doc::query(s, scope, &query)) {
            Ok(answer) => answer,
            Err(_) => Err(QueryFailure::Unsupported),
        }
    }

    fn unix_millis(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn fill_random(&self, buf: &mut [u8]) {
        if let Err(e) = getrandom::fill(buf) {
            // `Services::fill_random` cannot report a failure, so the host hands the guest this
            // zero-filled buffer as if it were random. The failure is logged. Reporting it to the guest
            // as a failed call takes a fallible `fill_random` in craft-plugin-host.
            log::error!("no operating system entropy: {e}");
            buf.fill(0);
        }
    }

    fn notify(&self, notice: UserNotice) {
        self.bridge.notify(notice);
    }

    fn link_apps(&self, _owner: &Owner) -> Result<Vec<LinkApp>, LinkFailure> {
        match &self.link {
            Some(client) => client.apps(),
            None => Err(LinkFailure::Failed("Craft Link is not available in this session".into())),
        }
    }

    fn link_call(&self, _owner: &Owner, app: &str, method: &str, params: &Value, cancel: &AtomicBool) -> Result<Value, LinkFailure> {
        match &self.link {
            Some(client) => client.call(app, method, params, cancel),
            None => Err(LinkFailure::Failed("Craft Link is not available in this session".into())),
        }
    }

    fn lend_session(&self, session: &mut dyn std::any::Any, run: &mut dyn FnMut()) {
        self.bridge.lend_any(session, run);
    }

    fn begin_call(&self, _owner: &Owner, _contribution: &str) -> CallScope {
        CallScope(self.bridge.begin())
    }

    fn end_call(&self, scope: CallScope, outcome: &CallOutcome) {
        let faulted = matches!(outcome, CallOutcome::Faulted(_));
        if let Some(why) = self.bridge.end(scope.0, faulted) {
            self.bridge.notify(UserNotice { plugin_id: String::new(), contribution: None, message: why });
        }
    }
}
