//! How a host call of a guest reaches the [`Session`].
//!
//! The plugin host calls [`craft_plugin_host::services::Services`] through `&self`, and a
//! `DynCommand` body only gets `&mut Session` for the length of its own call. The guest runs inside
//! that call, so a host call such as `cmd.execute` has to reach the same session while the command
//! body still holds the `&mut`. Safe Rust has one way to do that: move the session.
//!
//! The plugin host does it around the guest call only, through `Services::lend_session`, so the
//! command body and the `before` gate keep the real session for everything else.
//!
//! ```text
//! Session::execute("plugin.x.go")            (a before gate lends the same way)
//!   command body with the real session
//!     Services::lend_session -> Bridge::lend_any
//!        swap(slot, spare)         the slot now holds an idle spare, the real session sits in a box
//!        push box on the live stack
//!        run the guest call        generation.call -> guest -> host call -> Services
//!                                    -> Bridge::with_session pops the box, works on it, pushes it back
//!        pop box, swap back        also on unwind (Drop)
//! ```
//!
//! Three situations reach [`Bridge::with_session`].
//!
//! | Caller | Access |
//! |---|---|
//! | the session thread inside a guest call (command body, `before` gate, panel event) | the live session, at once |
//! | the session thread outside a guest call | none. The session is borrowed above this call. The call fails with [`Unavailable::Busy`]. |
//! | any other thread (`after` and `change` hooks, batch jobs) | a job in the mailbox. [`Bridge::pump`] runs it on the session thread, once per frame. The caller waits up to `wait`. |
//!
//! `begin` opens a scope. The first command a scope runs takes a [`Checkpoint`] of the
//! document just before it runs, because a guest can change the document only through `cmd.execute`.
//! `end` with a fault puts the checkpoint back. What a checkpoint covers: the project, the editor
//! state, the undo and redo stacks, the command journal, the path the document saves to and the
//! media pool with the document of each stored media path. What it cannot cover: files a command
//! wrote and jobs it started.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::Duration;

use craft_command::Cause;
use craft_plugin_host::services::{CommandFailure, UserNotice};
use filmcraft_engine::media_pool::PoolSnapshot;
use filmcraft_engine::project::Project;
use filmcraft_engine::{EditorState, FsServices, MediaPool, Session};
use serde_json::Value;

/// How many commands may nest through plugins before a call is refused.
pub const MAX_NESTING: u32 = 16;

/// Why a host call could not reach the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Called on the session thread outside a guest call.
    Busy,
    /// Called on another thread and the session thread did not pump in time.
    TimedOut,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::Busy => f.write_str("the document is busy: this call cannot run outside a plug-in call"),
            Unavailable::TimedOut => f.write_str("the editor did not answer in time"),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One undo or redo step: label, project, and the id of the command that made it.
type Step = (String, Arc<Project>, Option<String>);

/// The document as a scope found it.
pub struct Checkpoint {
    project: Arc<Project>,
    state: EditorState,
    undo: Vec<Step>,
    redo: Vec<Step>,
    merge_key: Option<String>,
    /// Where the document saves. `file.newProject` and `file.open` change it.
    path: Option<String>,
    journal_len: usize,
    /// The media pool of the document and what it held. `file.open` swaps the pool, and
    /// `file.newProjectFromTemplate` clears it in place, with the paths' documents.
    media: Arc<MediaPool>,
    pool: PoolSnapshot,
    /// The project was clean (saved) when the scope began.
    clean: bool,
    /// `Session::saved_revision` when the scope began. A command such as `file.newProject` moves it,
    /// and a dirty document must come back dirty.
    saved_revision: u64,
    /// `Session::revision` after the scope's last command. A rollback from another thread only runs
    /// when this still holds, so it never undoes work that is not the plug-in's.
    last_revision: u64,
}

impl Checkpoint {
    fn take(s: &Session) -> Checkpoint {
        Checkpoint {
            project: Arc::clone(&s.project),
            state: s.state.clone(),
            undo: s.history.undo.clone(),
            redo: s.history.redo.clone(),
            merge_key: s.history.merge_key.clone(),
            path: s.path.clone(),
            journal_len: s.journal.len(),
            media: Arc::clone(&s.media),
            pool: s.media.snapshot(),
            clean: !s.is_dirty(),
            saved_revision: s.saved_revision,
            last_revision: s.revision,
        }
    }

