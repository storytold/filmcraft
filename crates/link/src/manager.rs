//! `LinkManager`: keeps every link of the open project in step with a running EffectCraft.
//!
//! Two halves.
//!
//! **The thread** ([`LinkManager::start`]) owns one control connection to EffectCraft. Once a
//! second it looks for an instance in discovery. It says `link.hello`, subscribes to
//! `link.changed`, `link.result` and `app.closing`, and `link.attach`es each wanted link. Each
//! attached link gets a [`PeerFrames`] supply. The state of each link follows the machine below.
//! A `link.changed` raises the link's epoch at once (so the next frame asked for is a new one) and
//! sends `link.refresh` to the UI thread through the [`ControlSink`], at most one in flight per link.
//!
//! ```text
//!   (wanted) -> Pending --attach ok--> Live --peer gone / app.closing--> NotLive{Offline}
//!                  |                    ^                                     |
//!                  |                    +---------- peer back, attach ok -----+
//!                  +-- comp gone --> Broken{why}      no result file & no peer: NotLive{NoResult}
//!                                                     (NotLive{Interrupted} when `<result>.part` is there)
//! ```
//!
//! **The installer** ([`pump`], on the UI thread) wraps the media pool source of each Live item in
//! a [`LinkedSource`], puts the result-file source back when the link stops being Live, and shows
//! the offline slate with the reason for `NotLive{NoResult}`.
//!
//! ## Pool replacements
//!
//! Sources are reinstalled after `Change { cause: Load }`: `install_project` replaces the pool
//! and `new_from_template` calls `clear`. They are not reinstalled after `MediaPool::remove`:
//! import relink, project Clear, `remove_unused`, `consolidate_duplicates` and the `edit_subclip`
//! convert delete or replace the item on purpose. `sync_result` replaces the source with a fresh
//! file source and calls [`after_open`],
//! which wraps it again.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use craft_ipc::client::{Client, HelloClient};
use craft_ipc::fetch::FetchOptions;
use craft_ipc::server::ControlSink;
use craft_ipc::wire::{PROTOCOL_MAX, PROTOCOL_MIN};
use craft_ipc::{Discovery, InstanceRecord, LinkError, RuntimeDir};
use filmcraft_engine::Session;
use filmcraft_engine::media_pool::SlateSource;
use filmcraft_engine::project::{ItemId, Project};
use filmcraft_engine::time::FrameRate;
use filmcraft_render::offline::OfflineReason;
use serde_json::{Value, json};

use crate::link_ops::{LINK_KEY, record_of};
use crate::prompt::EFFECTCRAFT_APP;
use crate::source::{FrameSupply, LinkedSource, PeerFrames};

/// How often the thread looks for EffectCraft when it has no connection.
pub const SCAN_EVERY: Duration = Duration::from_secs(1);
/// How long the thread waits for an event before it looks at everything else again.
const SLICE: Duration = Duration::from_millis(100);
/// How long before a link that failed to attach is tried again.
const RETRY: Duration = Duration::from_secs(2);

/// Why a link plays its result file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotLive {
    /// EffectCraft is not running.
    Offline,
    /// There is no EffectCraft binary to start.
    NotInstalled,
    /// The two apps speak different protocol versions.
    VersionMismatch,
    /// No result file yet and no EffectCraft to render frames.
    NoResult,
    /// No result file, but a half-written `.part` of one: EffectCraft stopped in the middle of the render.
    Interrupted,
    Other(String),
}

impl NotLive {
    pub fn label(&self) -> String {
        match self {
            Self::Offline => "EffectCraft is not running".to_string(),
            Self::NotInstalled => "EffectCraft is not installed".to_string(),
            Self::VersionMismatch => "EffectCraft needs an update".to_string(),
            Self::NoResult => "no result rendered yet".to_string(),
            Self::Interrupted => "result render was interrupted".to_string(),
            Self::Other(text) => text.clone(),
        }
    }
}

/// The state of one link. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Pending,
    Live,
    NotLive(NotLive),
    Broken(String),
}

impl LinkState {
    /// The text of the badge.
    pub fn badge(&self) -> String {
        match self {
            Self::Pending => "linked, connecting".to_string(),
            Self::Live => "linked".to_string(),
            Self::NotLive(reason) => format!("linked, not live: {}", reason.label()),
            Self::Broken(why) => format!("link broken: {why}"),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }
}

/// A link the open project has.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub link_id: String,
    pub target_project: String,
    pub target_comp: String,
    pub result_path: String,
    pub rate: FrameRate,
}

/// What the UI side can see of a link.
#[derive(Clone)]
pub struct View {
    pub state: LinkState,
    /// Newest epoch the peer announced. [`LinkedSource`] asks frames at this epoch.
    pub epoch: Arc<AtomicU64>,
    pub supply: Option<Arc<PeerFrames>>,
    /// Changes each time a new supply is attached. The installer compares it.
    pub generation: u64,
}

/// What the manager thread needs from the app.
#[derive(Clone)]
pub struct ManagerConfig {
    pub runtime: Arc<dyn Fn() -> Result<RuntimeDir, LinkError> + Send + Sync>,
    /// The UI thread's request channel. `link.refresh` goes here.
    pub ui: Arc<dyn ControlSink>,
    /// Asks the UI for a frame (a state changed).
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub scan_every: Duration,
    pub fetch: FetchOptions,
}

struct Slot {
    state: LinkState,
    epoch: Arc<AtomicU64>,
    supply: Option<Arc<PeerFrames>>,
    generation: u64,
    retry_at: Option<Instant>,
}

impl Slot {
    fn new() -> Self {
        Self { state: LinkState::Pending, epoch: Arc::new(AtomicU64::new(0)), supply: None, generation: 0, retry_at: None }
    }
}