    /// Put the document back. The revision still goes up, so the change feed reports the revert.
    fn restore(self, s: &mut Session) {
        let hooks = s.hooks.clone();
        let path_moved = s.path != self.path;
        s.path = self.path;
        s.media = self.media;
        s.media.restore(self.pool);
        hooks.with_cause(Cause::Job("plugin rollback"), || {
            s.set_project(self.project);
            s.state = self.state;
            s.history.undo = self.undo;
            s.history.redo = self.redo;
            s.history.merge_key = self.merge_key;
            s.journal.truncate(self.journal_len);
            s.bump_view();
            // The revision went up, so a clean document is clean at the new one. A dirty document
            // returns to the revision it was saved at, which `bump_view` may have replaced.
            s.saved_revision = if self.clean { s.revision } else { self.saved_revision };
        });
        if path_moved {
            // The previews follow the path, as they do when the document is saved elsewhere.
            if s.path.is_some() {
                s.previews_follow_path();
            } else {
                s.previews.reset_temp();
            }
        }
        s.sync_persistence();
        s.previews.live.publish_project(Arc::clone(&s.project));
    }
}

struct Live {
    session: Box<Session>,
    thread: ThreadId,
}

/// Where a queued job is. The waiting thread and [`Bridge::pump`] race to move it out of `QUEUED`,
/// and the one that wins decides. A job is never abandoned and run too.
const QUEUED: u8 = 0;
/// The session thread took the job. The caller waits for its result, because it may edit the
/// document or write a file even after the caller's wait has run out.
const RUNNING: u8 = 1;
/// The caller gave up before the session thread took the job. The pump drops it.
const CANCELLED: u8 = 2;

struct Job {
    state: Arc<AtomicU8>,
    work: Box<dyn FnOnce(&mut Session) + Send>,
}

thread_local! {
    /// Open scopes of guest calls on this thread, innermost last.
    static SCOPES: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// See the module comment.
pub struct Bridge {
    session_thread: ThreadId,
    live: Mutex<Vec<Live>>,
    spare: Mutex<Vec<Box<Session>>>,
    mailbox: Mutex<VecDeque<Job>>,
    scopes: Mutex<HashMap<u64, Option<Checkpoint>>>,
    next_scope: AtomicU64,
    notices: Mutex<Vec<UserNotice>>,
    wait: Duration,
}

impl Bridge {
    /// Call on the thread that owns the session. `wait` is how long another thread waits for a
    /// pump before its call fails.
    ///
    /// One spare session is built here. A `before` gate lends the session inside the 2 ms gate
    /// budget, and building a session on the first lend would cost more than that.
    pub fn new(wait: Duration) -> Arc<Bridge> {
        Arc::new(Bridge {
            session_thread: std::thread::current().id(),
            live: Mutex::new(Vec::new()),
            spare: Mutex::new(vec![Box::new(Session::new(Arc::new(FsServices)))]),
            mailbox: Mutex::new(VecDeque::new()),
            scopes: Mutex::new(HashMap::new()),
            next_scope: AtomicU64::new(1),
            notices: Mutex::new(Vec::new()),
            wait,
        })
    }

    /// `Services::lend_session`: lend `session` to host calls while `run` executes. A value that is
    /// not a [`Session`] cannot be lent, so `run` still runs and its host calls fail as busy.
    pub fn lend_any(&self, session: &mut dyn std::any::Any, run: &mut dyn FnMut()) {
        match session.downcast_mut::<Session>() {
            Some(session) => self.lend_with(session, |_lent| run()),
            None => run(),
        }
    }

    fn lend_with<R>(&self, session: &mut Session, f: impl FnOnce(&mut Session) -> R) -> R {
        let mut real = lock(&self.spare).pop().unwrap_or_else(|| Box::new(Session::new(Arc::new(FsServices))));
        std::mem::swap(session, &mut *real);
        lock(&self.live).push(Live { session: real, thread: std::thread::current().id() });
        let guard = Return { bridge: self, slot: session };
        f(&mut *guard.slot)
    }

    /// Run `f` with `session` on the live stack, so host calls made inside `f` reach it. The UI
    /// uses this for a panel event, which is a guest call that is not a command.
    pub fn scoped<R>(&self, session: &mut Session, f: impl FnOnce() -> R) -> R {
        self.lend_with(session, |_lent| f())
    }

    /// The live session if this thread lent it and it is the innermost lend, else nothing. The
    /// caller must put it back with `give_back`.
    fn take_live(&self) -> Option<Live> {
        let me = std::thread::current().id();
        let mut live = lock(&self.live);
        // Only the top of the stack may be borrowed, so the stack order stays the call order.
        if live.last().is_some_and(|l| l.thread == me) { live.pop() } else { None }
    }

    fn give_back(&self, live: Live) {
        lock(&self.live).push(live);
    }

    /// Run `f` on the session. See the module comment for who gets what.
    pub fn with_session<R: Send + 'static>(&self, f: impl FnOnce(&mut Session) -> R + Send + 'static) -> Result<R, Unavailable> {
        if let Some(mut live) = self.take_live() {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut live.session)));
            self.give_back(live);
            return match ran {
                Ok(r) => Ok(r),
                Err(payload) => std::panic::resume_unwind(payload),
            };
        }
        if std::thread::current().id() == self.session_thread {
            return Err(Unavailable::Busy);
        }
        let (tx, rx) = channel();
        let state = Arc::new(AtomicU8::new(QUEUED));
        let job = Job {
            state: Arc::clone(&state),
            work: Box::new(move |s| {
                // The receiver is alive while the job runs: the caller waits for a running job.
                let _ = tx.send(f(s));
            }),
        };
        lock(&self.mailbox).push_back(job);
        match rx.recv_timeout(self.wait) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Disconnected) => Err(Unavailable::TimedOut),
            Err(RecvTimeoutError::Timeout) => {
                // Cancel only a job nobody has started. A running job finishes, and the caller gets
                // its real answer instead of a timeout that hides an edit.
                match state.compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => Err(Unavailable::TimedOut),
                    // The sender drops without a value when the job panics.
                    Err(_) => rx.recv().map_err(|_| Unavailable::TimedOut),
                }
            }
        }
    }

    /// Run the jobs other threads queued. Call once per frame on the session thread, with no
    /// plugin command running. Returns how many ran.
    pub fn pump(&self, session: &mut Session) -> usize {
        let mut ran = 0;
        loop {
            let Some(job) = lock(&self.mailbox).pop_front() else {
                return ran;
            };
            // The caller may have cancelled while the job waited. Then it is not run.
            if job.state.compare_exchange(QUEUED, RUNNING, Ordering::AcqRel, Ordering::Acquire).is_err() {
                continue;
            }
            let work = job.work;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(session))).is_err() {
                log::error!("a plug-in host call panicked on the session thread");
            }
            ran += 1;
        }
    }

    /// Jobs waiting for a pump.
    pub fn queued(&self) -> usize {
        lock(&self.mailbox).len()
    }

    // ---- scopes ------------------------------------------------------------------

    /// A guest call starts on this thread.
    pub fn begin(&self) -> u64 {
        let id = self.next_scope.fetch_add(1, Ordering::Relaxed);
        lock(&self.scopes).insert(id, None);
        SCOPES.with(|s| s.borrow_mut().push(id));
        id
    }

    /// The scope of the innermost guest call on this thread.
    pub fn current_scope() -> Option<u64> {
        SCOPES.with(|s| s.borrow().last().copied())
    }

    /// A guest call ends. A fault puts the document back to its checkpoint, if the call took one.
    /// Returns a message when it could not.
    pub fn end(&self, id: u64, faulted: bool) -> Option<String> {
        SCOPES.with(|s| s.borrow_mut().retain(|x| *x != id));
        let checkpoint = lock(&self.scopes).remove(&id).flatten()?;
        if !faulted {
            return None;
        }
        let expect = checkpoint.last_revision;
        let outcome = self.with_session(move |s| {
            if s.revision != expect {
                return Err("the document changed after the plug-in's edits, so they were left in place".to_owned());
            }
            checkpoint.restore(s);
            Ok(())
        });
        match outcome {
            Ok(Ok(())) => None,
            Ok(Err(why)) => Some(why),
            Err(why) => Some(format!("the plug-in's edits could not be undone: {why}")),
        }
    }

    /// Open the checkpoint of `scope` if it has none. Call on the session, before a command runs.
    fn ensure_checkpoint(&self, scope: Option<u64>, s: &Session) {
        let Some(id) = scope else { return };
        if let Some(slot) = lock(&self.scopes).get_mut(&id)
            && slot.is_none()
        {
            *slot = Some(Checkpoint::take(s));
        }
    }

    fn note_revision(&self, scope: Option<u64>, revision: u64) {
        if let Some(id) = scope
            && let Some(Some(cp)) = lock(&self.scopes).get_mut(&id)
        {
            cp.last_revision = revision;
        }
    }

    // ---- commands -------------------------------------------------------------------------

    /// `cmd.execute`: run `id` as one undo step. The scope of the calling guest, if any, gets its
    /// checkpoint first.
    pub fn execute(self: &Arc<Bridge>, id: &str, params: &Value) -> Result<Value, CommandFailure> {
        let scope = Bridge::current_scope();
        let (me, id, params) = (Arc::clone(self), id.to_owned(), params.clone());
        match self.with_session(move |s| me.execute_on(s, scope, &id, params)) {
            Ok(r) => r,
            Err(why) => Err(CommandFailure::Failed(why.to_string())),
        }
    }

    fn execute_on(&self, s: &mut Session, scope: Option<u64>, id: &str, params: Value) -> Result<Value, CommandFailure> {
        if s.registry.resolve(id).is_none() {
            return Err(CommandFailure::Unknown);
        }
        if s.exec_depth >= MAX_NESTING {
            return Err(CommandFailure::Failed(format!("commands are nested more than {MAX_NESTING} deep")));
        }
        self.ensure_checkpoint(scope, s);
        // A command that edits twice is still one step for the plug-in. Lift the history limit
        // while it runs so the count of new steps is exact, then fold them.
        let limit = s.history.limit;
        s.history.limit = limit.saturating_add(1024);
        let before = s.history.undo.len();
        let result = s.execute_as(id, params, craft_command::Origin::Plugin);
        s.history.limit = limit;
        fold_steps(s, before);
        self.note_revision(scope, s.revision);
        result.map_err(|e| CommandFailure::Failed(e.to_string()))
    }
}