struct Shared {
    wanted: Mutex<Vec<Wanted>>,
    slots: Mutex<BTreeMap<String, Slot>>,
    version: AtomicU64,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// See the module docs.
pub struct LinkManager {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl LinkManager {
    pub fn start(config: ManagerConfig) -> Arc<Self> {
        let shared =
            Arc::new(Shared { wanted: Mutex::new(Vec::new()), slots: Mutex::new(BTreeMap::new()), version: AtomicU64::new(0), stop: AtomicBool::new(false) });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new().name("link-manager".into()).spawn(move || {
            let wake = Arc::clone(&config.wake);
            let state = Arc::clone(&worker);
            run_guarded(&state, &*wake, move || Runner::new(worker, config).run());
        });
        let handle = match spawned {
            Ok(handle) => Some(handle),
            Err(err) => {
                log::warn!("the link manager could not start: {err}");
                None
            }
        };
        Arc::new(Self { shared, thread: Mutex::new(handle) })
    }

    /// Replace the set of links to keep in step. Links that left are forgotten, new ones attach.
    pub fn set_wanted(&self, wanted: Vec<Wanted>) {
        let mut current = lock(&self.shared.wanted);
        if *current != wanted {
            *current = wanted;
        }
    }

    pub fn view(&self, link_id: &str) -> Option<View> {
        lock(&self.shared.slots).get(link_id).map(|slot| View {
            state: slot.state.clone(),
            epoch: Arc::clone(&slot.epoch),
            supply: slot.supply.clone(),
            generation: slot.generation,
        })
    }

    pub fn state(&self, link_id: &str) -> Option<LinkState> {
        lock(&self.shared.slots).get(link_id).map(|slot| slot.state.clone())
    }

    /// Counts up each time any state or supply changes.
    pub fn version(&self) -> u64 {
        self.shared.version.load(Ordering::SeqCst)
    }

    /// Stop the thread and wait for it.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = lock(&self.thread).take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LinkManager {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------- the thread

/// What the badge says when the thread has stopped for good.
const STOPPED: &str = "the link manager stopped";

/// Run `body`, the loop of the manager thread. A panic ends the loop, and nothing would update the
/// links again. They would show their last state, such as `linked`, forever. So a panic is logged,
/// every link becomes `NotLive` with the supply dropped (the UI puts the result file back), and
/// `wake` asks the UI for a frame to show it.
fn run_guarded(shared: &Shared, wake: &(dyn Fn() + Send + Sync), body: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_ok() {
        return;
    }
    log::error!("{STOPPED} after a panic; its links stay not live");
    for slot in lock(&shared.slots).values_mut() {
        slot.supply = None;
        slot.retry_at = None;
        slot.state = LinkState::NotLive(NotLive::Other(STOPPED.to_string()));
    }
    shared.version.fetch_add(1, Ordering::SeqCst);
    wake();
}

struct Connection {
    record: InstanceRecord,
    client: Client,
}

/// A `link.refresh` the UI thread has not answered yet.
struct InFlight {
    reply: Receiver<Value>,
    /// A newer epoch arrived meanwhile: send once more when the reply comes.
    again: Option<u64>,
}

struct Runner {
    shared: Arc<Shared>,
    config: ManagerConfig,
    connection: Option<Connection>,
    last_scan: Option<Instant>,
    in_flight: BTreeMap<String, InFlight>,
    /// Latest `NotInstalled` look. Looking costs file system calls, so not every second.
    installed: Option<(Instant, bool)>,
    mismatch: bool,
}

impl Runner {
    fn new(shared: Arc<Shared>, config: ManagerConfig) -> Self {
        Self { shared, config, connection: None, last_scan: None, in_flight: BTreeMap::new(), installed: None, mismatch: false }
    }

    fn run(&mut self) {
        while !self.shared.stop.load(Ordering::SeqCst) {
            let wanted = lock(&self.shared.wanted).clone();
            self.forget_unwanted(&wanted);
            if wanted.is_empty() {
                self.connection = None;
                self.sleep();
                continue;
            }
            if self.connection.is_none() {
                self.look_for_peer(&wanted);
            }
            if self.connection.is_some() {
                self.attach_wanted(&wanted);
                self.read_events();
            } else {
                self.mark_offline(&wanted);
                self.sleep();
            }
            self.retry_refreshes();
        }
    }

    fn sleep(&self) {
        std::thread::sleep(SLICE);
    }

    fn changed(&self) {
        self.shared.version.fetch_add(1, Ordering::SeqCst);
        (self.config.wake)();
    }

    fn set_state(&self, link_id: &str, state: LinkState) {
        let mut slots = lock(&self.shared.slots);
        let slot = slots.entry(link_id.to_string()).or_insert_with(Slot::new);
        if slot.state != state {
            slot.state = state;
            drop(slots);
            self.changed();
        }
    }

    fn forget_unwanted(&mut self, wanted: &[Wanted]) {
        let mut slots = lock(&self.shared.slots);
        let before = slots.len();
        slots.retain(|link_id, _| wanted.iter().any(|w| &w.link_id == link_id));
        let removed = slots.len() != before;
        drop(slots);
        self.in_flight.retain(|link_id, _| wanted.iter().any(|w| &w.link_id == link_id));
        if removed {
            self.changed();
        }
    }

    // ---- finding EffectCraft

    fn look_for_peer(&mut self, wanted: &[Wanted]) {
        if self.last_scan.is_some_and(|at| at.elapsed() < self.config.scan_every) {
            self.sleep();
            return;
        }
        self.last_scan = Some(Instant::now());
        self.mismatch = false;
        let Ok(dir) = (self.config.runtime)() else { return };
        let Ok(records) = Discovery::scan(&dir) else { return };
        let Some(record) = pick(&records, wanted) else { return };
        match greet(&record) {
            Ok(client) => self.connection = Some(Connection { record, client }),
            Err(LinkError::Protocol { .. }) => self.mismatch = true,
            Err(err) => log::debug!("link manager: EffectCraft pid {} did not answer: {err}", record.pid),
        }
    }