/// Fold the undo steps pushed since `before` into one, and trim to the limit.
fn fold_steps(s: &mut Session, before: usize) {
    let after = s.history.undo.len();
    if after > before.saturating_add(1)
        && let (Some(first), Some(last)) = (s.history.undo.get(before), s.history.undo.last())
    {
        let folded = (last.0.clone(), Arc::clone(&first.1), first.2.clone());
        s.history.undo.truncate(before);
        s.history.undo.push(folded);
    }
    let limit = s.history.limit;
    let excess = s.history.undo.len().saturating_sub(limit);
    if excess > 0 {
        s.history.undo.drain(..excess);
    }
}

/// Puts the real session back into the lent slot when the command body ends or unwinds.
struct Return<'a> {
    bridge: &'a Bridge,
    slot: &'a mut Session,
}

impl Drop for Return<'_> {
    fn drop(&mut self) {
        let me = std::thread::current().id();
        let mut live = lock(&self.bridge.live);
        let Some(at) = live.iter().rposition(|l| l.thread == me) else {
            return;
        };
        let mut entry = live.remove(at);
        drop(live);
        std::mem::swap(self.slot, &mut *entry.session);
        lock(&self.bridge.spare).push(entry.session);
    }
}

impl Bridge {
    /// Queue a message for the user. [`Bridge::drain_notices`] hands it to the session.
    pub fn notify(&self, notice: UserNotice) {
        lock(&self.notices).push(notice);
    }

    pub fn drain_notices(&self) -> Vec<UserNotice> {
        std::mem::take(&mut lock(&self.notices))
    }
}