    /// The reason to show while there is no connection.
    fn offline_reason(&mut self, want: &Wanted) -> NotLive {
        if self.mismatch {
            return NotLive::VersionMismatch;
        }
        if !Path::new(&want.result_path).exists() {
            // EffectCraft renders to `<result>.part` and renames it when done. A part file without
            // the result is a render that was cut off. It is never the result.
            let part = format!("{}.part", want.result_path);
            return if Path::new(&part).exists() { NotLive::Interrupted } else { NotLive::NoResult };
        }
        let looked = self.installed.is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(30));
        if !looked {
            self.installed = Some((Instant::now(), !matches!(craft_ipc::launch::resolve(EFFECTCRAFT_APP), Err(LinkError::NotInstalled { .. }))));
        }
        if self.installed.is_some_and(|(_, found)| !found) { NotLive::NotInstalled } else { NotLive::Offline }
    }

    fn mark_offline(&mut self, wanted: &[Wanted]) {
        let mut dropped = false;
        for want in wanted {
            let reason = self.offline_reason(want);
            let mut slots = lock(&self.shared.slots);
            let slot = slots.entry(want.link_id.clone()).or_insert_with(Slot::new);
            if slot.supply.take().is_some() {
                slot.generation += 1;
                dropped = true;
            }
            let state = LinkState::NotLive(reason);
            if slot.state != state {
                slot.state = state;
                dropped = true;
            }
        }
        if dropped {
            self.changed();
        }
    }

    // ---- attaching

    fn attach_wanted(&mut self, wanted: &[Wanted]) {
        for want in wanted {
            let due = {
                let slots = lock(&self.shared.slots);
                match slots.get(&want.link_id) {
                    Some(slot) => slot.supply.is_none() && slot.retry_at.is_none_or(|at| Instant::now() >= at) && !matches!(slot.state, LinkState::Broken(_)),
                    None => true,
                }
            };
            if due {
                self.attach(want);
            }
            if self.connection.is_none() {
                return;
            }
        }
    }

    fn attach(&mut self, want: &Wanted) {
        let Some(conn) = self.connection.as_mut() else { return };
        let params = json!({"link_id": want.link_id, "project": want.target_project, "comp": want.target_comp});
        let attached = conn.client.call("link.attach", params).and_then(|reply| {
            let epoch = reply.get("epoch").and_then(Value::as_u64).unwrap_or(0);
            PeerFrames::connect(&conn.record, &want.link_id, self.config.fetch.clone()).map(|supply| (epoch, supply))
        });
        match attached {
            Ok((epoch, supply)) => {
                let mut slots = lock(&self.shared.slots);
                let slot = slots.entry(want.link_id.clone()).or_insert_with(Slot::new);
                slot.epoch.fetch_max(epoch, Ordering::SeqCst);
                slot.supply = Some(Arc::new(supply));
                slot.generation += 1;
                slot.state = LinkState::Live;
                slot.retry_at = None;
                drop(slots);
                self.changed();
            }
            Err(LinkError::Closed | LinkError::Io(_) | LinkError::Timeout) => self.connection = None,
            Err(LinkError::Protocol { .. }) => {
                self.mismatch = true;
                self.connection = None;
            }
            Err(LinkError::Busy(text)) => self.retry_later(&want.link_id, LinkState::NotLive(NotLive::Other(text))),
            Err(err) => {
                let why = err.to_string();
                let mut slots = lock(&self.shared.slots);
                let slot = slots.entry(want.link_id.clone()).or_insert_with(Slot::new);
                slot.supply = None;
                drop(slots);
                self.set_state(&want.link_id, LinkState::Broken(why));
            }
        }
    }

    fn retry_later(&self, link_id: &str, state: LinkState) {
        if let Some(slot) = lock(&self.shared.slots).get_mut(link_id) {
            slot.retry_at = Some(Instant::now() + RETRY);
        }
        self.set_state(link_id, state);
    }

    // ---- events

    fn read_events(&mut self) {
        let Some(conn) = self.connection.as_mut() else { return };
        let mut changed: BTreeMap<String, u64> = BTreeMap::new();
        let mut wait = SLICE;
        loop {
            match conn.client.next_event(Some(wait)) {
                Ok(Some(event)) => {
                    wait = Duration::from_millis(1);
                    match event.event.as_str() {
                        "link.changed" | "link.result" => {
                            if let (Some(link_id), Some(epoch)) =
                                (event.data.get("link_id").and_then(Value::as_str), event.data.get("epoch").and_then(Value::as_u64))
                            {
                                let newest = changed.entry(link_id.to_string()).or_insert(0);
                                *newest = (*newest).max(epoch);
                            }
                        }
                        "app.closing" => {
                            self.connection = None;
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    self.connection = None;
                    break;
                }
            }
        }
        for (link_id, epoch) in changed {
            self.refresh(&link_id, epoch);
        }
    }

    /// Raise the epoch now, and ask the UI thread to refresh. One request per link at a time.
    fn refresh(&mut self, link_id: &str, epoch: u64) {
        if let Some(slot) = lock(&self.shared.slots).get(link_id) {
            slot.epoch.fetch_max(epoch, Ordering::SeqCst);
        }
        if let Some(pending) = self.in_flight.get_mut(link_id) {
            pending.again = Some(pending.again.map_or(epoch, |old| old.max(epoch)));
            return;
        }
        self.send_refresh(link_id, epoch);
    }

    fn send_refresh(&mut self, link_id: &str, epoch: u64) {
        if let Some(reply) = self.config.ui.submit("link.refresh".to_string(), json!({"link_id": link_id, "epoch": epoch})) {
            self.in_flight.insert(link_id.to_string(), InFlight { reply, again: None });
        }
    }

    /// A refresh that finished lets the next one go.
    fn retry_refreshes(&mut self) {
        let mut ready = Vec::new();
        for (link_id, pending) in &self.in_flight {
            match pending.reply.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(_) | Err(TryRecvError::Disconnected) => ready.push((link_id.clone(), pending.again)),
            }
        }
        for (link_id, again) in ready {
            self.in_flight.remove(&link_id);
            if let Some(epoch) = again {
                self.send_refresh(&link_id, epoch);
            }
        }
    }
}

/// The instance to talk to: the one that holds a wanted link's project, else the only one.
fn pick(records: &[InstanceRecord], wanted: &[Wanted]) -> Option<InstanceRecord> {
    let running: Vec<&InstanceRecord> = records.iter().filter(|record| record.app == EFFECTCRAFT_APP).collect();
    let holder = running.iter().find(|record| wanted.iter().any(|w| record.docs.iter().any(|doc| doc.path.as_deref() == Some(w.target_project.as_str()))));
    match (holder, running.as_slice()) {
        (Some(record), _) => Some((**record).clone()),
        (None, [only]) => Some((*only).clone()),
        _ => None,
    }
}

/// Connect, say hello, and subscribe.
fn greet(record: &InstanceRecord) -> Result<Client, LinkError> {
    let mut client = Client::connect(record)?;
    client.hello(&HelloClient {
        app: "filmcraft".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        instance: craft_ipc::random_uuid()?,
        protocol_min: PROTOCOL_MIN,
        protocol_max: PROTOCOL_MAX,
    })?;
    client.subscribe(&["link.changed", "link.result", "app.closing"])?;
    Ok(client)
}

// ---------------------------------------------------------------- the process manager

static MANAGER: Mutex<Option<Arc<LinkManager>>> = Mutex::new(None);
static INSTALLER: Mutex<Installer> = Mutex::new(Installer::new());

/// Start the process manager. A second call replaces the first.
pub fn install(config: ManagerConfig) -> Arc<LinkManager> {
    let manager = LinkManager::start(config);
    let previous = lock(&MANAGER).replace(Arc::clone(&manager));
    lock(&INSTALLER).reset();
    drop(previous);
    manager
}

/// Stop the process manager and forget the installer's state. The next [`pump`] does nothing
/// until [`install`] runs again.
pub fn uninstall() {
    let previous = lock(&MANAGER).take();
    lock(&INSTALLER).reset();
    drop(previous);
}

/// Tests share one manager and one installer per process, and [`pump`] serves any session that
/// asks. A pump from another test's session while the installer test runs sets the wanted links to
/// that session's (none), and the manager forgets the link. A test that installs the manager, or
/// pumps a session while one may be installed, holds this guard to the end.
#[cfg(test)]
pub(crate) fn exclusive() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    lock(&GUARD)
}

/// The process manager, if [`install`] ran.
pub fn manager() -> Option<Arc<LinkManager>> {
    lock(&MANAGER).clone()
}

/// Once per frame on the UI thread. See the module docs.
pub fn pump(session: &mut Session) {
    let Some(manager) = manager() else { return };
    lock(&INSTALLER).pump(&manager, session);
}

/// `sync_result` opened the result file of `media` and put it in the pool. If the link is Live,
/// wrap the new source again.
pub fn after_open(session: &mut Session, media: ItemId) {
    let Some(manager) = manager() else { return };
    lock(&INSTALLER).after_open(&manager, session, media);
}

struct Installed {
    link_id: String,
    kind: Kind,
}

enum Kind {
    Live { generation: u64, source: Arc<LinkedSource> },
    Slate { reason: String },
}

/// The UI-thread half. See the module docs.
pub struct Installer {
    installed: BTreeMap<ItemId, Installed>,
    badges: BTreeMap<ItemId, String>,
    links: Vec<(ItemId, crate::record::LinkRecord)>,
    scanned: Option<u64>,
    seen_version: u64,
    bus: Option<(Arc<craft_command::ChangeBus<Arc<Project>>>, u64)>,
}

impl Installer {
    pub const fn new() -> Self {
        Self { installed: BTreeMap::new(), badges: BTreeMap::new(), links: Vec::new(), scanned: None, seen_version: 0, bus: None }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn pump(&mut self, manager: &LinkManager, session: &mut Session) {
        self.look_at_project(manager, session);
        if self.links.is_empty() {
            if !session.link_badges.is_empty() {
                session.link_badges.clear();
                session.link_frames.clear();
            }
            return;
        }
        let version = manager.version();
        let moved = version != self.seen_version;
        self.seen_version = version;
        let links = self.links.clone();
        for (item, record) in &links {
            self.ensure(manager, session, *item, record, moved);
        }
        if session.link_badges != self.badges {
            session.link_badges = self.badges.clone();
        }
        let frames: BTreeMap<ItemId, (u64, u64, u64)> = self
            .installed
            .iter()
            .filter_map(|(item, installed)| match &installed.kind {
                Kind::Live { source, .. } => Some((*item, source.counters().snapshot())),
                Kind::Slate { .. } => None,
            })
            .collect();
        if session.link_frames != frames {
            session.link_frames = frames;
        }
    }

    /// Find the linked items when the project changed, drain the change feed, and tell the thread.
    fn look_at_project(&mut self, manager: &LinkManager, session: &Session) {
        if self.scanned != Some(session.revision) {
            self.scanned = Some(session.revision);
            self.links = linked_items(&session.project);
            manager.set_wanted(self.links.iter().map(|(_, record)| wanted_of(record)).collect());
            self.badges.retain(|item, _| self.links.iter().any(|(id, _)| id == item));
            self.installed.retain(|item, _| self.links.iter().any(|(id, _)| id == item));
        }
        if self.links.is_empty() {
            if let Some((bus, id)) = self.bus.take() {
                bus.unsubscribe(id);
            }
            return;
        }
        let (bus, id) = self.bus.get_or_insert_with(|| {
            let bus = session.hooks.change_bus::<Arc<Project>>();
            let id = bus.subscribe();
            (bus, id)
        });
        // The pool was replaced: nothing installed is in it any more.
        if bus.drain(*id).iter().any(|change| change.cause == craft_command::Cause::Load) {
            self.installed.clear();
        }
    }

    fn ensure(&mut self, manager: &LinkManager, session: &mut Session, item: ItemId, record: &crate::record::LinkRecord, moved: bool) {
        let Some(view) = manager.view(&record.link_id) else {
            self.badges.insert(item, LinkState::Pending.badge());
            return;
        };
        self.badges.insert(item, view.state.badge());
        match (&view.state, view.supply.as_ref()) {
            (LinkState::Live, Some(supply)) => {
                let current =
                    matches!(self.installed.get(&item), Some(Installed { kind: Kind::Live { generation, .. }, .. }) if *generation == view.generation);
                if !current {
                    self.install_live(session, item, record, &view, Arc::clone(supply));
                }
            }
            (LinkState::NotLive(reason @ (NotLive::NoResult | NotLive::Interrupted)), _) => self.install_slate(session, item, record, &reason.label()),
            _ => {
                if moved || self.installed.contains_key(&item) {
                    self.uninstall(session, item);
                }
            }
        }
    }

    fn install_live(&mut self, session: &mut Session, item: ItemId, record: &crate::record::LinkRecord, view: &View, supply: Arc<PeerFrames>) {
        let Some(fallback) = self.fallback_of(session, item) else { return };
        let rate = FrameRate::new(i64::try_from(record.format.rate_num).unwrap_or(24), i64::try_from(record.format.rate_den).unwrap_or(1));
        let source = Arc::new(LinkedSource::new(&record.link_id, fallback, supply as Arc<dyn FrameSupply>, Arc::clone(&view.epoch), rate));
        session.media.insert(item, Arc::clone(&source) as filmcraft_media::SharedSource);
        self.installed.insert(item, Installed { link_id: record.link_id.clone(), kind: Kind::Live { generation: view.generation, source } });
        session.bump_view();
    }

    /// What plays when the live frame is late: the source the pool holds, or the one under our wrapper.
    fn fallback_of(&self, session: &Session, item: ItemId) -> Option<filmcraft_media::SharedSource> {
        let cached = session.media.cached(item);
        if let (Some(Installed { kind: Kind::Live { source, .. }, .. }), Some(cached)) = (self.installed.get(&item), cached.as_ref())
            && std::ptr::addr_eq(Arc::as_ptr(cached), Arc::as_ptr(source))
        {
            return Some(Arc::clone(source.fallback()));
        }
        match cached {
            Some(found) => Some(found),
            None => session.media.source_for(&session.project, item, &*session.services),
        }
    }

    fn install_slate(&mut self, session: &mut Session, item: ItemId, record: &crate::record::LinkRecord, reason: &str) {
        let same = matches!(self.installed.get(&item), Some(Installed { kind: Kind::Slate { reason: have }, .. }) if have == reason);
        if same {
            return;
        }
        let Some(clip) = session.project.item(item).and_then(|entry| entry.as_media()) else { return };
        if !clip.offline {
            // The result file is there; the pool plays it.
            return;
        }
        let slate = SlateSource::new(clip.info.clone(), reason, OfflineReason::Missing);
        session.media.insert(item, Arc::new(slate));
        self.installed.insert(item, Installed { link_id: record.link_id.clone(), kind: Kind::Slate { reason: reason.to_string() } });
        session.bump_view();
    }

    /// Give the item back to the pool, which opens the result file (or the offline slate) again.
    fn uninstall(&mut self, session: &mut Session, item: ItemId) {
        if self.installed.remove(&item).is_some() {
            session.media.remove(item);
            session.bump_view();
        }
    }

    pub fn after_open(&mut self, manager: &LinkManager, session: &mut Session, media: ItemId) {
        // The pool holds the fresh file source now, whatever we had installed is gone.
        let Some(previous) = self.installed.remove(&media) else { return };
        let Some(view) = manager.view(&previous.link_id) else { return };
        let Some(supply) = view.supply.clone() else { return };
        if !view.state.is_live() {
            return;
        }
        let Ok(Some(record)) = record_of(&session.project, media) else { return };
        self.install_live(session, media, &record, &view, supply);
    }
}

impl Default for Installer {
    fn default() -> Self {
        Self::new()
    }
}

/// Every media item that carries a valid link record.
pub fn linked_items(project: &Project) -> Vec<(ItemId, crate::record::LinkRecord)> {
    project
        .items
        .iter()
        .filter(|(_, item)| item.metadata.contains_key(LINK_KEY))
        .filter_map(|(id, _)| record_of(project, *id).ok().flatten().map(|record| (*id, record)))
        .collect()
}

fn wanted_of(record: &crate::record::LinkRecord) -> Wanted {
    Wanted {
        link_id: record.link_id.clone(),
        target_project: record.target_project.clone(),
        target_comp: record.target_comp.clone(),
        result_path: record.result_path.clone(),
        rate: FrameRate::new(i64::try_from(record.format.rate_num).unwrap_or(24), i64::try_from(record.format.rate_den).unwrap_or(1)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc::{Sender, channel};

    use craft_ipc::bulk::{FrameLayout, FrameRequest, FrameSource, FrameSpec, FrameTickets, RenderedFrame};
    use craft_ipc::server::{Activate, Activation, Frames, Server, ServerConfig};
    use filmcraft_media::FrameRequest as MediaFrameRequest;

    use super::*;
    use crate::fixtures::{TempProject, linked_offline};

    /// 2x2 frames: premultiplied red 90, opaque, with the epoch the fake EffectCraft is at.
    struct Pixels {
        epoch: AtomicU64,
    }

    impl FrameSource for Pixels {
        fn open(&self, _spec: &FrameSpec) -> Result<FrameLayout, LinkError> {
            Ok(FrameLayout { w: 2, h: 2, stride: 8, len: 16 })
        }
        fn render(&self, spec: &FrameSpec, request: &FrameRequest) -> Result<RenderedFrame, LinkError> {
            Ok(RenderedFrame {
                epoch: self.epoch.load(Ordering::SeqCst).max(request.epoch),
                w: 2,
                h: 2,
                stride: 8,
                format: spec.format,
                render_ms: 1.0,
                pixels: [90, 0, 0, 255].repeat(4),
            })
        }
        fn close(&self, _spec: &FrameSpec) {}
    }

    type Calls = Arc<Mutex<Vec<(String, Value)>>>;

    /// EffectCraft's link methods, as far as the manager uses them.
    struct EcSink {
        calls: Calls,
        gone_comp: Option<&'static str>,
    }

    impl ControlSink for EcSink {
        fn submit(&self, method: String, params: Value) -> Option<Receiver<Value>> {
            lock(&self.calls).push((method.clone(), params.clone()));
            let (tx, rx) = channel();
            let reply = match method.as_str() {
                "link.attach" if self.gone_comp.is_some_and(|gone| params["comp"] == json!(gone)) => {
                    json!({"ok": false, "error": {"code": "comp_missing", "message": "comp_missing: no such comp", "data": {}}})
                }
                "link.attach" => json!({"ok": true, "result": {"format": {"w": 2, "h": 2}, "epoch": 2}}),
                _ => json!({"ok": false, "error": "unexpected"}),
            };
            tx.send(reply).ok()?;
            Some(rx)
        }
    }

    struct NoActivate;

    impl Activate for NoActivate {
        fn activate(&self, _t: Option<String>, _p: Option<u32>, _o: Option<Value>) -> Activation {
            Activation::No
        }
    }

    struct FakeEc {
        server: Option<Server>,
        runtime: RuntimeDir,
        dir: PathBuf,
        calls: Calls,
        gone_comp: Option<&'static str>,
    }

    impl FakeEc {
        fn start(name: &str, gone_comp: Option<&'static str>) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = PathBuf::from(format!("/tmp/fc-mgr-{name}-{:x}", (nanos as u64) ^ u64::from(std::process::id())));
            let _ = std::fs::remove_dir_all(&dir);
            let runtime = RuntimeDir::at(&dir).unwrap();
            let mut ec = Self { server: None, runtime, dir, calls: Arc::new(Mutex::new(Vec::new())), gone_comp };
            ec.up();
            ec
        }

        fn up(&mut self) {
            let tickets = Arc::new(FrameTickets::new(Arc::new(Pixels { epoch: AtomicU64::new(2) })));
            let server = Server::start(ServerConfig {
                app: "effectcraft".into(),
                app_version: "0.0.0-test".into(),
                instance: None,
                runtime: Some(self.runtime.clone()),
                tcp_port: None,
                sink: Arc::new(EcSink { calls: Arc::clone(&self.calls), gone_comp: self.gone_comp }),
                activate: Arc::new(NoActivate),
                frames: tickets as Arc<dyn Frames>,
            })
            .unwrap();
            self.server = Some(server);
        }

        fn down(&mut self) {
            if let Some(server) = self.server.take() {
                server.shutdown().unwrap();
            }
        }

        fn emit(&self, event: &str, data: Value) {
            self.server.as_ref().unwrap().emit(event, data).unwrap();
        }

        fn attaches(&self) -> usize {
            lock(&self.calls).iter().filter(|(method, _)| method == "link.attach").count()
        }
    }

    impl Drop for FakeEc {
        fn drop(&mut self) {
            self.down();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The UI thread: records `link.refresh`, answers when the test lets it.
    struct FakeUi {
        seen: Calls,
        replies: Mutex<Vec<Sender<Value>>>,
        answer_now: AtomicBool,
    }

    impl FakeUi {
        fn new(answer_now: bool) -> Arc<Self> {
            Arc::new(Self { seen: Arc::new(Mutex::new(Vec::new())), replies: Mutex::new(Vec::new()), answer_now: AtomicBool::new(answer_now) })
        }

        fn refreshes(&self) -> Vec<Value> {
            lock(&self.seen).iter().filter(|(method, _)| method == "link.refresh").map(|(_, params)| params.clone()).collect()
        }

        fn answer_all(&self) {
            for tx in lock(&self.replies).drain(..) {
                let _ = tx.send(json!({"ok": true, "result": {}}));
            }
        }
    }

    impl ControlSink for FakeUi {
        fn submit(&self, method: String, params: Value) -> Option<Receiver<Value>> {
            lock(&self.seen).push((method, params));
            let (tx, rx) = channel();
            if self.answer_now.load(Ordering::SeqCst) {
                tx.send(json!({"ok": true, "result": {}})).ok()?;
            } else {
                lock(&self.replies).push(tx);
            }
            Some(rx)
        }
    }

    fn wanted(link_id: &str, comp: &str, result: &str) -> Wanted {
        Wanted { link_id: link_id.into(), target_project: "/p.ecproj".into(), target_comp: comp.into(), result_path: result.into(), rate: FrameRate::FPS_24 }
    }

    fn config(ec: &FakeEc, ui: Arc<FakeUi>) -> ManagerConfig {
        let runtime = ec.runtime.clone();
        ManagerConfig {
            runtime: Arc::new(move || Ok(runtime.clone())),
            ui,
            wake: Arc::new(|| {}),
            scan_every: Duration::from_millis(50),
            fetch: FetchOptions { connections: 1, io_timeout: Duration::from_secs(2), ..FetchOptions::default() },
        }
    }

    fn wait_state(manager: &LinkManager, link_id: &str, what: &str, check: impl Fn(&LinkState) -> bool) -> LinkState {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(state) = manager.state(link_id).filter(|state| check(state)) {
                return state;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; the state is {:?}", manager.state(link_id));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn scratch_file(name: &str) -> String {
        let path = format!("/tmp/fc-mgr-result-{}-{name}.mov", std::process::id());
        std::fs::write(&path, b"x").unwrap();
        path
    }

    #[test]
    fn a_panic_in_the_thread_leaves_every_link_not_live_and_wakes_the_ui() {
        let shared = Shared { wanted: Mutex::new(Vec::new()), slots: Mutex::new(BTreeMap::new()), version: AtomicU64::new(0), stop: AtomicBool::new(false) };
        for (link, state) in [("live", LinkState::Live), ("pending", LinkState::Pending)] {
            let mut slot = Slot::new();
            slot.state = state;
            lock(&shared.slots).insert(link.to_string(), slot);
        }
        let woke = std::sync::atomic::AtomicUsize::new(0);
        let wake = || {
            woke.fetch_add(1, Ordering::SeqCst);
        };

        // A loop that ends on its own changes nothing.
        run_guarded(&shared, &wake, || {});
        assert_eq!(lock(&shared.slots).get("live").unwrap().state, LinkState::Live);
        assert_eq!((shared.version.load(Ordering::SeqCst), woke.load(Ordering::SeqCst)), (0, 0));

        run_guarded(&shared, &wake, || panic!("the loop failed"));
        for link in ["live", "pending"] {
            let state = lock(&shared.slots).get(link).unwrap().state.clone();
            assert_eq!(state, LinkState::NotLive(NotLive::Other(STOPPED.to_string())), "{link}");
            assert_eq!(state.badge(), "linked, not live: the link manager stopped");
        }
        assert_eq!(shared.version.load(Ordering::SeqCst), 1, "the installer sees a change");
        assert_eq!(woke.load(Ordering::SeqCst), 1, "the UI is asked for a frame");
    }

    #[test]
    fn a_wanted_link_attaches_and_serves_frames() {
        let ec = FakeEc::start("attach", None);
        let manager = LinkManager::start(config(&ec, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "c1", "/nope.mov")]);
        wait_state(&manager, "link-a", "Live", LinkState::is_live);
        let attach = lock(&ec.calls).iter().find(|(method, _)| method == "link.attach").cloned().unwrap().1;
        assert_eq!(attach, json!({"link_id": "link-a", "project": "/p.ecproj", "comp": "c1"}));
        let view = manager.view("link-a").unwrap();
        assert_eq!(view.epoch.load(Ordering::SeqCst), 2, "the epoch of the attach reply");
        let supply = view.supply.unwrap();
        let live = supply.frame(1.0, 3, 0.125, 2, Duration::from_secs(5));
        assert!(matches!(live, crate::source::Supplied::Frame(_)), "{live:?}");
        assert_eq!(LinkState::Live.badge(), "linked");
    }

    #[test]
    fn link_changed_raises_the_epoch_and_asks_the_ui_for_one_refresh_at_a_time() {
        let ec = FakeEc::start("changed", None);
        let ui = FakeUi::new(false);
        let manager = LinkManager::start(config(&ec, Arc::clone(&ui)));
        manager.set_wanted(vec![wanted("link-a", "c1", "/nope.mov")]);
        wait_state(&manager, "link-a", "Live", LinkState::is_live);
        let epoch = manager.view("link-a").unwrap().epoch;
        ec.emit("link.changed", json!({"link_id": "link-a", "epoch": 5}));
        let deadline = Instant::now() + Duration::from_secs(5);
        while ui.refreshes().is_empty() {
            assert!(Instant::now() < deadline, "no link.refresh reached the UI");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ui.refreshes(), vec![json!({"link_id": "link-a", "epoch": 5})]);
        assert_eq!(epoch.load(Ordering::SeqCst), 5, "the frame source asks for the new epoch before the UI answers");
        // Two more while the first is unanswered: no new request yet.
        ec.emit("link.changed", json!({"link_id": "link-a", "epoch": 6}));
        ec.emit("link.result", json!({"link_id": "link-a", "epoch": 7, "path": "/x.mov"}));
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(ui.refreshes().len(), 1);
        assert_eq!(epoch.load(Ordering::SeqCst), 7);
        ui.answer_all();
        let deadline = Instant::now() + Duration::from_secs(5);
        while ui.refreshes().len() < 2 {
            assert!(Instant::now() < deadline, "the held back refresh never went out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ui.refreshes()[1], json!({"link_id": "link-a", "epoch": 7}), "one refresh with the newest epoch");
    }

    #[test]
    fn a_peer_that_goes_away_makes_the_link_not_live_and_it_comes_back() {
        let mut ec = FakeEc::start("away", None);
        let result = scratch_file("away");
        let manager = LinkManager::start(config(&ec, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "c1", &result)]);
        wait_state(&manager, "link-a", "Live", LinkState::is_live);
        let first = manager.view("link-a").unwrap().generation;
        ec.down();
        let state = wait_state(&manager, "link-a", "NotLive", |state| matches!(state, LinkState::NotLive(_)));
        assert_eq!(state, LinkState::NotLive(NotLive::Offline));
        assert_eq!(state.badge(), "linked, not live: EffectCraft is not running");
        assert!(manager.view("link-a").unwrap().supply.is_none());
        ec.up();
        wait_state(&manager, "link-a", "Live again", LinkState::is_live);
        assert!(manager.view("link-a").unwrap().generation > first, "a new supply");
        assert_eq!(ec.attaches(), 2);
        let _ = std::fs::remove_file(result);
    }

    #[test]
    fn app_closing_ends_live_and_a_missing_result_with_no_peer_is_no_result() {
        let mut ec = FakeEc::start("closing", None);
        let manager = LinkManager::start(config(&ec, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "c1", "/no/such/result.mov")]);
        wait_state(&manager, "link-a", "Live", LinkState::is_live);
        ec.emit("app.closing", json!({}));
        // The app is still there, so the manager attaches again at once; take the server away to see the reason.
        ec.down();
        let state = wait_state(&manager, "link-a", "NotLive", |state| matches!(state, LinkState::NotLive(_)));
        assert_eq!(state, LinkState::NotLive(NotLive::NoResult));
        assert_eq!(state.badge(), "linked, not live: no result rendered yet");
    }

    #[test]
    fn a_part_file_alone_is_not_a_result_and_says_the_render_was_interrupted() {
        let dir = std::env::temp_dir().join(format!("fc-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let result = dir.join("link-a.mov");
        std::fs::write(dir.join("link-a.mov.part"), b"half a movie").unwrap();
        let mut gone = FakeEc::start("part-gone", None);
        gone.down();
        let manager = LinkManager::start(config(&gone, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "c1", result.to_str().unwrap())]);
        let state = wait_state(&manager, "link-a", "NotLive", |state| matches!(state, LinkState::NotLive(_)));
        assert_eq!(state, LinkState::NotLive(NotLive::Interrupted));
        assert_eq!(state.badge(), "linked, not live: result render was interrupted");
        // Once the movie exists the part file no longer matters.
        std::fs::write(&result, b"x").unwrap();
        let state = wait_state(&manager, "link-a", "Offline", |state| matches!(state, LinkState::NotLive(NotLive::Offline | NotLive::NotInstalled)));
        assert!(matches!(state, LinkState::NotLive(NotLive::Offline | NotLive::NotInstalled)), "{state:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_comp_that_is_gone_breaks_the_link() {
        let ec = FakeEc::start("broken", Some("gone"));
        let manager = LinkManager::start(config(&ec, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "gone", "/nope.mov"), wanted("link-b", "c2", "/nope2.mov")]);
        let broken = wait_state(&manager, "link-a", "Broken", |state| matches!(state, LinkState::Broken(_)));
        assert!(broken.badge().starts_with("link broken: "), "{}", broken.badge());
        wait_state(&manager, "link-b", "the other link Live", LinkState::is_live);
        assert_eq!(ec.attaches(), 2, "a broken link is not attached again");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(ec.attaches(), 2);
    }

    #[test]
    fn a_link_that_leaves_the_project_is_forgotten() {
        let ec = FakeEc::start("forget", None);
        let manager = LinkManager::start(config(&ec, FakeUi::new(true)));
        manager.set_wanted(vec![wanted("link-a", "c1", "/nope.mov")]);
        wait_state(&manager, "link-a", "Live", LinkState::is_live);
        manager.set_wanted(Vec::new());
        let deadline = Instant::now() + Duration::from_secs(5);
        while manager.view("link-a").is_some() {
            assert!(Instant::now() < deadline, "the link was never forgotten");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The installer shares process globals with nothing else in this module, and runs as one flow.
    #[test]
    fn the_installer_puts_a_linked_source_in_the_pool_and_takes_it_out() {
        const LINK: &str = "11111111-1111-4111-8111-111111111111";
        let _exclusive = exclusive();
        let mut ec = FakeEc::start("install", None);
        let project = TempProject::new("mgr");
        std::fs::create_dir_all(std::path::Path::new(&project.path).parent().unwrap()).unwrap();
        let result = format!("{}.result.mov", project.path);
        let mut session = filmcraft_engine::Session::default();
        let item = linked_offline(&mut session, LINK, &result);
        // The link record of the fixture points at project "/tmp/ec.ecproj", comp "c".
        let manager = install(config(&ec, FakeUi::new(true)));
        let live_pixel = |session: &filmcraft_engine::Session| -> [u8; 4] {
            let source = session.media.source_for(&session.project, item, &*session.services).unwrap();
            let frame = source.video_frame(MediaFrameRequest::full(filmcraft_engine::time::Tick::ZERO)).unwrap();
            match &frame.data {
                filmcraft_frame::PixelData::Rgba8(data) => [data[0], data[1], data[2], data[3]],
                other => panic!("expected Rgba8, got {other:?}"),
            }
        };
        let pump_until = |session: &mut filmcraft_engine::Session, what: &str, done: &dyn Fn(&filmcraft_engine::Session) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done(session) {
                pump(session);
                assert!(Instant::now() < deadline, "timed out waiting for {what}; badge {:?}", session.link_badges.get(&item));
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        pump_until(&mut session, "the live source", &|s| s.link_badges.get(&item).is_some_and(|b| b == "linked") && live_pixel(s) == [90, 0, 0, 255]);
        assert_eq!(session.link_badges.get(&item).map(String::as_str), Some("linked"));

        // The peer goes away: the badge says why and the pool plays the offline slate or the file.
        ec.down();
        pump_until(&mut session, "not live", &|s| s.link_badges.get(&item).is_some_and(|b| b.starts_with("linked, not live")));
        assert_ne!(live_pixel(&session), [90, 0, 0, 255], "the live frame is gone");

        // The peer returns: live again.
        ec.up();
        pump_until(&mut session, "live again", &|s| s.link_badges.get(&item).is_some_and(|b| b == "linked") && live_pixel(s) == [90, 0, 0, 255]);

        // A load replaces the pool: the source is installed again.
        session.execute("file.saveAs", json!({"path": project.path})).unwrap();
        session.execute("file.open", json!({"path": project.path})).unwrap();
        assert_ne!(live_pixel(&session), [90, 0, 0, 255], "the new pool holds no linked source yet");
        pump_until(&mut session, "the source after a load", &|s| live_pixel(s) == [90, 0, 0, 255]);

        // The last link leaves: the badge goes.
        manager.set_wanted(Vec::new());
        session.project_mut().item_mut(item).unwrap().metadata.remove(crate::link_ops::LINK_KEY);
        pump(&mut session);
        assert!(session.link_badges.is_empty());
        manager.stop();
        uninstall();
        assert!(self::manager().is_none(), "uninstall leaves no process manager for the next test to pump");
        let _ = std::fs::remove_file(&result);
    }
}
