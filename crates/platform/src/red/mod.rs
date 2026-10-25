//! Optional SDK source (Windows x64 and Linux x64). The proprietary decoder runs outside the
//! editor process.
//!
//! Frames come back through shared memory (since FCRD6, [`shm`], the backend's one FFI module); the
//! rest of this module is safe code over pipes.
#[allow(unsafe_code)]
mod shm;

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use filmcraft_color::{ColorInfo, Transfer};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{
    AudioStreamInfo, FrameRequest, MediaError, MediaInfo, MediaKind, MediaSource, RawAsShot, RawClipInfo, RawColorSpace, RawDevelop, RawFlashing, RawGamma,
    RawRollOff, RawToneMap, Result, SharedReader, SharedSource, VideoStreamInfo,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

const MAX_PIXELS: usize = 33_554_432;
const MAX_SAMPLES: usize = 1_048_576;
const MAX_REPLY: usize = MAX_PIXELS * 6;
const TIMEOUT: Duration = Duration::from_secs(120);

fn failure(message: impl Into<String>) -> MediaError {
    MediaError::Decode(message.into())
}
fn io(error: std::io::Error) -> MediaError {
    failure(format!("RED decoder: {error}"))
}

/// `FILMCRAFT_R3D_TIMING` is set: the helper's per-frame timings reach the app's stderr, and this
/// side logs each request's wait for a slot, round trip, reply read and conversion.
fn timing() -> bool {
    static TIMING: OnceLock<bool> = OnceLock::new();
    *TIMING.get_or_init(|| std::env::var_os("FILMCRAFT_R3D_TIMING").is_some())
}

/// The helper's stdin, shared by the request writers and the slot leases (a lease hands its
/// slot back from wherever it is dropped).
type Commands = Arc<Mutex<Box<dyn Write + Send>>>;

/// The helper's frame slots: its section mapped read-only, with the geometry its metadata
/// declared, and the stdin to hand slots back on.
struct SharedFrames {
    mapping: shm::Mapping,
    slots: u32,
    slot_bytes: usize,
    commands: Commands,
}

/// A decoded frame the helper handed over in one of its shared slots. The bytes are read in
/// place; dropping the lease tells the helper the slot is free (`F <slot>`), wherever that
/// happens: after the repack, inside a cancelled requester's abandoned channel, or in the
/// reader discarding an orphan. A helper that has exited makes the write fail, which is fine:
/// its slots died with it, while the pages stay readable for as long as the mapping lives.
struct SlotLease {
    shared: Arc<SharedFrames>,
    index: u32,
    len: usize,
    delivered: Instant,
}
impl SlotLease {
    fn bytes(&self) -> Result<&[u8]> {
        let offset = (self.index as usize).checked_mul(self.shared.slot_bytes).ok_or_else(|| failure("RED frame slot out of range"))?;
        self.shared.mapping.slice(offset, self.len)
    }
}
impl Drop for SlotLease {
    fn drop(&mut self) {
        let mut commands = self.shared.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = commands.write_all(format!("F {}\n", self.index).as_bytes()).and_then(|_| commands.flush());
    }
}

/// One answer from the helper: bytes that came down the pipe (metadata, audio, and video from a
/// CPU-only helper), a video frame waiting in a shared slot, or the helper's error for this
/// request. An error answers one request (an unreadable clip, a malformed setting) and leaves the
/// helper serving every other clip (FCRD8: one helper serves them all).
enum Reply {
    Inline(Vec<u8>),
    Slot(SlotLease),
    Error(String),
}
impl Reply {
    fn into_inline(self) -> Result<Vec<u8>> {
        match self {
            Self::Inline(bytes) => Ok(bytes),
            Self::Slot(_) => Err(failure("RED decoder answered with a frame slot where bytes were expected")),
            Self::Error(message) => Err(failure(format!("RED decoder: {message}"))),
        }
    }
}

/// Reply header: status `u32`, request id `u64`, payload length `u64`, slot `u32`.
const HEADER: usize = 24;
/// Status of a reply whose frame is in a shared slot (no payload follows).
const STATUS_SHARED: u32 = 2;

fn read_reply(reader: &mut impl Read, shared: &OnceLock<Arc<SharedFrames>>) -> Result<(u64, Result<Reply>)> {
    let mut header = [0u8; HEADER];
    reader.read_exact(&mut header).map_err(io)?;
    let t0 = Instant::now();
    let status = u32::from_le_bytes(header[..4].try_into().map_err(|_| failure("invalid RED response"))?);
    let id = u64::from_le_bytes(header[4..12].try_into().map_err(|_| failure("invalid RED request id"))?);
    let size = u64::from_le_bytes(header[12..20].try_into().map_err(|_| failure("invalid RED response"))?);
    let slot = u32::from_le_bytes(header[20..].try_into().map_err(|_| failure("invalid RED response"))?);
    if size > MAX_REPLY as u64 || (status == 1 && size > 16_384) {
        return Err(failure("RED decoder response exceeds memory limit"));
    }
    if status == STATUS_SHARED {
        // The frame is in the helper's section; nothing follows the header. The slot and the
        // length are the helper's word, checked against the geometry it declared at start-up
        // (and the mapping checks every read against the bytes it mapped).
        let shared = shared.get().ok_or_else(|| failure("RED decoder answered with a frame slot before sharing frames"))?;
        if slot >= shared.slots || size as usize > shared.slot_bytes {
            return Err(failure("RED frame slot out of range"));
        }
        return Ok((id, Ok(Reply::Slot(SlotLease { shared: shared.clone(), index: slot, len: size as usize, delivered: t0 }))));
    }
    if status > 1 || slot != 0 {
        return Err(failure("invalid RED response"));
    }
    // Frame-sized replies come from the plane pool: a fresh 31 MB allocation per frame at 24 fps
    // is page faults and a zero fill every time (`video_frame` hands the buffer back).
    let mut bytes = filmcraft_frame::pool::take_u8(size as usize);
    bytes.try_reserve_exact(size as usize).map_err(|_| failure("RED response allocation failed"))?;
    bytes.resize(size as usize, 0);
    let t1 = Instant::now();
    reader.read_exact(&mut bytes).map_err(io)?;
    if timing() && size >= 1 << 20 {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        log::info!("R3D reply {id}: {:.1} MB, buffer {:.1} ms, pipe read {:.1} ms", size as f64 / 1e6, ms(t1.saturating_duration_since(t0)), ms(t1.elapsed()));
    }
    if status != 0 {
        return Ok((id, Ok(Reply::Error(String::from_utf8_lossy(&bytes).into_owned()))));
    }
    Ok((id, Ok(Reply::Inline(bytes))))
}

struct PendingRequest {
    sender: mpsc::SyncSender<Result<Reply>>,
    started: Instant,
    /// Requests outstanding when this one was registered, this one included.
    in_flight: usize,
}
impl PendingRequest {
    fn new(sender: mpsc::SyncSender<Result<Reply>>) -> Self {
        Self { sender, started: Instant::now(), in_flight: 1 }
    }
}
/// One answered request: when its reply arrived, how many were in flight with it, how long it took.
#[derive(Clone, Copy)]
struct Answered {
    at: Instant,
    in_flight: usize,
    /// Requests outstanding when this one was answered, this one included.
    outstanding: usize,
    took: Duration,
}
/// Replies remembered for the throughput estimate ([`Pending::rate`]).
const RATE_WINDOW: usize = 32;
/// Replies older than this before the newest say nothing about the current throughput.
const RATE_SPAN: Duration = Duration::from_secs(2);
#[derive(Default)]
struct Pending {
    next: u64,
    failed: Option<String>,
    requests: HashMap<u64, PendingRequest>,
    answered: VecDeque<Answered>,
    /// The helper's decode slots (0 until known): the pipeline's throughput shows only in replies
    /// to requests that had that many in flight with them.
    slots: usize,
    /// The last throughput read off saturated replies (see [`Self::rate`]), and when.
    rate: Option<f64>,
    rate_at: Option<Instant>,
}
impl Pending {
    fn fail(&mut self, message: &str) {
        self.failed = Some(message.into());
        for (_, sender) in self.requests.drain() {
            let _ = sender.sender.send(Err(failure(message)));
        }
    }
    fn expired(&self, timeout: Duration) -> bool {
        self.requests.values().any(|request| request.started.elapsed() >= timeout)
    }
    fn dispatch(&mut self, id: u64, reply: Result<Reply>) -> Result<()> {
        // After start-up the helper sends ID 0 only for its own fatal error, then exits.
        if id == 0 && !self.requests.contains_key(&0) {
            return Err(match reply {
                Ok(Reply::Error(message)) => failure(format!("RED decoder: {message}")),
                Err(e) => e,
                Ok(_) => failure("unexpected RED start-up reply"),
            });
        }
        let request = self.requests.remove(&id).ok_or_else(|| failure("unexpected RED response id"))?;
        // the helper's own fatal error after start-up arrives as ID 0 (handled above); at
        // start-up its error reply is the start-up failure
        let reply = match reply {
            Ok(Reply::Error(message)) if id == 0 => Err(failure(format!("RED decoder: {message}"))),
            other => other,
        };
        if id != 0 && matches!(reply, Ok(Reply::Inline(_) | Reply::Slot(_))) {
            if self.answered.len() >= RATE_WINDOW {
                self.answered.pop_front();
            }
            self.answered.push_back(Answered {
                at: Instant::now(),
                in_flight: request.in_flight,
                outstanding: self.requests.len().saturating_add(1),
                took: request.started.elapsed(),
            });
        }
        // A cancelled requester may have dropped its receiver. Still consume its reply.
        let _ = request.sender.send(reply);
        Ok(())
    }
    /// Frames per second the helper delivers when kept busy: `in flight / latency` (Little's
    /// law, with everything in flight: a request waiting in the helper's queue is part of the
    /// latency of the ones it waits behind) averaged over the recent replies to requests that
    /// had at least the slot count in flight when they were registered and still when they were
    /// answered. Both ends, because Little's law holds for a system kept full: a burst into an
    /// idle helper registers its fifth request with five in flight, yet that one took a free
    /// slot and never queued, and its short latency read as 32 fps from a pipeline that
    /// completes 26 when full (measured with five slots: the plan then stepped the
    /// 60p clip up to every second frame and cascaded back to every fifth). Replies from a lull
    /// say nothing about capacity (with one request in flight the SDK idles at ~4 fps, and
    /// playback that asks for less would read that as the pipeline falling behind and ask for
    /// less again, down to every tenth frame), so through a lull the last saturated reading
    /// stands. A request that was cancelled after submission still counts, since the helper
    /// decoded it. `None` until three saturated replies.
    fn rate(&mut self) -> Option<f64> {
        let newest = self.answered.back()?.at;
        let full = self.slots.max(1);
        let saturated =
            |a: &&Answered| newest.saturating_duration_since(a.at) <= RATE_SPAN && !a.took.is_zero() && a.in_flight >= full && a.outstanding >= full;
        let rates: Vec<f64> = self.answered.iter().filter(saturated).map(|a| a.in_flight as f64 / a.took.as_secs_f64()).collect();
        if rates.len() >= 3
            && let mean = rates.iter().sum::<f64>() / rates.len() as f64
            && mean.is_finite()
            && mean > 0.0
        {
            self.rate = Some(mean);
            self.rate_at = Some(newest);
        }
        self.rate
    }
    /// [`Self::rate`] and whether it was read within [`RATE_SPAN`] of now.
    fn delivery(&mut self) -> (Option<f64>, bool) {
        let rate = self.rate();
        (rate, self.rate_at.is_some_and(|at| at.elapsed() <= RATE_SPAN))
    }
}

/// The requests outstanding on one helper, shared by the callers, the response reader and the
/// watchdog. `changed` wakes callers waiting for a free slot.
#[derive(Default)]
struct Registry {
    pending: Mutex<Pending>,
    changed: Condvar,
    /// The helper's frame slots, mapped once its metadata has said how many and how large
    /// (a GPU helper; a CPU-only one sends its frames inline and never sets this).
    shared: OnceLock<Arc<SharedFrames>>,
}
impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Register a request once fewer than `limit` are outstanding. The helper has that many
    /// decode slots (plus one line waiting in its pipe): a request sent beyond them would only
    /// queue in the pipe, where it cannot be cancelled and will occupy a slot for a frame playback
    /// has moved past. Waiting here is cancellable, and a cancelled wait sends nothing.
    fn register(&self, limit: usize, sender: mpsc::SyncSender<Result<Reply>>) -> Result<u64> {
        let started = Instant::now();
        let mut pending = self.lock();
        loop {
            if let Some(error) = &pending.failed {
                return Err(failure(error));
            }
            if pending.requests.len() < limit {
                break;
            }
            if filmcraft_media::cancel::cancelled() {
                return Err(MediaError::Cancelled);
            }
            if started.elapsed() >= TIMEOUT {
                return Err(failure("RED decoder slots stayed busy for 120 seconds"));
            }
            pending = self.changed.wait_timeout(pending, Duration::from_millis(10)).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
        pending.next = pending.next.checked_add(1).ok_or_else(|| failure("RED request ids exhausted"))?;
        let id = pending.next;
        let mut request = PendingRequest::new(sender);
        request.in_flight = pending.requests.len().saturating_add(1);
        pending.requests.insert(id, request);
        Ok(id)
    }
    fn unregister(&self, id: u64) {
        self.lock().requests.remove(&id);
        self.changed.notify_all();
    }
    fn dispatch(&self, id: u64, reply: Result<Reply>) -> Result<()> {
        let result = self.lock().dispatch(id, reply);
        self.changed.notify_all();
        result
    }
    fn fail(&self, message: &str) {
        self.lock().fail(message);
        self.changed.notify_all();
    }
}

static PIPE_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Helper processes alive right now (logged with every start and close: a count that climbs
/// is the first sign of a source opened once per frame worker or per settings change).
static LIVE_PROCESSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Longest helper stderr line kept whole; longer ones arrive in pieces.
const STDERR_LINE_MAX: u64 = 4096;

/// Forward the helper's stderr to the app log, one record a line, until the helper exits. Its
/// per-frame timings (`FILMCRAFT_R3D_TIMING`, which keeps stderr inherited instead) would be
/// debug records; everything else it says there is a problem report.
fn forward_stderr(pid: u32, stderr: std::process::ChildStderr) {
    let spawned = std::thread::Builder::new().name("red-stderr".into()).spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut reader = std::io::BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match (&mut reader).take(STDERR_LINE_MAX).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = line.trim_end();
                if text.is_empty() {
                    continue;
                }
                if text.starts_with("R3D frame") || text.starts_with("R3D slot") {
                    log::debug!("R3D helper {pid}: {text}");
                } else {
                    log::warn!("R3D helper {pid}: {text}");
                }
            }
        }));
    });
    if let Err(e) = spawned {
        log::warn!("R3D helper {pid}: stderr not forwarded: {e}");
    }
}

/// How long a helper may take to create its reply pipe (it does so before loading the SDK).
#[cfg(windows)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the helper's replies come from: on Windows the named pipe it creates, on Linux its
/// stdout (see `connect_replies`).
type Replies = Box<dyn Read + Send>;

/// Open the helper's reply pipe `\\.\pipe\<name>` as a file (the helper creates it first
/// thing and waits for this connection). A helper that exits first reports its start-up error
/// on stdout in the reply framing; that message is returned.
#[cfg(windows)]
fn connect_replies(child: &mut Child, mut stdout: std::process::ChildStdout, name: &str) -> Result<Replies> {
    let path = format!(r"\\.\pipe\{name}");
    let started = Instant::now();
    loop {
        let error = match std::fs::File::open(&path) {
            Ok(f) => return Ok(Box::new(f)),
            Err(e) => e,
        };
        if let Ok(Some(status)) = child.try_wait() {
            let message = match read_reply(&mut stdout, &OnceLock::new()) {
                Ok((_, Err(e))) => e.to_string(),
                _ => format!("RED helper exited ({status}) before creating its reply pipe"),
            };
            return Err(failure(message));
        }
        if started.elapsed() >= CONNECT_TIMEOUT {
            return Err(failure(format!("RED helper reply pipe unavailable: {error}")));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Linux: the helper's stdout is its reply pipe (it widens the pipe to 1 MiB itself; since
/// FCRD6 nothing frame-sized goes down it), and the name only names the shared frame memory.
#[cfg(target_os = "linux")]
fn connect_replies(_: &mut Child, stdout: std::process::ChildStdout, _: &str) -> Result<Replies> {
    Ok(Box::new(stdout))
}

/// Linux: `REDR3D-x64.so` loads `REDCuda-x64.so`, `REDOpenCL-x64.so` and `REDDecoder-x64.so` by
/// name, so the runtime directory goes first on the helper's `LD_LIBRARY_PATH` (ahead of whatever
/// the app inherited). A directory `join_paths` rejects (one containing `:`) is used on its own.
#[cfg(target_os = "linux")]
fn library_search_path(libraries: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
    let paths = std::iter::once(libraries.to_path_buf()).chain(std::env::split_paths(&inherited));
    std::env::join_paths(paths).unwrap_or_else(|_| libraries.as_os_str().to_os_string())
}

struct Worker {
    child: Arc<Mutex<Child>>,
    commands: Commands,
    registry: Arc<Registry>,
    shutdown: mpsc::Sender<()>,
    /// Requests outstanding at once: the helper's decode slots plus one waiting in its pipe.
    limit: usize,
    /// The helper process, for the log: its id, when it started, and whether it is the GPU or
    /// the CPU helper.
    pid: u32,
    started: Instant,
    label: String,
}

impl Worker {
    /// A helper that serves any number of clips (FCRD8; clips are opened in it with `O`): the GPU
    /// helper (`cpu_only` false), whose shared frame slots hold `slot_bytes` each, or the CPU
    /// helper (audio, thumbnails, metadata probes, and frames while the GPU is unavailable).
    fn open_hub(cpu_only: bool, slot_bytes: usize) -> Result<(Self, HubInfo)> {
        #[cfg(test)]
        if !cpu_only && tests::FAIL_GPU_STARTS.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(failure("RED decoder: Create RED GPU decoder (RED GPU status 13; simulated)"));
        }
        let helper_name = format!("filmcraft-red-helper{}", std::env::consts::EXE_SUFFIX);
        let beside_app = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join(&helper_name)));
        let helper = std::env::var_os("FILMCRAFT_R3D_HELPER")
            .map(PathBuf::from)
            .or_else(|| beside_app.filter(|p| p.is_file()))
            .unwrap_or_else(|| PathBuf::from(env!("FILMCRAFT_BUILT_RED_HELPER")));
        let libraries = std::env::var_os("RED_R3D_LIBRARY_PATH")
            .map(PathBuf::from)
            .or_else(|| helper.parent().map(Path::to_path_buf))
            .ok_or_else(|| failure("RED helper directory missing"))?;
        let mut command = Command::new(&helper);
        // Audio, thumbnails and the import probe need no GPU.
        if cpu_only {
            command.env("FILMCRAFT_R3D_PROCESSING", "cpu");
        }
        // Replies come down a named pipe the helper creates (see `connect_replies`); its name is
        // unguessable and the helper allows one instance, so nothing else can serve or take it.
        // The same name names the helper's shared frame memory.
        let pipe_name = format!("filmcraft-red-{}-{}-{:016x}", std::process::id(), PIPE_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed), {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
            h.finish()
        });
        let slots_argument = if cpu_only { 0 } else { slot_bytes };
        command.arg(&libraries).arg(&pipe_name).arg(slots_argument.to_string()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(if timing() {
            Stdio::inherit()
        } else {
            Stdio::piped() // forwarded to the app log (`forward_stderr`)
        });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW: the helper has no interactive UI.
        }
        #[cfg(target_os = "linux")]
        command.env("LD_LIBRARY_PATH", library_search_path(&libraries));
        let mut child = command
            .spawn()
            .map_err(|e| failure(format!("Cannot start RED helper {}: {e}. Rebuild with red-sdk or set FILMCRAFT_R3D_HELPER.", helper.display())))?;
        // Until the app holds the helper's frame memory, leaving here removes its name (Linux;
        // a Windows section dies with its handles).
        let _frame_memory_name = shm::Name::new(&pipe_name);
        let pid = child.id();
        let started = Instant::now();
        let label = format!("{} helper", if cpu_only { "CPU" } else { "GPU" });
        let live = LIVE_PROCESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed).saturating_add(1);
        log::info!("R3D helper {pid} ({label}) started from {}; {live} live", helper.display());
        if let Some(stderr) = child.stderr.take() {
            forward_stderr(pid, stderr);
        }
        let pipes = (child.stdin.take(), child.stdout.take());
        let (Some(stdin), Some(stdout)) = pipes else {
            let _ = child.kill();
            let _ = child.wait();
            LIVE_PROCESSES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            return Err(failure("RED helper pipes unavailable"));
        };
        let mut stdout = match connect_replies(&mut child, stdout, &pipe_name) {
            Ok(f) => f,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                LIVE_PROCESSES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                log::warn!("R3D helper {pid} ({label}) did not connect: {e}");
                return Err(e);
            }
        };
        let (tx, replies) = mpsc::sync_channel(1);
        let registry = Arc::new(Registry::default());
        registry.lock().requests.insert(0, PendingRequest::new(tx));
        let child = Arc::new(Mutex::new(child));
        let reader_registry = registry.clone();
        let reader_child = child.clone();
        let thread = std::thread::Builder::new().name("red-responses".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                loop {
                    let (id, reply) = read_reply(&mut stdout, &reader_registry.shared)?;
                    reader_registry.dispatch(id, reply)?;
                }
            }));
            let message = match result {
                Ok(Err(e)) => e.to_string(),
                _ => "RED response worker panicked or stopped".into(),
            };
            // An orderly close fails the registry first ("RED worker closed"); only a reader
            // that stopped on its own has something to report.
            let closing = match &reader_registry.lock().failed {
                None => {
                    log::warn!("R3D helper {pid} replies ended: {message}");
                    false
                }
                Some(why) => why == CLOSED,
            };
            reader_registry.fail(&message);
            // an orderly close is letting the helper exit by itself (Drop kills it if it does not)
            if !closing {
                let _ = reader_child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).kill();
            }
        });
        let (shutdown, stop) = mpsc::channel();
        let commands: Commands = Arc::new(Mutex::new(Box::new(stdin)));
        let mut worker = Self { child, commands, registry, shutdown, limit: 1, pid, started, label };
        if let Err(e) = thread {
            worker.fail(&format!("RED response thread: {e}"));
            return Err(io(e));
        }
        // Cancelled callers drop their receiver immediately, but their submitted jobs still
        // retain a deadline. A stalled orphan must also terminate the isolated helper.
        let watchdog_registry = worker.registry.clone();
        let watchdog_child = worker.child.clone();
        let watchdog = std::thread::Builder::new().name("red-timeouts".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    match stop.recv_timeout(Duration::from_millis(100)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let pending = watchdog_registry.lock();
                    if pending.failed.is_some() {
                        return;
                    }
                    if pending.expired(TIMEOUT) {
                        drop(pending);
                        watchdog_registry.fail("RED request exceeded 120-second deadline");
                        let _ = watchdog_child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).kill();
                        return;
                    }
                }
            }));
            if result.is_err() {
                watchdog_registry.fail("RED timeout worker panicked");
                let _ = watchdog_child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).kill();
            }
        });
        if let Err(e) = watchdog {
            return Err(io(e));
        }
        let metadata = worker.receive(replies, false)?.into_inline()?;
        let parsed = HubInfo::parse(&metadata)?;
        // A GPU helper's frames wait in its shared slots: map them now, before any request,
        // sized exactly as its metadata says (fewer bytes in the section fail the mapping).
        if parsed.slots > 0 {
            let len = parsed.slot_bytes.checked_mul(parsed.slots).ok_or_else(|| failure("RED frame slots too large"))?;
            let mapping = match shm::Mapping::open(&pipe_name, len) {
                Ok(mapping) => mapping,
                Err(e) => {
                    worker.fail(&e.to_string());
                    return Err(e);
                }
            };
            let shared = SharedFrames { mapping, slots: parsed.slots as u32, slot_bytes: parsed.slot_bytes, commands: worker.commands.clone() };
            if worker.registry.shared.set(Arc::new(shared)).is_err() {
                return Err(failure("RED frame slots mapped twice"));
            }
        }
        // The GPU helper keeps its slots busy plus one request waiting; the CPU helper's threads
        // each take one, with as many waiting (audio jumps the queue inside the helper).
        worker.limit = if parsed.slots > 0 { parsed.slots.saturating_add(1) } else { parsed.threads.max(1).saturating_mul(2) };
        worker.registry.lock().slots = if parsed.slots > 0 { parsed.slots } else { parsed.threads.max(1) };
        log::info!(
            "R3D helper {} ({}) ready in {:.1} s: {}, {} slots of {:.1} MB, {} CPU threads",
            worker.pid,
            worker.label,
            worker.started.elapsed().as_secs_f64(),
            parsed.processing,
            parsed.slots,
            parsed.slot_bytes as f64 / 1e6,
            parsed.threads
        );
        Ok((worker, parsed))
    }
    fn fail(&self, message: &str) {
        // The orderly close (`Drop`) fails the registry too; that one is not a problem.
        if message != CLOSED && self.registry.lock().failed.is_none() {
            log::warn!("R3D helper {} ({}) failed: {message}", self.pid, self.label);
        }
        self.registry.fail(message);
        let _ = self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).kill();
    }
    fn receive(&self, replies: mpsc::Receiver<Result<Reply>>, cancellable: bool) -> Result<Reply> {
        let started = Instant::now();
        let result = loop {
            if cancellable && filmcraft_media::cancel::cancelled() {
                return Err(MediaError::Cancelled); // The reader consumes and discards the eventual reply.
            }
            match replies.recv_timeout(Duration::from_millis(10)) {
                Ok(reply) => break reply,
                Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() < TIMEOUT => continue,
                Err(e) => break Err(failure(format!("RED worker stopped responding: {e}"))),
            }
        };
        if let Err(e) = &result {
            self.fail(&e.to_string());
        }
        result
    }
    fn request(&self, command: &str) -> Result<Reply> {
        let (op, arguments) = command.split_once(' ').ok_or_else(|| failure("invalid RED command"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let t0 = Instant::now();
        let id = self.registry.register(self.limit, sender)?;
        let t1 = Instant::now();
        // Cancelled on the way to the pipe: nothing is sent, so nothing is decoded for it.
        if filmcraft_media::cancel::cancelled() {
            self.registry.unregister(id);
            return Err(MediaError::Cancelled);
        }
        // Only the short write is serialized. Waiting and conversion never hold this lock.
        let mut commands = self.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(e) = commands.write_all(format!("{op} {id} {arguments}").as_bytes()).and_then(|_| commands.flush()) {
            drop(commands);
            self.fail(&format!("RED request write: {e}"));
            return Err(io(e));
        }
        drop(commands);
        // the helper's error for this request: an error for its caller, the helper serves on
        let result = self.receive(receiver, true).and_then(|reply| match reply {
            Reply::Error(message) => Err(failure(format!("RED decoder: {message}"))),
            other => Ok(other),
        });
        if timing() && op == "V" {
            let in_flight = self.registry.lock().requests.len();
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            log::info!(
                "R3D request {id} ({}): slot wait {:.1} ms, round trip {:.1} ms, {in_flight} still in flight, {}",
                arguments.trim_end(),
                ms(t1.saturating_duration_since(t0)),
                ms(t1.elapsed()),
                match &result {
                    Ok(_) => "ok",
                    Err(MediaError::Cancelled) => "cancelled",
                    Err(_) => "failed",
                }
            );
        }
        result
    }
}
/// The failure message of an orderly close: callers still waiting get it as their error.
const CLOSED: &str = "RED worker closed";

/// Whether the last helper closed exited with success (tests).
#[cfg(test)]
static LAST_CLOSE_SUCCESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long an orderly close waits for the helper to exit by itself (it finishes the decodes in
/// flight and releases the SDK) before killing it.
const ORDERLY_EXIT: Duration = Duration::from_secs(3);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        let earlier = self.registry.lock().failed.clone();
        // An orderly close (idle, or replaced by a helper with larger slots) ends the helper's
        // input: it exits 0 by itself, so the log tells a helper that really failed (a non-zero exit
        // code) from an orderly close. A helper that
        // failed, or does not exit in time, is killed as before.
        // closed first: the reply reader then takes the helper's end of output as this close
        self.registry.fail(CLOSED);
        let exited = earlier.is_none() && {
            *self.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Box::new(std::io::sink());
            let started = Instant::now();
            loop {
                match self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).try_wait() {
                    Ok(Some(_)) => break true,
                    Ok(None) if started.elapsed() < ORDERLY_EXIT => {}
                    Ok(None) | Err(_) => break false,
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        if !exited {
            self.fail(CLOSED);
        }
        let status = self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner).wait();
        #[cfg(test)]
        LAST_CLOSE_SUCCESS.store(status.as_ref().is_ok_and(|s| s.success()), std::sync::atomic::Ordering::Relaxed);
        let live = LIVE_PROCESSES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed).saturating_sub(1);
        let status = match status {
            Ok(s) => s.to_string(),
            Err(e) => format!("not reaped: {e}"),
        };
        let why = earlier.map(|m| format!(" after failing: {m}")).unwrap_or_default();
        log::info!("R3D helper {} ({}) closed after {:.1} s ({status}){why}; {live} live", self.pid, self.label, self.started.elapsed().as_secs_f64());
    }
}

/// The protocol tag: the first field of a helper's start-up reply and of every clip's metadata.
/// A helper built for another version is refused (its requests would be misread).
const PROTOCOL: &str = "FCRD9";

/// A helper's start-up reply (FCRD9): `FCRD9 <slots> <slot bytes> <CPU threads>`, then its
/// processing backend. The clips it serves are opened afterwards (`O`), each answered with its
/// [`Metadata`].
#[derive(Debug)]
struct HubInfo {
    /// Decode slots the GPU pipeline runs at once (0 for the CPU helper).
    slots: usize,
    /// Bytes per shared frame slot (0 for the CPU helper): whole pages, at least the largest
    /// full-resolution frame the app asked for.
    slot_bytes: usize,
    /// Threads for audio and CPU frames in the helper.
    threads: usize,
    processing: String,
}
impl HubInfo {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1024 {
            return Err(failure("oversized RED start-up reply"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| failure("invalid RED start-up encoding"))?;
        let mut lines = text.lines();
        let fields: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
        if fields.len() != 4 || fields.first() != Some(&PROTOCOL) {
            return Err(failure("incompatible RED helper protocol; rebuild the app and its helper together"));
        }
        let n = |i: usize| -> Result<u64> { fields.get(i).and_then(|s| s.parse().ok()).ok_or_else(|| failure("invalid RED start-up number")) };
        let (slots, slot_bytes, threads) = (n(1)?, n(2)?, n(3)?);
        if slots > 64
            || threads == 0
            || threads > 64
            || (slots == 0) != (slot_bytes == 0)
            || (slots > 0 && (!slot_bytes.is_multiple_of(4096) || slot_bytes > MAX_REPLY as u64 + 4096 || slot_bytes.saturating_mul(slots) > MAX_SHARED))
        {
            return Err(failure("RED helper reports unsupported slots"));
        }
        let processing = lines.next().ok_or_else(|| failure("RED helper did not report its processing backend; rebuild the helper"))?;
        if processing.len() > 300
            || processing.chars().any(char::is_control)
            || !(processing.starts_with("CUDA ") || processing.starts_with("OpenCL ") || processing == "CPU (explicit override)")
        {
            return Err(failure("invalid RED processing backend"));
        }
        Ok(Self { slots: slots as usize, slot_bytes: slot_bytes as usize, threads: threads as usize, processing: processing.into() })
    }
}

#[derive(Debug)]
struct Metadata {
    width: u32,
    height: u32,
    frames: i64,
    rate: FrameRate,
    channels: usize,
    sample_rate: u32,
    samples: u64,
    float_audio: bool,
    bits: u32,
    timecode: Option<i64>,
    duration: Tick,
    /// The clip's recorded ISO, white balance and exposure adjustment (FCRD7): what a request
    /// without RAW adjustments develops with, shown by Source Settings as the defaults.
    as_shot: RawAsShot,
    /// What else the clip (or its RMD sidecar) records (FCRD9): Source Settings' "as recorded".
    clip: RawClipInfo,
    /// The pixel aspect ratio of the lens squeeze the clip records
    /// ([`filmcraft_media::lens_squeeze_par`]); square when it records none.
    par: (u32, u32),
}
/// The most shared frame memory a helper may ask the app to map.
const MAX_SHARED: u64 = 16 << 30;
impl Metadata {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 16384 {
            return Err(failure("oversized RED metadata"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| failure("invalid RED metadata encoding"))?;
        let mut lines = text.lines();
        let fields: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
        if fields.len() != 13 || fields.first() != Some(&PROTOCOL) {
            return Err(failure("incompatible RED helper protocol"));
        }
        let n = |i: usize| -> Result<u64> { fields.get(i).and_then(|s| s.parse().ok()).ok_or_else(|| failure("invalid RED metadata number")) };
        let (w, h, frames, num, den, channels, rate, samples, floating, bits, slots, slot_bytes) =
            (n(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?, n(7)?, n(8)?, n(9)?, n(10)?, n(11)?, n(12)?);
        if slots > 64 {
            return Err(failure("RED helper reports an unsupported slot count"));
        }
        if w == 0
            || h == 0
            || w > 8192
            || h > 8192
            || w * h > MAX_PIXELS as u64
            || frames == 0
            || frames > i64::MAX as u64
            || num == 0
            || den == 0
            || num > 1_000_000
            || den > 1_000_000
            || num > den * 1000
            || channels > 16
            || samples > i64::MAX as u64
            || floating > 1
            || rate > 384_000
            || bits > 32
            || (channels > 0 && (rate == 0 || ![24, 32].contains(&bits)))
        {
            return Err(failure("RED clip metadata outside supported limits"));
        }
        // A GPU helper shares slots of at least one full-resolution frame, in whole pages; a
        // CPU-only helper shares nothing.
        if (slots == 0) != (slot_bytes == 0)
            || (slots > 0
                && (slot_bytes < w * h * 6 || !slot_bytes.is_multiple_of(4096) || slot_bytes > MAX_REPLY as u64 + 4096 || slot_bytes * slots > MAX_SHARED))
        {
            return Err(failure("RED helper reports unsupported shared frame memory"));
        }
        let ticks = frames as i128 * den as i128 * TICKS_PER_SECOND as i128 / num as i128;
        if ticks == 0 || ticks > i64::MAX as i128 {
            return Err(failure("R3D duration outside supported range"));
        }
        let frame_rate = FrameRate { num: num as i64, den: den as i64 };
        let tc = lines.next().unwrap_or("");
        let timecode = if tc.is_empty() {
            None
        } else {
            Some(filmcraft_time::parse_timecode(tc, frame_rate, tc.contains(';'), 0).map_err(|_| failure("invalid R3D timecode"))?)
        };
        let processing = lines.next().ok_or_else(|| failure("RED helper did not report its processing backend; rebuild the helper"))?;
        if processing.len() > 300
            || processing.chars().any(char::is_control)
            || !(processing.starts_with("CUDA ") || processing.starts_with("OpenCL ") || processing == "CPU (explicit override)")
        {
            return Err(failure("invalid RED processing backend"));
        }
        // FCRD7: `ISO Kelvin Tint ExposureAdjust` as the SDK reports them for the clip. The values
        // are hostile numbers from a helper: finite and within the SDK's own limits (with room
        // for an SDK that widens them), or the clip is refused.
        let raw: Vec<_> =
            lines.next().ok_or_else(|| failure("RED helper did not report the clip's RAW settings; rebuild the helper"))?.split_whitespace().collect();
        let f = |i: usize| -> Result<f32> {
            raw.get(i).and_then(|s| s.parse::<f32>().ok()).filter(|v| v.is_finite()).ok_or_else(|| failure("invalid RED as-shot value"))
        };
        let iso = raw.first().and_then(|s| s.parse::<u32>().ok()).ok_or_else(|| failure("invalid RED as-shot ISO"))?;
        let (kelvin, tint, exposure) = (f(1)?, f(2)?, f(3)?);
        if raw.len() != 4
            || iso == 0
            || iso > 1_000_000
            || !(1000.0..=1_000_000.0).contains(&kelvin)
            || !(-1000.0..=1000.0).contains(&tint)
            || !(-100.0..=100.0).contains(&exposure)
        {
            return Err(failure("RED as-shot settings outside supported limits"));
        }
        let clip = parse_clip_info(lines.next(), lines.next())?;
        // The recorded lens squeeze (the desqueeze factor; 0 when none is recorded). A helper
        // from before it sends no line; a value that is no usable ratio is ignored, not refused:
        // the clip still plays, with square pixels.
        let par = match lines.next().map(str::trim) {
            None | Some("") => (1, 1),
            Some(v) => v.parse::<f64>().ok().and_then(filmcraft_media::lens_squeeze_par).unwrap_or((1, 1)),
        };
        Ok(Self {
            width: w as u32,
            height: h as u32,
            frames: frames as i64,
            rate: frame_rate,
            channels: channels as usize,
            sample_rate: rate as u32,
            samples,
            float_audio: floating == 1,
            bits: bits as u32,
            timecode,
            duration: Tick(ticks as i64),
            as_shot: RawAsShot { iso, kelvin, tint, exposure },
            clip,
            par,
        })
    }
}

struct State {
    /// The last frame: its index, preview divisor, development settings and image.
    last: Option<(i64, u32, RawDevelop, Arc<VideoFrame>)>,
}

/// The two helpers every R3D source shares (FCRD8). The GPU helper decodes foreground frames
/// into its shared slots; the CPU helper serves audio, thumbnails, metadata probes and the frames
/// of a GPU that is unavailable. A clip is opened in a helper (`O`, ~6 ms inside a running SDK)
/// the first time a source needs it there, so a timeline of a hundred clips is two processes and
/// a cut is a clip already open, not a helper start (helpers per source would cost a GPU helper
/// start of 0.2-0.8 s at each cut and a process and its pools per clip: a hundred-clip timeline
/// ran over a hundred of them).
///
/// A helper that fails or exits is started again by the next request; its `generation` tells the
/// sources their clips must be opened again. One idle for [`Hub::idle`] is closed (the GPU one
/// gives its memory back) and started again when it is next needed.
struct Hub {
    cpu_only: bool,
    idle: Duration,
    state: Mutex<HubState>,
    /// A [`Hub::warm`] start is under way.
    warming: std::sync::atomic::AtomicBool,
}
#[derive(Default)]
struct HubState {
    worker: Option<Arc<Worker>>,
    /// Counts helper starts: a source's clip handle is valid for the generation it was opened in.
    generation: u64,
    /// The current GPU helper's frame slot size.
    slot_bytes: usize,
    last_used: Option<Instant>,
    /// The GPU helper's failed starts, and until when frames decode on the CPU helper instead.
    backoff: GpuBackoff,
}
/// The frame slot size the GPU helper starts with: a Nikon ZR 6K frame (6048 Ã— 3402, 6 bytes a
/// pixel); a larger clip starts a helper with larger slots.
const DEFAULT_SLOT_BYTES: usize = 6048 * 3402 * 6;
/// Idle time after which the GPU helper is closed (its CUDA context and pools hold 1-2 GB).
const GPU_IDLE: Duration = Duration::from_secs(120);
/// Idle time after which the CPU helper is closed.
const CPU_IDLE: Duration = Duration::from_secs(60);
/// Clip handles, unique across both helpers and every source.
static HANDLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn next_handle() -> u64 {
    HANDLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
fn gpu_hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub::new(false, GPU_IDLE))
}
fn cpu_hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub::new(true, CPU_IDLE))
}
/// Lock `slot`, giving up when the caller's work is cancelled instead of waiting behind a start
/// that takes a moment (another frame starting the helper).
fn lock_cancellably<T>(slot: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    let started = Instant::now();
    loop {
        match slot.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if filmcraft_media::cancel::cancelled() {
            return Err(MediaError::Cancelled);
        }
        if started.elapsed() >= TIMEOUT {
            return Err(failure("RED helper start stayed locked for 120 seconds"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
impl Hub {
    fn new(cpu_only: bool, idle: Duration) -> Self {
        start_hub_reaper();
        Self { cpu_only, idle, state: Mutex::new(HubState::default()), warming: Default::default() }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The helper and its generation: the running one, or one started now (again after a
    /// failure, or with larger slots when a clip needs `slot_bytes`). While the GPU helper is
    /// backing off after a failed start, an error without trying.
    fn worker(&self, slot_bytes: usize) -> Result<(Arc<Worker>, u64)> {
        let mut st = lock_cancellably(&self.state)?;
        let running = st.worker.as_ref().filter(|w| w.registry.lock().failed.is_none()).cloned();
        if let Some(worker) = running
            && (self.cpu_only || st.slot_bytes >= slot_bytes)
        {
            st.last_used = Some(Instant::now());
            return Ok((worker, st.generation));
        }
        if !self.cpu_only && st.backoff.until.is_some_and(|until| Instant::now() < until) {
            return Err(failure("RED GPU helper unavailable (backing off after a failed start)"));
        }
        // The old helper (failed, or with slots too small) goes; requests still running on it
        // hold it until they end.
        st.worker = None;
        let want = if self.cpu_only { 0 } else { slot_bytes.max(DEFAULT_SLOT_BYTES).max(st.slot_bytes) };
        match Worker::open_hub(self.cpu_only, want) {
            Ok((worker, info)) => {
                st.generation = st.generation.wrapping_add(1);
                st.slot_bytes = info.slot_bytes;
                st.backoff = GpuBackoff::default();
                st.last_used = Some(Instant::now());
                let worker = Arc::new(worker);
                st.worker = Some(worker.clone());
                if !self.cpu_only {
                    log::info!("R3D NE processing: {}", info.processing);
                }
                Ok((worker, st.generation))
            }
            Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
            Err(e) => {
                if !self.cpu_only {
                    st.backoff.failures = st.backoff.failures.saturating_add(1);
                    let delay = backoff_delay(st.backoff.failures);
                    st.backoff.until = Some(Instant::now() + delay);
                    log::warn!(
                        "R3D NE GPU unavailable: {e}; frames decode on the CPU helper for the next {} s (failed start {})",
                        delay.as_secs(),
                        st.backoff.failures
                    );
                }
                Err(e)
            }
        }
    }
    /// Have the helper running for frames of `slot_bytes` soon, without waiting for it: a running
    /// one counts as used (it is not closed as idle), a missing one is started on a thread of its
    /// own. Still frames decode on the CPU helper, and playback that starts after a scrub must
    /// find the GPU helper ready: started by its first frame, the first half second of playback
    /// was dropped frames (17 of 236 in a measured run).
    fn warm(&'static self, slot_bytes: usize) {
        {
            let Ok(mut st) = self.state.try_lock() else { return }; // starting or busy: nothing to do
            let running = st.worker.as_ref().is_some_and(|w| w.registry.lock().failed.is_none());
            if running && (self.cpu_only || st.slot_bytes >= slot_bytes) {
                st.last_used = Some(Instant::now());
                return;
            }
            if st.backoff.until.is_some_and(|until| Instant::now() < until) {
                return;
            }
        }
        if self.warming.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new().name("red-helper-warm".into()).spawn(move || {
            let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.worker(slot_bytes).map(|_| ())));
            if let Ok(Err(e)) = &started {
                log::debug!("R3D helper warm start: {e}");
            }
            self.warming.store(false, std::sync::atomic::Ordering::Release);
        });
        if spawned.is_err() {
            self.warming.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    /// The GPU helper is backing off after a failed start: frames go to the CPU helper.
    fn unavailable(&self) -> bool {
        self.lock().backoff.until.is_some_and(|until| Instant::now() < until)
    }
    /// What the running helper delivers (see [`Pending::delivery`]); nothing before it runs.
    fn delivery(&self) -> (Option<f64>, bool) {
        // a hint, asked for every frame: never wait for a helper being started under the lock
        let worker = match self.state.try_lock() {
            Ok(st) => st.worker.clone(),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().worker.clone(),
            Err(std::sync::TryLockError::WouldBlock) => return (None, false),
        };
        worker.map_or((None, false), |w| w.registry.lock().delivery())
    }
    /// The clip `handle` opened in helper `generation` is no longer needed: closed there (`C`), if
    /// that helper still runs. Best effort: a helper gone took its clips with it.
    fn close_clip(&self, generation: u64, handle: u64) {
        let worker = {
            let st = self.lock();
            if st.generation != generation {
                return;
            }
            st.worker.clone()
        };
        if let Some(worker) = worker {
            let mut commands = worker.commands.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = commands.write_all(format!("C {handle}\n").as_bytes()).and_then(|_| commands.flush());
        }
    }
    /// Close the helper if nothing has asked it for anything for [`Self::idle`] and nothing is in
    /// flight; the next request starts it again (the sources reopen their clips).
    fn sweep(&self) {
        let closing = {
            let mut st = self.lock();
            let idle = st.last_used.is_none_or(|t| t.elapsed() >= self.idle);
            let quiet = st.worker.as_ref().is_some_and(|w| w.registry.lock().requests.is_empty());
            if !(idle && quiet) {
                return;
            }
            st.worker.take()
        };
        if let Some(worker) = closing {
            log::info!("R3D helper {} ({}) closed: idle for {} s", worker.pid, worker.label, self.idle.as_secs());
            drop(worker);
        }
    }
    /// The running helper, if any (tests).
    #[cfg(test)]
    fn running(&self) -> Option<Arc<Worker>> {
        self.lock().worker.clone()
    }
}
/// The thread that closes idle helpers (started with the first hub; its sweep runs under
/// `catch_unwind`, and a thread that could not be started only means idle helpers stay up).
fn start_hub_reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("red-helper-reaper".into()).spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if std::panic::catch_unwind(|| {
                    gpu_hub().sweep();
                    cpu_hub().sweep();
                })
                .is_err()
                {
                    log::warn!("R3D helper reaper: a sweep panicked; the next runs anyway");
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("R3D helper reaper not started: {e}; idle helpers stay up");
        }
    });
}
/// Open `path` as clip `handle` in `worker` (`O`): the clip's metadata, or the helper's reason.
fn open_in(worker: &Worker, handle: u64, path: &Path) -> Result<Vec<u8>> {
    let path = path.to_str().ok_or_else(|| failure("R3D path is not valid Unicode"))?;
    if path.is_empty() || path.contains(['\n', '\r']) || path.len() > 8192 {
        return Err(failure("unsupported R3D path"));
    }
    worker.request(&format!("O {handle} {path}\n"))?.into_inline()
}

/// The GPU helper failed to become ready (an exit before its start-up reply: CUDA
/// initialisation or the SDK's pool allocation failing on a full GPU, a missing runtime). Frames
/// decode on the CPU helper until `until`, and each failure doubles the wait, from
/// [`BACKOFF_FIRST`] to [`BACKOFF_MAX`]; a start that succeeds clears it. Without it a failed
/// start was retried by the next frame request at once, each try a process on a GPU that had
/// just refused one.
#[derive(Default)]
struct GpuBackoff {
    failures: u32,
    until: Option<Instant>,
}
const BACKOFF_FIRST: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// The wait after the `failures`th consecutive failed start.
fn backoff_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    BACKOFF_FIRST.saturating_mul(1u32 << doublings).min(BACKOFF_MAX)
}

pub struct RedSource {
    info: MediaInfo,
    metadata: Metadata,
    state: Mutex<State>,
    path: PathBuf,
    /// This clip in the GPU helper and in the CPU helper ([`Hub`]): the helper generation it was
    /// opened in and its handle there. Opened on first use; a helper started again since means
    /// the clip is opened again.
    gpu_clip: Mutex<Option<(u64, u64)>>,
    cpu_clip: Mutex<Option<(u64, u64)>>,
    /// Clip ▸ Source Settings…: how the helper develops the picture ([`MediaSource::set_develop`]).
    develop: Mutex<RawDevelop>,
    /// What the clip records (as shot, and the rest): from its metadata, and again after an RMD
    /// sidecar write changed it ([`MediaSource::write_raw_sidecar`]).
    recorded: Mutex<(RawAsShot, RawClipInfo)>,
}

/// The position of `v` in `all` (the request codes of the development fields follow the order of
/// the settings' `ALL` lists, which the helper's tables mirror).
fn code<T: PartialEq + Copy>(all: &[T], v: T) -> usize {
    all.iter().position(|x| *x == v).unwrap_or(0)
}

/// The development fields of a request (FCRD9), after `V <id> <clip> <frame> <divisor>` (with the
/// half-resolution quality first: `half`, 0 premium or 1 good) or after `W <id> <clip>`: colour
/// space and gamma (their place in [`RawColorSpace::ALL`] and [`RawGamma::ALL`]), tone map and
/// rolloff (0 = as recorded), the HDR peak (0 = as recorded), an options word (1 chroma noise
/// reduction set, 2 its value, 4 apply the metadata curves, 8 the creative LUT, 16 the CDL), the
/// flashing pixel adjustment (0 = as recorded), then the RAW adjustments as a flag word (bit 0 ISO,
/// 1 Kelvin, 2 tint, 3 exposure; a clear bit means as shot) followed by the ISO, the Kelvin, the
/// tint in hundredths and the exposure adjustment in hundredths of a stop, each 0 unless its bit
/// is set. The helper checks every value against the SDK's limits and answers anything else with
/// an error; the same ranges are checked here first ([`RawDevelop::validate`]) so a bad project
/// value is an error message, not a helper round trip.
fn wire(d: &RawDevelop, half: Option<bool>) -> Result<String> {
    d.validate().map_err(|e| failure(format!("RAW development setting refused: {e}")))?;
    let options = d.chroma_noise_reduction.map_or(0, |on| 1 | if on { 2 } else { 0 })
        | if d.metadata_curves { 4 } else { 0 }
        | if d.creative_lut { 8 } else { 0 }
        | if d.cdl { 16 } else { 0 };
    let flashing = d.flashing_pixel.map_or(0, |f| code(&RawFlashing::ALL, f) + 1);
    let flags = u8::from(d.iso.is_some()) | u8::from(d.kelvin.is_some()) << 1 | u8::from(d.tint.is_some()) << 2 | u8::from(d.exposure.is_some()) << 3;
    let fields = format!(
        "{} {} {} {} {} {options} {flashing} {flags} {} {} {} {}",
        code(&RawColorSpace::ALL, d.color_space),
        code(&RawGamma::ALL, d.gamma),
        code(&RawToneMap::ALL, d.tone_map),
        code(&RawRollOff::ALL, d.rolloff),
        d.hdr_peak_nits.unwrap_or(0),
        d.iso.unwrap_or(0),
        d.kelvin.unwrap_or(0),
        d.tint.map_or(0, |t| t.0),
        d.exposure.map_or(0, |e| e.0),
    );
    Ok(match half {
        Some(good) => format!("{} {fields}", u8::from(good)),
        None => fields,
    })
}

/// Parse the FCRD9 lines after the as-shot line of a clip's metadata: what else it records
/// (codes as in [`wire`], 0 for a recorded value without one; the HDR peak, flags, track count,
/// the SDK's HDR peak range and default tone map and rolloff, the CDL) and its RMD sidecar path.
/// Hostile numbers from a helper: every one bounded, or the clip is refused.
fn parse_clip_info(line: Option<&str>, path: Option<&str>) -> Result<RawClipInfo> {
    let line = line.ok_or_else(|| failure("RED helper did not report the clip's recorded settings; rebuild the helper"))?;
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() != 28 {
        return Err(failure("invalid RED recorded settings"));
    }
    let n = |i: usize, max: u64| -> Result<u64> {
        f.get(i).and_then(|s| s.parse::<u64>().ok()).filter(|v| *v <= max).ok_or_else(|| failure("invalid RED recorded setting"))
    };
    let pick = |i: usize, max: u64| -> Result<Option<usize>> { Ok(n(i, max)?.checked_sub(1).map(|v| v as usize)) };
    let flag = |i: usize| -> Result<bool> { Ok(n(i, 1)? == 1) };
    let mut cdl = [0f32; 10];
    for (k, out) in cdl.iter_mut().enumerate() {
        *out = f
            .get(18 + k)
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|v| v.is_finite() && v.abs() <= 1000.0)
            .ok_or_else(|| failure("invalid RED recorded CDL"))?;
    }
    let path = path.unwrap_or("");
    if path.len() > 8192 || path.chars().any(char::is_control) {
        return Err(failure("invalid RED RMD path"));
    }
    let (lo, hi, default) = (n(13, 100_000)? as u32, n(14, 100_000)? as u32, n(15, 100_000)? as u32);
    let rmd = flag(12)?;
    Ok(RawClipInfo {
        color_space: pick(0, 4)?.and_then(|i| RawColorSpace::ALL.get(i).copied()),
        gamma: pick(1, 6)?.and_then(|i| RawGamma::ALL.get(i).copied()),
        // tone map and rolloff codes count from None (1): the ALL lists start with "as recorded"
        tone_map: pick(2, 4)?.and_then(|i| RawToneMap::ALL.get(i + 1).copied()),
        rolloff: pick(3, 5)?.and_then(|i| RawRollOff::ALL.get(i + 1).copied()),
        hdr_peak_nits: Some(n(4, 100_000)? as u32).filter(|p| *p > 0),
        chroma_noise_reduction: flag(5)?,
        flashing_pixel: pick(6, 4)?.and_then(|i| RawFlashing::ALL.get(i).copied()).unwrap_or_default(),
        lut: flag(7)?,
        lut_enabled: flag(8)?,
        cdl_enabled: flag(9)?,
        cdl,
        curves: flag(10)?,
        hdrx: n(11, 16)? > 1,
        rmd: rmd.then(|| path.to_string()),
        rmd_path: path.to_string(),
        hdr_peak_range: (lo, hi),
        hdr_peak_default: default,
        tone_map_default: pick(16, 4)?.and_then(|i| RawToneMap::ALL.get(i + 1).copied()),
        rolloff_default: pick(17, 5)?.and_then(|i| RawRollOff::ALL.get(i + 1).copied()),
    })
}

/// What a developed frame holds: linear light (the helper's half floats, or the code values of
/// any other gamma linearised here) in the developed primaries. The compositor's interpretation
/// of a frame whose primaries are not Rec. 709 comes from the clip's development
/// (`Project::color_space_override`), since container metadata cannot name them all.
fn frame_color(d: &RawDevelop) -> ColorInfo {
    let primaries = match d.color_space {
        RawColorSpace::Rec2020 => filmcraft_color::Primaries::Bt2020,
        RawColorSpace::P3D65 => filmcraft_color::Primaries::P3D65,
        RawColorSpace::Rec709 | RawColorSpace::RedWideGamutRgb => filmcraft_color::Primaries::Bt709,
    };
    ColorInfo { primaries, ..linear_color() }
}

/// Code value → linear light for each of the helper's 65 536 16-bit code values of `gamma`, as
/// IEEE halves: the compositor's own curve ([`filmcraft_color::transform::decode_channel`], full
/// range), so its interpretation of the clip and these frames agree exactly (mid grey encoded by
/// the curve comes back as mid grey). Built once per curve.
fn linearise_table(gamma: RawGamma) -> &'static [u16] {
    static TABLES: [OnceLock<Vec<u16>>; 6] = [const { OnceLock::new() }; 6];
    let slot = code(&RawGamma::ALL, gamma);
    let table = TABLES.get(slot).unwrap_or(&TABLES[0]);
    table.get_or_init(|| {
        (0..=u16::MAX)
            .map(|c| {
                let v = f64::from(c) / 65535.0;
                let l = filmcraft_color::transform::decode_channel(v, gamma.curve(), filmcraft_color::Range::Full);
                filmcraft_frame::f32_to_half(if l.is_finite() { l as f32 } else { 0.0 })
            })
            .collect()
    })
}

impl RedSource {
    /// Open `path`: its metadata through the CPU helper (the clip is opened there, ~6 ms, and
    /// stays open for the source's audio and thumbnails); no GPU work until a frame is asked for.
    fn open(name: &str, path: &Path, len: u64) -> Result<Self> {
        let (worker, generation) = cpu_hub().worker(0)?;
        let handle = next_handle();
        let bytes = open_in(&worker, handle, path)?;
        let m = match Metadata::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                cpu_hub().close_clip(generation, handle);
                return Err(e);
            }
        };
        let audio_streams = if m.channels == 0 {
            vec![]
        } else {
            vec![AudioStreamInfo {
                sample_rate: m.sample_rate,
                channels: m.channels as u32,
                codec: if m.float_audio { "pcm_f32be" } else { "pcm_s24be" }.into(),
                bits_per_sample: Some(m.bits),
            }]
        };
        let info = MediaInfo {
            name: name.into(),
            kind: MediaKind::Movie,
            duration: m.duration,
            video: Some(VideoStreamInfo {
                width: m.width,
                height: m.height,
                frame_rate: m.rate,
                par: m.par,
                codec: "R3D NE".into(),
                pixel_format: "RAW 12-bit → linear RGB float".into(),
                color: linear_color(),
                has_alpha: false,
                bitrate: None,
                hdr: None,
            }),
            audio_streams,
            container: "R3D NE".into(),
            start_timecode: m.timecode,
            file_size: Some(len),
        };
        let recorded = Mutex::new((m.as_shot, m.clip.clone()));
        Ok(RedSource {
            info,
            metadata: m,
            state: Mutex::new(State { last: None }),
            path: path.into(),
            gpu_clip: Mutex::new(None),
            recorded,
            cpu_clip: Mutex::new(Some((generation, handle))),
            develop: Mutex::new(RawDevelop::default()),
        })
    }
    /// This clip in `hub` (its slot in this source is `mine`): the helper and the clip's handle
    /// there, opened now when it is not yet (or the helper was started again since). The
    /// source's slot stays locked while the clip opens, so two frames of one source open it once.
    fn clip_on(&self, hub: &Hub, mine: &Mutex<Option<(u64, u64)>>) -> Result<(Arc<Worker>, u64)> {
        let mut mine = lock_cancellably(mine)?;
        let m = &self.metadata;
        let frame_bytes = (m.width as usize).saturating_mul(m.height as usize).saturating_mul(6);
        let (worker, generation) = hub.worker(frame_bytes)?;
        if let Some((opened_in, handle)) = *mine
            && opened_in == generation
        {
            return Ok((worker, handle));
        }
        let handle = next_handle();
        let bytes = open_in(&worker, handle, &self.path)?;
        let opened = Metadata::parse(&bytes);
        let same = opened.as_ref().is_ok_and(|o| (o.width, o.height, o.frames) == (m.width, m.height, m.frames));
        if !same {
            hub.close_clip(generation, handle);
            return Err(opened.err().unwrap_or_else(|| failure("R3D clip changed on disk; reopen it")));
        }
        *mine = Some((generation, handle));
        Ok((worker, handle))
    }
}
impl Drop for RedSource {
    fn drop(&mut self) {
        for (hub, mine) in [(gpu_hub(), &self.gpu_clip), (cpu_hub(), &self.cpu_clip)] {
            if let Some((generation, handle)) = *mine.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
                hub.close_clip(generation, handle);
            }
        }
    }
}

pub fn reader_opener(name: &str, _: &[u8], reader: &SharedReader) -> Option<Result<SharedSource>> {
    if !filmcraft_codecs::r3d::is_r3d(name) {
        return None;
    }
    Some((|| {
        let path = reader
            .local_path()
            .ok_or_else(|| MediaError::Unsupported("RED SDK requires a local R3D file; browser and memory sources are not supported".into()))?;
        Ok(Arc::new(RedSource::open(name, path, reader.len())?) as SharedSource)
    })())
}

fn linear_color() -> ColorInfo {
    ColorInfo { transfer: Transfer::Linear, ..ColorInfo::SRGB_FULL }
}

/// The frame the SDK writes for a reduced decode: both dimensions rounded down, rows packed at
/// that width (SDK 9.2.1, measured on 6048×3402 for half, quarter, eighth and sixteenth; see
/// `docs/r3d-ne.md`). `None` for a divisor that is not one of the SDK's or that leaves no pixel.
/// The helper sizes its buffer by the same rule; `video_texels` and `video_linear` reject any
/// other byte count.
fn reduced_size(width: u32, height: u32, divisor: u32) -> Option<(u32, u32)> {
    if ![1, 2, 4, 8, 16].contains(&divisor) {
        return None;
    }
    let (w, h) = (width.checked_div(divisor)?, height.checked_div(divisor)?);
    (w > 0 && h > 0).then_some((w, h))
}

/// The smallest divisor a still frame is decoded at by the CPU helper rather than the GPU one.
/// One frame through the GPU decoder takes ~160 ms at any size on a current desktop GPU (its pipeline is
/// built for throughput); the CPU decoder answers a quarter in 50-95 ms, an eighth in 30-35 ms
/// and a sixteenth in 15-22 ms, but a half in 0.6-1.3 s (6K and 4K Nikon ZR clips on a desktop CPU,
/// `real_random_frame_latency_on_each_helper`). `FILMCRAFT_R3D_STILL_CPU_DIVISOR` overrides it
/// (0: never).
fn still_cpu_divisor() -> u32 {
    static DIVISOR: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *DIVISOR.get_or_init(|| {
        std::env::var("FILMCRAFT_R3D_STILL_CPU_DIVISOR")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|d| [0, 1, 2, 4, 8, 16].contains(d))
            .unwrap_or(4)
    })
}

/// Whether a frame at `divisor` goes to the CPU helper for its latency: a still frame at the CPU
/// divisor or smaller.
fn still_on_cpu(still: bool, divisor: u32) -> bool {
    let from = still_cpu_divisor();
    still && from != 0 && divisor >= from
}

/// The divisor a preview at `scale` decodes at under a decode quality whose smallest divisor is
/// `cap` ([`filmcraft_media::RawQuality::divisor`]): the covering one ([`preview_divisor`]) or
/// the quality's, whichever is smaller, falling back to the next larger decode for a clip too
/// small for it.
fn capped_divisor(width: u32, height: u32, scale: f32, cap: u32) -> u32 {
    let mut d = preview_divisor(width, height, scale).max(cap).min(16);
    while d > 1 && reduced_size(width, height, d).is_none() {
        d /= 2;
    }
    d
}

fn preview_divisor(width: u32, height: u32, scale: f32) -> u32 {
    // The smallest decode that still covers the requested scale (an odd dimension loses up to
    // one pixel row or column to the SDK's rounding, which a preview can spare; full requests
    // never reduce).
    [16, 8, 4, 2].into_iter().find(|d| scale <= 1.0 / *d as f32 && reduced_size(width, height, *d).is_some()).unwrap_or(1)
}

/// The helper's RGB halves (native little endian on x64, 6 bytes a pixel) as RGBA halves with
/// alpha 1.0: the frame's [`filmcraft_frame::PixelData::RgbaF16`] layout, which the GPU uploads
/// as it is. A copy with two bytes inserted per pixel (a 6K half-resolution frame in a few
/// milliseconds), not a 32-bit image (82 MB written, then converted back to halves for the
/// upload, ~120 ms of a frame worker). Infinities and NaNs (an exponent field of all ones) are
/// rejected, as the f32 conversion did.
fn video_texels(bytes: &[u8], pixels: usize) -> Result<Vec<u8>> {
    if pixels > MAX_PIXELS || bytes.len() != pixels * 6 {
        return Err(failure("RED video buffer size mismatch"));
    }
    let len = pixels * 8;
    let mut rgba = filmcraft_frame::pool::take_u8(len);
    rgba.try_reserve_exact(len).map_err(|_| failure("RED frame allocation failed"))?;
    rgba.resize(len, 0);
    let mut non_finite = false;
    for (rgb, texel) in bytes.as_chunks::<6>().0.iter().zip(rgba.as_chunks_mut::<8>().0.iter_mut()) {
        texel[..6].copy_from_slice(rgb);
        texel[6] = 0x00;
        texel[7] = 0x3c; // 1.0
        non_finite |= (rgb[1] & 0x7c == 0x7c) | (rgb[3] & 0x7c == 0x7c) | (rgb[5] & 0x7c == 0x7c);
    }
    if non_finite {
        filmcraft_frame::pool::recycle_u8(rgba);
        return Err(failure("non-finite RED video sample"));
    }
    Ok(rgba)
}
/// The helper's 16-bit integer RGB code values (every gamma but linear, 6 bytes a pixel, read in
/// place from the slot) linearised through `table` ([`linearise_table`]) into RGBA halves with
/// alpha 1.0: the frame's [`filmcraft_frame::PixelData::RgbaF16`] layout, linear light as every
/// float frame is, which the GPU uploads as it is. A NaN half is a plain code value here: nothing
/// is rejected but a wrong size (or a table that is not one per code value).
fn video_linear(bytes: &[u8], pixels: usize, table: &[u16]) -> Result<Vec<u8>> {
    if pixels > MAX_PIXELS || bytes.len() != pixels * 6 || table.len() != 65536 {
        return Err(failure("RED video buffer size mismatch"));
    }
    let len = pixels * 8;
    let mut rgba = filmcraft_frame::pool::take_u8(len);
    rgba.try_reserve_exact(len).map_err(|_| failure("RED frame allocation failed"))?;
    rgba.resize(len, 0);
    for (rgb, texel) in bytes.as_chunks::<6>().0.iter().zip(rgba.as_chunks_mut::<8>().0.iter_mut()) {
        for (value, output) in rgb.as_chunks::<2>().0.iter().zip(texel.as_chunks_mut::<2>().0.iter_mut()) {
            let h = table.get(usize::from(u16::from_le_bytes(*value))).copied().unwrap_or(0);
            *output = h.to_le_bytes();
        }
        texel[6] = 0x00;
        texel[7] = 0x3c; // 1.0
    }
    Ok(rgba)
}
fn audio_samples(bytes: &[u8], channels: usize, requested: usize, floating: bool) -> Result<Vec<Vec<f32>>> {
    if channels == 0 || channels > 16 || requested > MAX_SAMPLES || !bytes.len().is_multiple_of(channels * 4) || bytes.len() / (channels * 4) > requested {
        return Err(failure("RED audio buffer size mismatch"));
    }
    let mut out = vec![Vec::with_capacity(bytes.len() / (channels * 4)); channels];
    for frame in bytes.chunks_exact(channels * 4) {
        for (c, word) in frame.as_chunks::<4>().0.iter().enumerate() {
            let bits = [word[0], word[1], word[2], word[3]];
            let v = if floating { f32::from_be_bytes(bits) } else { i32::from_be_bytes(bits) as f32 / 2147483648.0 };
            if !v.is_finite() {
                return Err(failure("non-finite RED audio sample"));
            }
            out[c].push(v);
        }
    }
    Ok(out)
}

impl MediaSource for RedSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    /// Every R3D frame decodes on its own, and the helper's reply timings say how many per
    /// second it manages: playback asks for the frames it can finish, not for all of them.
    fn frame_delivery(&self) -> filmcraft_media::FrameDelivery {
        // The GPU helper serves every clip: its reading is what any of them gets. Not started yet
        // (or closed while idle), it has none; the first frames start it.
        let (rate, fresh) = gpu_hub().delivery();
        filmcraft_media::FrameDelivery { independent: true, rate, fresh }
    }
    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        if filmcraft_media::cancel::cancelled() {
            return Err(MediaError::Cancelled);
        }
        if !req.scale.is_finite() || req.scale <= 0.0 {
            return Err(failure("invalid R3D frame scale"));
        }
        let m = &self.metadata;
        let index = m.rate.frame_at(req.time).clamp(0, m.frames - 1);
        let develop = *self.develop.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let quality = develop.quality();
        // a full-size request (an export, a monitor at 100 %) always decodes at full resolution;
        // a preview at the smallest size that covers it, never larger than the decode quality
        let divisor = if req.scale >= 1.0 { 1 } else { capped_divisor(m.width, m.height, req.scale, quality.divisor()) };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((previous, previous_divisor, previous_develop, frame)) = &state.last
            && *previous == index
            && *previous_divisor == divisor
            && *previous_develop == develop
        {
            return Ok(frame.clone());
        }
        state.last = None; // Release the previous large frame before decoding another.
        drop(state);
        let develop_fields = wire(&develop, Some(divisor == 2 && quality.half_good()))?;
        // Background work (a thumbnail) never touches the GPU: the CPU helper decodes it at the
        // request's divisor. So does every frame while the GPU helper is unavailable, the frame
        // whose GPU start just failed (the back-off starts with it), and a still frame (a scrub,
        // a paused monitor) small enough for the CPU decoder to answer first (`still_on_cpu`).
        let on_cpu = filmcraft_media::cancel::background() || still_on_cpu(filmcraft_media::cancel::still(), divisor) || gpu_hub().unavailable();
        if on_cpu && !filmcraft_media::cancel::background() {
            let frame_bytes = (m.width as usize).saturating_mul(m.height as usize).saturating_mul(6);
            gpu_hub().warm(frame_bytes);
        }
        // The reply, or the error and whether the helper itself went (killed, crashed) under it.
        let decode = || -> std::result::Result<Reply, (MediaError, bool)> {
            let (worker, handle) = if on_cpu {
                self.clip_on(cpu_hub(), &self.cpu_clip).map_err(|e| (e, false))?
            } else {
                match self.clip_on(gpu_hub(), &self.gpu_clip) {
                    Ok(clip) => clip,
                    Err(MediaError::Cancelled) => return Err((MediaError::Cancelled, false)),
                    Err(_) => self.clip_on(cpu_hub(), &self.cpu_clip).map_err(|e| (e, false))?,
                }
            };
            worker.request(&format!("V {handle} {index} {divisor} {develop_fields}\n")).map_err(|e| {
                let gone = !matches!(e, MediaError::Cancelled) && worker.registry.lock().failed.is_some();
                (e, gone)
            })
        };
        // A frame in flight when its helper died is asked again of the helper started in its place
        // (once). Before, the frames in flight failed and the monitor showed the damaged-media slate
        // for a moment after a helper crash (a GPU helper killed mid-play).
        let reply = match decode() {
            Err((_, true)) if !filmcraft_media::cancel::cancelled() => {
                log::info!("R3D frame {index}: its helper went; asking the helper started in its place");
                decode().map_err(|(e, _)| e)?
            }
            r => r.map_err(|(e, _)| e)?,
        };
        if filmcraft_media::cancel::cancelled() {
            return Err(MediaError::Cancelled); // dropping a slot reply hands the slot back
        }
        let (width, height) = reduced_size(m.width, m.height, divisor).ok_or_else(|| failure("invalid R3D preview divisor"))?;
        let pixels = width as usize * height as usize;
        let t0 = Instant::now();
        // Repacked straight out of the helper's slot: the one copy of the frame in this process.
        // The linear output is half float, repacked as it is; every other gamma comes as 16-bit
        // code values, linearised into halves (linear light keeps 11 significant bits of each).
        let bytes = match &reply {
            Reply::Inline(bytes) => bytes.as_slice(),
            Reply::Slot(lease) => lease.bytes()?,
            Reply::Error(message) => return Err(failure(format!("RED decoder: {message}"))),
        };
        let image = if develop.gamma == RawGamma::Linear {
            VideoFrame::rgba_f16(width, height, video_texels(bytes, pixels)?)
        } else {
            VideoFrame::rgba_f16(width, height, video_linear(bytes, pixels, linearise_table(develop.gamma))?)
        };
        if timing() {
            let held = match &reply {
                Reply::Slot(lease) => format!(", slot {} held {:.1} ms", lease.index, lease.delivered.elapsed().as_secs_f64() * 1e3),
                Reply::Inline(_) | Reply::Error(_) => String::new(),
            };
            log::info!("R3D frame {index}/{divisor}: convert {:.1} ms{held}", t0.elapsed().as_secs_f64() * 1e3);
        }
        match reply {
            Reply::Inline(bytes) => filmcraft_frame::pool::recycle_u8(bytes),
            Reply::Slot(lease) => drop(lease), // `F <slot>`: the helper may decode into it again
            Reply::Error(_) => {}
        }
        let mut frame = image.with_pts(m.rate.tick_of(index));
        frame.color = frame_color(&develop);
        let frame = Arc::new(frame);
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last = Some((index, divisor, develop, frame.clone()));
        Ok(frame)
    }
    fn raw_as_shot(&self) -> Option<RawAsShot> {
        Some(self.recorded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0)
    }
    fn raw_clip_info(&self) -> Option<RawClipInfo> {
        Some(self.recorded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1.clone())
    }
    /// Update RMD: `W` to the CPU helper, which writes the sidecar beside the clip
    /// (`CreateOrUpdateRmd`) and answers the clip's metadata as it now reads, RMD included. The
    /// GPU helper's copy of the clip still has the old recorded settings: it is closed there, so
    /// the next frame opens it again with the sidecar.
    fn write_raw_sidecar(&self, develop: &RawDevelop) -> Result<String> {
        let fields = wire(develop, None)?;
        let (worker, handle) = self.clip_on(cpu_hub(), &self.cpu_clip)?;
        let bytes = worker
            .request(&format!(
                "W {handle} {fields}
"
            ))?
            .into_inline()?;
        let m = Metadata::parse(&bytes)?;
        let path = m.clip.rmd.clone().ok_or_else(|| failure("the RED SDK wrote no RMD sidecar"))?;
        *self.recorded.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (m.as_shot, m.clip);
        if let Some((generation, handle)) = self.gpu_clip.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            gpu_hub().close_clip(generation, handle);
        }
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last = None;
        Ok(path)
    }
    /// The next frames are developed with `develop` (the helper takes the settings per request,
    /// so nothing is restarted); the cached frame is dropped.
    fn set_develop(&self, develop: &RawDevelop) -> Result<()> {
        let mut current = self.develop.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != *develop {
            *current = *develop;
            self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last = None;
        }
        Ok(())
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        let m = &self.metadata;
        if m.channels == 0 {
            return Err(MediaError::NoStream("audio"));
        }
        if frames > MAX_SAMPLES || sample_rate == 0 || sample_rate > 384_000 {
            return Err(failure("R3D audio request outside supported limits"));
        }
        if filmcraft_media::cancel::cancelled() {
            return Err(MediaError::Cancelled);
        }
        let end = start.checked_add(frames as i64).ok_or_else(|| failure("R3D audio position overflow"))?;
        let pos = |sample: i64| sample as i128 * m.sample_rate as i128;
        let first = pos(start).div_euclid(sample_rate as i128).max(0).min(m.samples as i128) as u64;
        let last = (pos(end).div_euclid(sample_rate as i128) + 2).max(0).min(m.samples as i128) as u64;
        let count = last.saturating_sub(first);
        if count > MAX_SAMPLES as u64 {
            return Err(failure("R3D resampling request too large"));
        }
        let mut output = AudioBuffer::silence(sample_rate, m.channels, frames);
        if frames == 0 || count == 0 {
            return Ok(output);
        }
        // The CPU helper decodes audio before any queued frame, so sound never waits on video.
        let (worker, handle) = self.clip_on(cpu_hub(), &self.cpu_clip)?;
        let bytes = worker.request(&format!("A {handle} {first} {count}\n"))?.into_inline()?;
        let samples = audio_samples(&bytes, m.channels, count as usize, m.float_audio)?;
        for i in 0..frames {
            let p = pos(start + i as i64);
            let absolute = p.div_euclid(sample_rate as i128);
            if absolute < first as i128 || absolute >= m.samples as i128 {
                continue;
            }
            let local = (absolute - first as i128) as usize;
            let fraction = p.rem_euclid(sample_rate as i128) as f32 / sample_rate as f32;
            for (channel, src) in samples.iter().enumerate() {
                let a = src.get(local).copied().ok_or_else(|| failure("RED decoder returned too few audio samples"))?;
                let b = src.get(local + 1).copied().unwrap_or(a);
                // Use f64 for interpolation so finite large float PCM cannot overflow b - a.
                let value = (a as f64 * (1.0 - fraction as f64) + b as f64 * fraction as f64) as f32;
                if !value.is_finite() {
                    return Err(failure("non-finite resampled RED audio"));
                }
                output.channels[channel][i] = value;
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Set: every GPU helper start fails as a helper whose CUDA decoder could not be created
    /// (the process is never started). For the back-off test.
    pub(super) static FAIL_GPU_STARTS: AtomicBool = AtomicBool::new(false);

    /// Helper processes alive on this machine right now, from the process list (not this
    /// process's own count): what the GPU actually sees.
    fn helper_processes() -> usize {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq filmcraft-red-helper.exe", "/FO", "CSV", "/NH"]).output().unwrap();
            String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("filmcraft-red-helper.exe")).count()
        }
        #[cfg(not(windows))]
        {
            // `comm` is the executable name cut to fifteen characters.
            std::fs::read_dir("/proc")
                .unwrap()
                .flatten()
                .filter(|entry| std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|comm| comm.trim_end() == "filmcraft-red-h"))
                .count()
        }
    }
    /// Wait for the helper processes to be gone (a close reaps synchronously, but the process
    /// list lags a little on Windows).
    fn wait_for_no_helpers() {
        let started = Instant::now();
        while helper_processes() > 0 && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(helper_processes(), 0, "helper processes left behind");
    }
    /// Sample the process list every few milliseconds until `stop`, keeping the peak.
    struct Sampler {
        stop: AtomicBool,
        processes: AtomicUsize,
        samples: AtomicUsize,
    }
    impl Sampler {
        fn new() -> Self {
            Self { stop: AtomicBool::new(false), processes: AtomicUsize::new(0), samples: AtomicUsize::new(0) }
        }
        fn run(&self) {
            while !self.stop.load(Ordering::Relaxed) {
                self.processes.fetch_max(helper_processes(), Ordering::Relaxed);
                self.samples.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    /// A GPU helper of its own (not the shared one) with the test clip open in it: the worker,
    /// the clip's handle, its metadata and the helper's start-up reply.
    fn gpu_helper_with_clip(path: &Path) -> (Worker, u64, Metadata, HubInfo) {
        let (worker, hub) = Worker::open_hub(false, DEFAULT_SLOT_BYTES).unwrap();
        let handle = next_handle();
        let m = Metadata::parse(&open_in(&worker, handle, path).unwrap()).unwrap();
        (worker, handle, m, hub)
    }
    /// Close both helpers and wait for their processes to be gone: a clean slate (a source's
    /// clips are opened again by its next request).
    fn close_hubs() {
        for hub in [gpu_hub(), cpu_hub()] {
            let worker = hub.lock().worker.take();
            drop(worker);
        }
        wait_for_no_helpers();
    }
    /// `count` sources opened on the test clip: open in the CPU helper (their metadata came from
    /// it), not yet in the GPU one.
    fn open_sources(path: &Path, count: usize) -> Vec<RedSource> {
        let len = std::fs::metadata(path).unwrap().len();
        let sources: Vec<_> = (0..count).map(|_| RedSource::open("test.R3D", path, len).unwrap()).collect();
        for source in &sources {
            assert!(source.gpu_clip.lock().unwrap().is_none() && source.cpu_clip.lock().unwrap().is_some());
        }
        sources
    }
    /// Every source asked for `request` at once from `threads` threads (each takes the next
    /// source off a shared queue); the results in source order.
    fn burst<T: Send>(sources: &[RedSource], threads: usize, request: impl Fn(&RedSource) -> T + Sync) -> Vec<T> {
        let queue = Mutex::new((0..sources.len()).collect::<VecDeque<_>>());
        let results = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..threads {
                let (queue, results, request) = (&queue, &results, &request);
                scope.spawn(move || {
                    loop {
                        // taken under a lock released before the request (a `while let` guard
                        // would live through the body and serialise the threads)
                        let next = queue.lock().unwrap().pop_front();
                        let Some(i) = next else { break };
                        let result = request(&sources[i]);
                        results.lock().unwrap().push((i, result));
                    }
                });
            }
        });
        let mut results = results.into_inner().unwrap();
        results.sort_by_key(|(i, _)| *i);
        results.into_iter().map(|(_, result)| result).collect()
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage; run alone (--test-threads=1)"]
    fn real_two_helpers_serve_every_source() {
        // Regression: a helper per source and per role ran over a hundred processes for a
        // hundred-clip timeline. Now one GPU and one CPU helper serve every clip.
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let mut sources = open_sources(&path, 24);
        assert_eq!(helper_processes(), 1, "opening 24 sources probed them through the one CPU helper");
        assert!(gpu_hub().running().is_none(), "no GPU work before a frame is asked for");
        let (w, h) = (sources[0].metadata.width, sources[0].metadata.height);
        let quarter = preview_divisor(w, h, 0.25);
        // A first frame from every source at once, and their audio at once: two processes.
        let sampler = Sampler::new();
        let (frames, sounds) = std::thread::scope(|scope| {
            let sampler = &sampler;
            scope.spawn(move || sampler.run());
            let frames = burst(&sources, 8, |source| source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).map(|f| (f.width, f.height)));
            let sounds = burst(&sources, 8, |source| source.audio(0, 4800, 48000).map(|a| a.channels.len()));
            sampler.stop.store(true, Ordering::Relaxed);
            (frames, sounds)
        });
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.as_ref().map_err(|e| e.to_string()), Ok(&(w / quarter, h / quarter)), "source {i}");
        }
        for (i, sound) in sounds.iter().enumerate() {
            assert_eq!(sound.as_ref().map_err(|e| e.to_string()), Ok(&sources[0].metadata.channels), "source {i}");
        }
        let peak = sampler.processes.load(Ordering::Relaxed);
        println!(
            "24 sources, a frame and audio from each at once on 8 threads: peak {peak} helper processes over {} samples",
            sampler.samples.load(Ordering::Relaxed)
        );
        assert!(peak <= 2, "{peak} helper processes");
        let generation = gpu_hub().lock().generation;
        assert!(sources.iter().all(|s| s.gpu_clip.lock().unwrap().is_some_and(|(g, _)| g == generation)), "every clip is open in the one GPU helper");
        // A source that goes closes its clips; the helpers stay for the others.
        drop(sources.pop());
        assert_eq!(helper_processes(), 2);
        sources[0].video_frame(FrameRequest { time: sources[0].metadata.rate.frame_duration(), scale: 0.25 }).unwrap();
        // A helper that dies is started again by the next request, which opens its clip again.
        let old = gpu_hub().running().unwrap();
        {
            let mut child = old.child.lock().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let started = Instant::now();
        while old.registry.lock().failed.is_none() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(old);
        let again = sources[1].video_frame(FrameRequest { time: sources[1].metadata.rate.tick_of(2), scale: 0.25 }).unwrap();
        assert_eq!((again.width, again.height), (w / quarter, h / quarter));
        assert_eq!(gpu_hub().lock().generation, generation.wrapping_add(1), "a new GPU helper");
        assert!(sources[1].gpu_clip.lock().unwrap().is_some_and(|(g, _)| g == generation.wrapping_add(1)));
        // A helper idle for its idle time is closed; the next request starts it again.
        cpu_hub().lock().last_used = Some(Instant::now() - CPU_IDLE);
        cpu_hub().sweep();
        assert!(cpu_hub().running().is_none(), "the idle CPU helper was not closed");
        assert!(sources[2].audio(0, 4800, 48000).is_ok());
        assert!(cpu_hub().running().is_some());
        drop(sources);
        close_hubs();
    }
    #[test]
    #[ignore = "benchmark: requires RED_R3D_TEST_CLIP; run alone (--test-threads=1 --nocapture)"]
    fn real_random_frame_latency_on_each_helper() {
        // A paused monitor asks for one frame at a time, anywhere in the clip (a scrub, a click on
        // the ruler): its latency, not the helpers' throughput, is what the user waits for.
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let source = open_sources(&path, 1).remove(0);
        let frames = source.metadata.frames.max(1);
        source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.125 }).unwrap(); // start the GPU helper
        filmcraft_media::cancel::with_background(true, || source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.0625 }).unwrap());
        for scale in [1.0f32, 0.5, 0.25, 0.125, 0.0625] {
            for (name, background) in [("gpu", false), ("cpu", true)] {
                let mut ms: Vec<f64> = (0..10i64)
                    .map(|k| {
                        let index = (k * 7919 + i64::from(scale.to_bits() % 97)) % frames;
                        let t0 = Instant::now();
                        filmcraft_media::cancel::with_background(background, || {
                            source.video_frame(FrameRequest { time: source.metadata.rate.tick_of(index), scale }).unwrap()
                        });
                        t0.elapsed().as_secs_f64() * 1e3
                    })
                    .collect();
                ms.sort_by(f64::total_cmp);
                println!("scale {scale:>6}: {name} p50 {:6.1} ms  max {:6.1} ms", ms[ms.len() / 2], ms[ms.len() - 1]);
            }
        }
        // the two decoders develop the same picture (a still frame from one, playback from the other)
        for scale in [0.25f32, 0.125] {
            let at = source.metadata.rate.tick_of(frames / 2);
            let gpu = source.video_frame(FrameRequest { time: at, scale }).unwrap();
            source.state.lock().unwrap().last = None;
            let cpu = filmcraft_media::cancel::with_still(true, || source.video_frame(FrameRequest { time: at, scale }).unwrap());
            let (a, b) = (gpu.to_rgba8(), cpu.to_rgba8());
            assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
            let diff = a.iter().zip(&b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len().max(1) as f64;
            println!("scale {scale}: mean |gpu - cpu| {diff:.2} of 255");
            assert!(diff < 2.0, "the CPU and GPU decoders differ by {diff} of 255 on average at {scale}");
        }
        drop(source);
        close_hubs();
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage; run alone (--test-threads=1)"]
    fn real_a_still_frame_decodes_on_the_cpu_and_warms_the_gpu_for_playback() {
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let source = open_sources(&path, 1).remove(0);
        let t0 = Instant::now();
        let still = filmcraft_media::cancel::with_still(true, || source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.125 }).unwrap());
        let took = t0.elapsed();
        assert_eq!(still.width, source.metadata.width / 8);
        assert!(source.gpu_clip.lock().unwrap().is_none(), "the still frame came from the CPU helper");
        println!("still frame at an eighth in {:.1} ms", took.as_secs_f64() * 1e3);
        // the GPU helper starts meanwhile, so playback finds it running
        let started = Instant::now();
        while gpu_hub().running().is_none() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gpu_hub().running().is_some(), "a still frame warms the GPU helper");
        let generation = gpu_hub().lock().generation;
        source.video_frame(FrameRequest { time: source.metadata.rate.tick_of(1), scale: 0.125 }).unwrap();
        assert_eq!(gpu_hub().lock().generation, generation, "playback used the warmed helper");
        drop(source);
        close_hubs();
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage; run alone (--test-threads=1)"]
    fn real_frames_in_flight_when_the_gpu_helper_dies_still_arrive() {
        // Regression: a GPU helper killed mid-play failed the frames in flight, three of them in a
        // row, and the monitor showed the damaged-media slate until the new helper delivered.
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let source = open_sources(&path, 1).remove(0);
        let frames = source.metadata.frames.max(1);
        source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.5 }).unwrap();
        for round in 0..3i64 {
            let old = gpu_hub().running().expect("the GPU helper runs");
            let results = std::thread::scope(|scope| {
                let asks: Vec<_> = (0..6i64)
                    .map(|k| {
                        let source = &source;
                        scope.spawn(move || {
                            let index = (round * 31 + k * 7 + 1) % frames;
                            source.video_frame(FrameRequest { time: source.metadata.rate.tick_of(index), scale: 0.5 }).map(|f| f.width)
                        })
                    })
                    .collect();
                std::thread::sleep(Duration::from_millis(30)); // the requests are on their way
                {
                    let mut child = old.child.lock().unwrap();
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
                asks.into_iter().map(|a| a.join().unwrap()).collect::<Vec<_>>()
            });
            for (k, r) in results.iter().enumerate() {
                assert_eq!(r.as_ref().map_err(|e| e.to_string()), Ok(&(source.metadata.width / 2)), "round {round}, frame {k}");
            }
            drop(old);
        }
        drop(source);
        close_hubs();
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage; run alone (--test-threads=1)"]
    fn real_an_idle_helper_closes_by_itself_with_exit_code_0() {
        // Regression: every close, idle reaps included, killed the helper ("exit code: 1" in every
        // log), so the closes of helpers that really failed did not stand out
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let source = open_sources(&path, 1).remove(0);
        assert!(source.audio(0, 4800, 48000).is_ok());
        drop(source);
        LAST_CLOSE_SUCCESS.store(false, Ordering::Relaxed);
        cpu_hub().lock().last_used = Some(Instant::now() - CPU_IDLE);
        cpu_hub().sweep();
        assert!(cpu_hub().running().is_none(), "the idle CPU helper was closed");
        assert!(LAST_CLOSE_SUCCESS.load(Ordering::Relaxed), "it exited by itself, with success");
        close_hubs();
    }

    #[test]
    fn still_frames_go_to_the_cpu_helper_from_a_quarter_down() {
        assert!(!still_on_cpu(false, 16), "playback stays on the GPU");
        assert!(!still_on_cpu(true, 1) && !still_on_cpu(true, 2), "a half or full still frame is quicker on the GPU");
        assert!(still_on_cpu(true, 4) && still_on_cpu(true, 8) && still_on_cpu(true, 16));
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage; run alone (--test-threads=1)"]
    fn real_thumbnails_use_the_cpu_helper_and_a_failed_gpu_start_backs_off() {
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        close_hubs();
        let sources = open_sources(&path, 24);
        let (w, h) = (sources[0].metadata.width, sources[0].metadata.height);
        // the smallest decode the SDK offers (a sixteenth: 6048x3402 rounds down to 378x212)
        let divisor = preview_divisor(w, h, 0.05);
        println!("thumbnails decode at 1/{divisor}");
        assert_eq!(divisor, 16, "thumbnails of a clip that does not divide exactly still decode at a sixteenth");
        // Thumbnails of every source at once: the CPU helper only, never the GPU.
        let sampler = Sampler::new();
        let results = std::thread::scope(|scope| {
            let sampler = &sampler;
            scope.spawn(move || sampler.run());
            let results = burst(&sources, 8, |source| {
                filmcraft_media::cancel::with_background(true, || {
                    source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.05 }).map(|f| (f.width, f.height))
                })
            });
            sampler.stop.store(true, Ordering::Relaxed);
            results
        });
        for (i, result) in results.iter().enumerate() {
            assert_eq!(result.as_ref().map_err(|e| e.to_string()), Ok(&(w / divisor, h / divisor)), "source {i}");
        }
        let processes = sampler.processes.load(Ordering::Relaxed);
        println!("{} thumbnails on 8 threads: peak {processes} helper processes", sources.len());
        assert_eq!(processes, 1, "thumbnails ran on more than the CPU helper");
        assert!(gpu_hub().running().is_none(), "a thumbnail started the GPU helper");
        // A GPU helper that fails to become ready: the frame falls back to the CPU helper, the GPU
        // backs off (a WARN line names the reason), and the next frames stay on the CPU until the
        // back-off ends; a start that succeeds clears it.
        let source = &sources[0];
        FAIL_GPU_STARTS.store(true, Ordering::Relaxed);
        let fallback = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap();
        FAIL_GPU_STARTS.store(false, Ordering::Relaxed);
        let quarter = preview_divisor(w, h, 0.25);
        assert_eq!((fallback.width, fallback.height), (w / quarter, h / quarter));
        assert!(gpu_hub().running().is_none());
        assert!(gpu_hub().unavailable());
        assert_eq!(gpu_hub().lock().backoff.failures, 1);
        source.video_frame(FrameRequest { time: source.metadata.rate.frame_duration(), scale: 0.25 }).unwrap();
        assert!(gpu_hub().running().is_none(), "a frame during the back-off started the GPU helper");
        gpu_hub().lock().backoff.until = None;
        source.video_frame(FrameRequest { time: source.metadata.rate.tick_of(2), scale: 0.25 }).unwrap();
        assert!(gpu_hub().running().is_some(), "the GPU helper was not started once the back-off ended");
        assert_eq!(gpu_hub().lock().backoff.failures, 0);
        drop(sources);
        close_hubs();
    }
    #[test]
    fn gpu_back_off_doubles_from_two_seconds_to_a_minute() {
        assert_eq!(backoff_delay(0), Duration::from_secs(2));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(32));
        assert_eq!(backoff_delay(6), Duration::from_secs(60));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(60));
        let mut backoff = GpuBackoff::default();
        assert!(backoff.until.is_none_or(|t| Instant::now() >= t));
        backoff.failures = 1;
        backoff.until = Some(Instant::now() + backoff_delay(1));
        assert!(backoff.until.is_some_and(|t| Instant::now() < t));
    }
    #[test]
    fn malformed_native_files_fail_without_crashing_the_editor() {
        let path = std::env::temp_dir().join(format!("filmcraft-red-invalid-{}.R3D", std::process::id()));
        for length in [0, 1, 64, 4096] {
            std::fs::write(&path, vec![0xa5; length]).unwrap();
            assert!(RedSource::open("invalid.R3D", &path, length as u64).is_err());
        }
        std::fs::remove_file(path).unwrap();
        // the helper answered each with an error and serves on (one helper serves every clip)
        assert!(cpu_hub().running().is_some_and(|w| w.registry.lock().failed.is_none()), "an unreadable file took the CPU helper down");
    }
    #[test]
    fn reduced_sizes_follow_the_measured_sdk_rounding() {
        // SDK 9.2.1 on 6048×3402 (docs/r3d-ne.md): both dimensions round down.
        assert_eq!(reduced_size(6048, 3402, 1), Some((6048, 3402)));
        assert_eq!(reduced_size(6048, 3402, 2), Some((3024, 1701)));
        assert_eq!(reduced_size(6048, 3402, 4), Some((1512, 850)));
        assert_eq!(reduced_size(6048, 3402, 8), Some((756, 425)));
        assert_eq!(reduced_size(6048, 3402, 16), Some((378, 212)));
        assert_eq!(reduced_size(4095, 2159, 16), Some((255, 134)));
        // Only the SDK's divisors, and never an empty frame.
        for divisor in [0, 3, 5, 6, 7, 9, 32, u32::MAX] {
            assert_eq!(reduced_size(6048, 3402, divisor), None, "divisor {divisor}");
        }
        assert_eq!(reduced_size(15, 3402, 16), None);
        assert_eq!(reduced_size(6048, 15, 16), None);
        assert_eq!(reduced_size(1, 1, 1), Some((1, 1)));
        assert_eq!(reduced_size(u32::MAX, u32::MAX, 16), Some((u32::MAX / 16, u32::MAX / 16)));
    }
    #[test]
    fn previews_pick_the_smallest_covering_decode_and_never_lower_export_resolution() {
        assert_eq!(preview_divisor(6048, 3402, 1.0), 1);
        assert_eq!(preview_divisor(6048, 3402, 0.5), 2);
        assert_eq!(preview_divisor(6048, 3402, 0.3), 2);
        assert_eq!(preview_divisor(6048, 3402, 0.25), 4);
        assert_eq!(preview_divisor(6048, 3402, 0.125), 8);
        assert_eq!(preview_divisor(6048, 3402, 0.0625), 16);
        assert_eq!(preview_divisor(6048, 3402, 0.01), 16);
        assert_eq!(preview_divisor(4096, 2160, 0.125), 8);
        assert_eq!(preview_divisor(4095, 2159, 0.1), 8);
        // A clip too small for a divisor falls back to the next larger decode, down to full.
        assert_eq!(preview_divisor(15, 15, 0.01), 8);
        assert_eq!(preview_divisor(1, 1, 0.01), 1);
        for (divisor, (w, h)) in [16, 8, 4].into_iter().map(|d| (d, reduced_size(6048, 3402, d).unwrap())) {
            // The size the app expects for the divisor it picked is what the helper sizes its
            // buffer to, and a reply of any other length is refused.
            assert_eq!((w, h), (6048 / divisor, 3402 / divisor));
            assert!(video_linear(&vec![0; w as usize * h as usize * 6 + 6], w as usize * h as usize, linearise_table(RawGamma::Bt1886)).is_err());
            assert!(video_linear(&vec![0; w as usize * h as usize * 6], w as usize * h as usize, linearise_table(RawGamma::Bt1886)).is_ok());
        }
    }

    /// Update RMD on a copy of the test clip in a temporary folder (never the user's original):
    /// the sidecar is written beside the copy only when asked, holds the settings (a fresh open
    /// reads them as the clip's recorded ones and says an RMD is in effect), and the source's own
    /// GPU copy of the clip is reopened with it.
    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage (copied to a temp folder); run alone (--test-threads=1)"]
    fn real_rmd_sidecar_is_written_beside_a_copy_and_read_back() {
        let original = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        let dir = std::env::temp_dir().join(format!(
            "filmcraft-rmd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(original.file_name().unwrap());
        std::fs::copy(&original, &path).unwrap();
        let open = |p: &Path| RedSource::open("copy.R3D", p, std::fs::metadata(p).unwrap().len()).unwrap();
        let source = open(&path);
        let before = source.raw_clip_info().unwrap();
        println!("before: {before:?}");
        assert!(before.rmd.is_none(), "a fresh copy has no sidecar");
        assert!(Path::new(&before.rmd_path).parent().is_some_and(|p| p == dir), "the sidecar goes beside the clip: {}", before.rmd_path);
        assert!(!Path::new(&before.rmd_path).exists(), "nothing is written by opening");
        let as_shot = source.raw_as_shot().unwrap();
        let iso = if as_shot.iso == 1600 { 3200 } else { 1600 };
        let frame = || source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap().to_linear_f32();
        let as_recorded = frame();
        let d = RawDevelop { iso: Some(iso), kelvin: Some(3200), gamma: RawGamma::Bt1886, tone_map: RawToneMap::Low, ..Default::default() };
        let written = source.write_raw_sidecar(&d).unwrap();
        println!("wrote {written}");
        assert!(Path::new(&written).exists() && Path::new(&written).parent().is_some_and(|p| p == dir), "{written}");
        let after = source.raw_clip_info().unwrap();
        assert_eq!(after.rmd.as_deref(), Some(written.as_str()), "the panel says an RMD is in effect");
        let recorded = source.raw_as_shot().unwrap();
        assert_eq!((recorded.iso, recorded.kelvin.round() as u32), (iso, 3200), "the RMD's settings are now the clip's recorded ones");
        assert_eq!(after.tone_map, Some(RawToneMap::Low));
        assert!(source.gpu_clip.lock().unwrap().is_none(), "the GPU copy of the clip is opened again with the sidecar");
        assert_ne!(frame(), as_recorded, "as recorded is now the RMD's development");
        // a fresh open (an import) reads the sidecar
        drop(source);
        let again = open(&path);
        assert_eq!(again.raw_clip_info().unwrap().rmd.as_deref(), Some(written.as_str()));
        assert_eq!(again.raw_as_shot().unwrap().iso, iso);
        // a hostile development is refused before the helper is asked
        assert!(again.write_raw_sidecar(&RawDevelop { iso: Some(1250), ..Default::default() }).is_err());
        drop(again);
        close_hubs();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage"]
    fn real_clip_video_audio_and_seeking() {
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        let reader: SharedReader = Arc::new(filmcraft_media::reader::FileReader::open(&path).unwrap());
        filmcraft_codecs::register_reader_opener(reader_opener);
        let source = filmcraft_media::reader::open_reader("test.R3D", reader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers()).unwrap();
        let info = source.info();
        let v = info.video.as_ref().unwrap();
        assert_eq!(v.codec, "R3D NE");
        assert!(info.duration > Tick::ZERO);
        let first = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap();
        assert!(first.to_linear_f32().as_chunks::<4>().0.iter().any(|p| p[..3].iter().any(|v| *v != 0.0)));
        // A quarter request decodes at quarter resolution, rounded down (6048×3402 → 1512×850),
        // and the picture is the half-resolution frame at half its size: a 2×2 box average of the
        // half frame (dropping its odd last row/column) is close to the quarter decode, and far
        // closer than a misaligned (flipped or one-row-shifted) comparison.
        assert_eq!((first.width, first.height), (v.width / 4, v.height / 4));
        let half = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.5 }).unwrap();
        assert_eq!((half.width, half.height), (v.width / 2, v.height / 2));
        let (qw, qh) = (first.width as usize, first.height as usize);
        let (half_px, quarter_px) = (half.to_linear_f32(), first.to_linear_f32());
        let half_at = |x: usize, y: usize, c: usize| half_px[(y * half.width as usize + x) * 4 + c];
        let quarter_at = |x: usize, y: usize, c: usize| quarter_px[(y * qw + x) * 4 + c];
        let error = |map: &dyn Fn(usize, usize) -> (usize, usize)| -> f64 {
            let mut sum = 0.0;
            for y in 0..qh {
                for x in 0..qw {
                    let (sx, sy) = map(x, y);
                    for c in 0..3 {
                        let box_mean =
                            (half_at(2 * sx, 2 * sy, c) + half_at(2 * sx + 1, 2 * sy, c) + half_at(2 * sx, 2 * sy + 1, c) + half_at(2 * sx + 1, 2 * sy + 1, c))
                                / 4.0;
                        sum += (box_mean - quarter_at(x, y, c)).abs() as f64;
                    }
                }
            }
            sum / (qw * qh * 3) as f64
        };
        let mean = quarter_px.as_chunks::<4>().0.iter().map(|p| (p[0] + p[1] + p[2]) as f64 / 3.0).sum::<f64>() / (qw * qh) as f64;
        let aligned = error(&|x, y| (x, y));
        let flipped = error(&|x, y| (x, qh - 1 - y));
        let shifted = error(&|x, y| (x, (y + 1).min(qh - 1)));
        println!("quarter vs box-filtered half: mean level {mean:.4}, mean abs error aligned {aligned:.5}, flipped {flipped:.5}, shifted one row {shifted:.5}");
        // Measured on a 6K 59.94p clip: aligned 0.051 at level 0.607 (Quarter Good against box-filtered
        // Half Premium), flipped 0.96, shifted 0.070.
        assert!(mean > 0.0 && aligned < 0.15 * mean, "quarter decode does not match the half decode: {aligned} vs level {mean}");
        assert!(aligned < flipped && aligned < shifted, "quarter decode is misaligned: {aligned} aligned, {flipped} flipped, {shifted} shifted");
        let last_time = info.duration - v.frame_rate.frame_duration();
        let last = source.video_frame(FrameRequest { time: last_time, scale: 0.25 }).unwrap();
        assert_eq!(last.pts, last_time);
        let again = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap();
        assert_eq!(first.to_linear_f32(), again.to_linear_f32());
        let full = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
        assert_eq!((full.width, full.height), (v.width, v.height));
        // Source Settings: every colour space and gamma decodes, as linear light; the display
        // curves differ from the linear default (tone mapped); back at the default the picture is
        // the same again
        let linear = first.to_linear_f32();
        let frame_with = |d: RawDevelop| {
            source.set_develop(&d).unwrap();
            source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap()
        };
        let display = frame_with(RawDevelop::STANDARD);
        assert_eq!((display.color.transfer, display.width), (Transfer::Linear, first.width));
        assert_ne!(display.to_linear_f32(), linear, "the display output is tone mapped, not linear");
        for space in RawColorSpace::ALL {
            for gamma in RawGamma::ALL.into_iter().filter(|g| RawDevelop::supports(space, *g)) {
                let f = frame_with(RawDevelop { color_space: space, gamma, ..RawDevelop::STANDARD }).to_linear_f32();
                let mean = f.as_chunks::<4>().0.iter().map(|p| f64::from(p[0] + p[1] + p[2]) / 3.0).sum::<f64>() / (f.len() / 4) as f64;
                println!("{space:?} {gamma:?}: mean linear level {mean:.4}");
                assert!(mean.is_finite() && mean > 0.0, "{space:?} {gamma:?}");
            }
        }
        // the SDK's Log3G10 encoding is the published curve the compositor decodes with: the same
        // frame developed into scene-linear REDWideGamutRGB and into Log3G10 (then linearised
        // here) agrees to within the 16-bit quantisation of the log code values
        let rwg_linear = frame_with(RawDevelop { color_space: RawColorSpace::RedWideGamutRgb, gamma: RawGamma::Linear, ..Default::default() }).to_linear_f32();
        let rwg_log = frame_with(RawDevelop { color_space: RawColorSpace::RedWideGamutRgb, gamma: RawGamma::Log3G10, ..Default::default() }).to_linear_f32();
        let (mut diff, mut level) = (0f64, 0f64);
        for (a, b) in rwg_linear.as_chunks::<4>().0.iter().zip(rwg_log.as_chunks::<4>().0) {
            for c in 0..3 {
                diff += f64::from((a[c] - b[c]).abs());
                level += f64::from(a[c].abs());
            }
        }
        println!(
            "REDWideGamutRGB linear vs Log3G10 linearised: mean abs difference {:.5} of level {:.5}",
            diff / level.max(1e-9),
            level / (rwg_linear.len() / 4 * 3) as f64
        );
        assert!(diff < 0.01 * level, "Log3G10 linearised does not match the linear output: {diff} vs {level}");
        // the decode quality caps a preview: a half-size preview at quarter quality is a quarter
        source.set_develop(&RawDevelop { quality: Some(filmcraft_media::RawQuality::QuarterGood), ..Default::default() }).unwrap();
        let capped = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.5 }).unwrap();
        assert_eq!(capped.width, v.width / 4);
        let half_good = frame_with(RawDevelop { quality: Some(filmcraft_media::RawQuality::HalfGood), ..Default::default() });
        assert_eq!(half_good.width, v.width / 4, "a quarter request decodes at a quarter at any quality");
        source.set_develop(&RawDevelop { quality: Some(filmcraft_media::RawQuality::HalfGood), ..Default::default() }).unwrap();
        assert_eq!(source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.5 }).unwrap().width, v.width / 2, "half, good");
        assert_eq!(source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().width, v.width, "a full-size request is full at any quality");
        // the processing options decode (their effect on a clean frame may be nil)
        for d in [
            RawDevelop { chroma_noise_reduction: Some(false), flashing_pixel: Some(RawFlashing::Strong), ..RawDevelop::STANDARD },
            RawDevelop { metadata_curves: false, cdl: true, creative_lut: true, ..RawDevelop::STANDARD },
            RawDevelop { color_space: RawColorSpace::Rec2020, gamma: RawGamma::St2084, hdr_peak_nits: Some(1000), ..RawDevelop::STANDARD },
        ] {
            assert!(frame_with(d).to_linear_f32().iter().all(|v| v.is_finite()), "{d:?}");
        }
        // what the clip records: Nikon R3D NE has no HDRx track; the SDK's default tone map and
        // rolloff are the ones a new project starts with
        let clip = source.raw_clip_info().expect("an R3D source reports what the clip records");
        println!("recorded: {clip:?}");
        assert!(!clip.hdrx, "Nikon R3D NE records no HDRx track");
        assert_eq!((clip.tone_map_default, clip.rolloff_default), (Some(RawDevelop::STANDARD.tone_map), Some(RawDevelop::STANDARD.rolloff)));
        assert!(clip.hdr_peak_range.0 <= clip.hdr_peak_default && clip.hdr_peak_default <= clip.hdr_peak_range.1, "{clip:?}");
        source.set_develop(&RawDevelop::default()).unwrap();
        assert_eq!(source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap().to_linear_f32(), linear);
        // RAW adjustments: the helper reports the clip's as-shot values; developing the same frame
        // at another ISO, colour temperature, tint or exposure changes the picture, each on its
        // own, and back at as shot (no adjustment set) the picture is the linear one again
        let as_shot = source.raw_as_shot().expect("an R3D source reports its as-shot settings");
        assert!(filmcraft_media::RAW_ISO_VALUES.contains(&as_shot.iso), "{as_shot:?}");
        assert!(filmcraft_media::RAW_KELVIN_RANGE.contains(&(as_shot.kelvin.round() as u32)), "{as_shot:?}");
        println!("as shot: {as_shot:?}");
        let other_iso = if as_shot.iso == 3200 { 800 } else { 3200 };
        let mut pictures = vec![linear.clone()];
        for (name, develop) in [
            ("ISO", RawDevelop { iso: Some(other_iso), ..Default::default() }),
            ("Kelvin", RawDevelop { kelvin: Some(if as_shot.kelvin > 4000.0 { 3200 } else { 6500 }), ..Default::default() }),
            ("tint", RawDevelop { tint: Some(filmcraft_media::Hundredths(4000)), ..Default::default() }),
            ("exposure", RawDevelop { exposure: Some(filmcraft_media::Hundredths(100)), ..Default::default() }),
        ] {
            source.set_develop(&develop).unwrap();
            let picture = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap().to_linear_f32();
            assert!(!pictures.contains(&picture), "{name} changed nothing");
            pictures.push(picture);
        }
        // setting the as-shot ISO explicitly is the as-shot picture (the SDK takes the same value)
        source.set_develop(&RawDevelop { iso: Some(as_shot.iso), ..Default::default() }).unwrap();
        assert_eq!(
            source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap().to_linear_f32(),
            linear,
            "the as-shot ISO is the as-shot picture"
        );
        source.set_develop(&RawDevelop::default()).unwrap();
        assert_eq!(
            source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap().to_linear_f32(),
            linear,
            "as shot reproduces the previous output"
        );
        // an ISO the SDK does not list is refused before any request is sent
        assert!(source.set_develop(&RawDevelop { iso: Some(1250), ..Default::default() }).is_ok(), "stored; refused when a frame is asked for");
        assert!(source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).is_err());
        source.set_develop(&RawDevelop::default()).unwrap();
        if let Some(a) = info.audio() {
            let audio = source.audio(0, 2048, a.sample_rate).unwrap();
            assert_eq!(audio.channels.len(), a.channels as usize);
            assert!(audio.channels.iter().flatten().all(|x| x.is_finite()));
            let lead = source.audio(-1024, 2048, a.sample_rate).unwrap();
            assert!(lead.channels.iter().all(|c| c[..1024].iter().all(|x| *x == 0.0)));
            for (ch, expected) in lead.channels.iter().zip(&audio.channels) {
                assert_eq!(&ch[1024..], &expected[..1024]);
            }
            let resampled = source.audio(0, 1024, 44100).unwrap();
            let left = source.audio(0, 512, 44100).unwrap();
            let right = source.audio(512, 512, 44100).unwrap();
            for c in 0..a.channels as usize {
                assert_eq!(resampled.channels[c], [left.channels[c].as_slice(), right.channels[c].as_slice()].concat());
            }
            assert!(source.audio(i64::MAX, 2, a.sample_rate).is_err());
            assert!(source.audio(0, 2, 0).is_err());
            assert!(
                source
                    .audio(info.duration.to_units_floor(a.sample_rate as i64) + a.sample_rate as i64, 128, a.sample_rate)
                    .unwrap()
                    .channels
                    .iter()
                    .flatten()
                    .all(|x| *x == 0.0)
            );
        }
        println!(
            "verified {}x{} {}/{} fps, duration {} ticks, {} audio streams",
            v.width,
            v.height,
            v.frame_rate.num,
            v.frame_rate.den,
            info.duration.0,
            info.audio_streams.len()
        );
        if info.has_audio() {
            // A video decode may hold this lock for seconds; audio must not wait for it.
            let independent = RedSource::open("test.R3D", &path, std::fs::metadata(&path).unwrap().len()).unwrap();
            independent.audio(0, 64, 48000).unwrap();
            std::thread::scope(|scope| {
                let guard = independent.state.lock().unwrap();
                let (send, receive) = mpsc::channel();
                let independent = &independent;
                scope.spawn(move || send.send(independent.audio(64, 64, 48000)).unwrap());
                let result = receive.recv_timeout(Duration::from_secs(2));
                drop(guard); // Release even on regression, so the test cannot deadlock on join.
                assert!(result.expect("audio waited for video lock").is_ok());
            });
        }
    }
    #[test]
    #[ignore = "requires RED_R3D_TEST_CLIP pointing to user-owned Nikon ZR footage"]
    fn real_pipeline_reuses_slots_routes_frames_and_contains_failures() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let path = PathBuf::from(std::env::var_os("RED_R3D_TEST_CLIP").expect("set RED_R3D_TEST_CLIP"));
        let (worker, clip, m, hub) = gpu_helper_with_clip(&path);
        assert!(m.frames >= 4);
        assert!(hub.slots >= 1 && hub.slot_bytes >= m.width as usize * m.height as usize * 6, "a GPU helper shares its frame slots");
        let divisor = preview_divisor(m.width, m.height, 0.5);
        let frame_bytes = (m.width / divisor) as usize * (m.height / divisor) as usize * 6;
        // A frame's bytes, copied out of its slot so the slot goes back to the helper at once;
        // holding every slot's lease while asking for more would wait for a slot forever.
        let copy = |reply: Reply| -> Vec<u8> {
            match reply {
                Reply::Slot(lease) => {
                    assert!(lease.index < hub.slots as u32);
                    lease.bytes().unwrap().to_vec()
                }
                Reply::Inline(_) | Reply::Error(_) => panic!("a GPU helper delivers frames in shared slots"),
            }
        };
        // Every reduced decode returns exactly the bytes of the rounded-down size (docs/r3d-ne.md).
        for d in [16, 8, 4, 2] {
            let (w, h) = reduced_size(m.width, m.height, d).unwrap();
            assert_eq!(copy(worker.request(&format!("V {clip} 0 {d} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()).len(), w as usize * h as usize * 6, "divisor {d}");
        }
        // The clip's as-shot values are reported, and a frame decodes with every RAW adjustment
        // set within the SDK's limits.
        assert!(filmcraft_media::RAW_ISO_VALUES.contains(&m.as_shot.iso), "{:?}", m.as_shot);
        assert_eq!(copy(worker.request(&format!("V {clip} 0 {divisor} 0 0 0 0 0 0 4 0 15 800 5600 -1250 33\n")).unwrap()).len(), frame_bytes);
        for round in 0..2 {
            let barrier = std::sync::Barrier::new(4);
            let results = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..4)
                    .map(|frame| {
                        let worker = &worker;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            copy(worker.request(&format!("V {clip} {frame} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap())
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
            });
            for (frame, bytes) in results.iter().enumerate() {
                assert_eq!(bytes.len(), frame_bytes);
                assert_eq!(
                    bytes,
                    &copy(worker.request(&format!("V {clip} {frame} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()),
                    "round {round}, frame {frame}"
                );
            }
        }
        // Every slot's frame cancelled after submission: the reader discards the replies and
        // hands every slot back, or the next request would wait for a slot until the deadline.
        for _ in 0..2 {
            let cancel = Arc::new(AtomicBool::new(false));
            std::thread::scope(|scope| {
                let (worker, cancel) = (&worker, &cancel);
                let requests: Vec<_> = (0..hub.slots)
                    .map(|frame| {
                        scope.spawn(move || {
                            filmcraft_media::cancel::with_cancel(cancel, || worker.request(&format!("V {clip} {frame} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")))
                        })
                    })
                    .collect();
                // Wait for registration, not a guessed decode duration.
                let started = Instant::now();
                while worker.registry.lock().requests.len() < hub.slots && started.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                cancel.store(true, Ordering::Relaxed);
                for request in requests {
                    assert!(matches!(request.join().unwrap(), Err(MediaError::Cancelled)));
                }
            });
            // An abandoned reply must not be delivered to the next request or poison the worker.
            assert_eq!(copy(worker.request(&format!("V {clip} 1 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()).len(), frame_bytes);
        }
        // Leases dropped before their frames arrive (the requester gave up) also come back.
        {
            let (send, _dropped) = mpsc::sync_channel::<Result<Reply>>(1);
            let id = worker.registry.register(worker.limit, send).unwrap();
            worker.commands.lock().unwrap().write_all(format!("V {id} {clip} 0 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n").as_bytes()).unwrap();
        }
        for frame in 0..hub.slots {
            assert_eq!(copy(worker.request(&format!("V {clip} {frame} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()).len(), frame_bytes);
        }
        // More callers than the helper has slots: the extra ones wait in Rust (never more than
        // slots + 1 outstanding), and every frame still comes back right.
        assert_eq!(worker.limit, hub.slots + 1);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for frame in 0..(hub.slots + 3) {
                let (worker, peak) = (&worker, &peak);
                scope.spawn(move || {
                    let bytes = copy(worker.request(&format!("V {clip} {} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n", frame % 4)).unwrap());
                    assert_eq!(bytes.len(), frame_bytes);
                    peak.fetch_max(worker.registry.lock().requests.len(), Ordering::Relaxed);
                });
            }
        });
        assert!(peak.load(Ordering::Relaxed) <= hub.slots + 1, "outstanding requests reached {}", peak.load(Ordering::Relaxed));
        // A delivered slot is the app's until it is freed: other frames decode around it into
        // the other slots and its bytes do not change (a torn picture would be the symptom).
        {
            let reply = worker.request(&format!("V {clip} 3 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap();
            let Reply::Slot(lease) = &reply else { panic!("shared frame expected") };
            let before = lease.bytes().unwrap().to_vec();
            for frame in 0..(hub.slots + 2) {
                assert_eq!(copy(worker.request(&format!("V {clip} {} {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n", frame % 3)).unwrap()).len(), frame_bytes);
            }
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(lease.bytes().unwrap(), &before[..], "a held slot was written while the app owned it");
        }
        // A frame read while its helper is gone: the section outlives the process.
        {
            let reply = worker.request(&format!("V {clip} 0 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap();
            let Reply::Slot(lease) = &reply else { panic!("shared frame expected") };
            let before = lease.bytes().unwrap().to_vec();
            let mut child = worker.child.lock().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            drop(child);
            assert_eq!(lease.bytes().unwrap(), &before[..]);
            assert!(worker.request(&format!("V {clip} 1 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).is_err());
        }
        let (worker, clip, _, _) = gpu_helper_with_clip(&path);
        assert_eq!(copy(worker.request(&format!("V {clip} 1 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()).len(), frame_bytes);
        // Force a stalled request deadline without a two-minute test delay. Watchdog must
        // kill the process and wake all pending receivers, including abandoned requests.
        let (send, receiver) = mpsc::sync_channel(1);
        worker.registry.lock().requests.insert(999, PendingRequest { sender: send, started: Instant::now() - TIMEOUT, in_flight: 1 });
        assert!(receiver.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
        assert!(worker.request(&format!("V {clip} 2 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).is_err());
        let status = worker.child.lock().unwrap().wait().unwrap();
        assert!(!status.success());
        // RAW adjustments the SDK does not accept (an unlisted ISO, a field set without its
        // flag, a value outside the SDK's limits) are malformed requests: the helper answers an
        // error naming the value instead of applying or crashing, and serves on (one helper
        // serves every clip; the app never sends these anyway, `wire` refuses them first).
        let (worker, clip, _, _) = gpu_helper_with_clip(&path);
        for (bad, expected) in [
            ("0 0 0 0 0 0 4 0 1 1250 0 0 0", "unsupported ISO 1250"),
            ("0 0 0 0 0 0 4 0 0 800 0 0 0", "malformed RAW adjustment"),
            ("0 0 0 0 0 0 4 0 2 0 100001 0 0", "colour temperature out of range"),
            // FCRD9 development fields: each beyond its table, a colour space and gamma that do
            // not go together, a chroma value without its flag, a peak beyond the SDK's limits,
            // too few or too many fields, a negative one
            ("2 0 0 0 0 0 4 0 0 0 0 0 0", "unsupported development setting"),
            ("0 4 0 0 0 0 4 0 0 0 0 0 0", "unsupported development setting"),
            ("0 0 6 0 0 0 4 0 0 0 0 0 0", "unsupported development setting"),
            ("0 0 1 5 0 0 4 0 0 0 0 0 0", "unsupported development setting"),
            ("0 0 1 0 6 0 4 0 0 0 0 0 0", "unsupported development setting"),
            ("0 0 0 0 0 0 32 0 0 0 0 0 0", "unsupported development setting"),
            ("0 0 0 0 0 0 4 5 0 0 0 0 0", "unsupported development setting"),
            ("0 0 3 0 0 0 4 0 0 0 0 0 0", "unsupported colour space and gamma curve"),
            ("0 3 1 0 0 0 4 0 0 0 0 0 0", "unsupported colour space and gamma curve"),
            ("0 2 4 0 0 0 4 0 0 0 0 0 0", "unsupported colour space and gamma curve"),
            ("0 0 0 0 0 0 6 0 0 0 0 0 0", "malformed chroma noise reduction"),
            ("0 1 4 0 0 1 4 0 0 0 0 0 0", "HDR peak out of range"),
            ("0 1 4 0 0 4000000000 4 0 0 0 0 0 0", "HDR peak out of range"),
            ("0 0 0 0 0 0 4 0 0 0 0 0", "video frame out of range"),
            ("0 0 0 0 0 0 4 0 0 0 0 0 0 0", "video frame out of range"),
            ("0 0 1 -1 0 0 4 0 0 0 0 0 0", ""), // refused however the stream reads a negative unsigned
        ] {
            let Err(e) = worker.request(&format!("V {clip} 0 {divisor} {bad}\n")) else { panic!("{bad} was accepted") };
            assert!(e.to_string().contains(expected), "{bad}: {e}");
        }
        // a request for a clip the helper does not have is an error too, and the helper serves on
        let Err(e) = worker.request(&format!("V {} 0 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n", clip + 1_000_000)) else {
            panic!("an unknown clip handle was accepted")
        };
        assert!(e.to_string().contains("unknown clip handle"), "{e}");
        assert_eq!(copy(worker.request(&format!("V {clip} 0 {divisor} 0 0 0 0 0 0 4 0 0 0 0 0 0\n")).unwrap()).len(), frame_bytes);
    }
    #[test]
    fn metadata_preserves_fractional_rate_and_rejects_hostile_values() {
        // FCRD9: the as-shot line, what else the clip records, and its RMD path
        const REC: &str = "1 2 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1";
        let text = |head: &str, raw: &str, rec: &str, path: &str| format!("{head}\n01:00:00;00\nCUDA test device\n{raw}\n{rec}\n{path}");
        const HEAD: &str = "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453440";
        let valid = text(HEAD, "1280 5600 -2.5 -0.0339", REC, "/footage/DSC_0001.RMD").into_bytes();
        let m = Metadata::parse(&valid).unwrap();
        assert_eq!(m.duration, Tick::from_rational(300, 1001, 30000));
        assert_eq!(m.timecode, Some(107892));
        assert_eq!(m.as_shot, RawAsShot { iso: 1280, kelvin: 5600.0, tint: -2.5, exposure: -0.0339 });
        let c = &m.clip;
        assert_eq!(
            (c.color_space, c.gamma, c.tone_map, c.rolloff),
            (Some(RawColorSpace::Rec709), Some(RawGamma::Bt1886), Some(RawToneMap::Medium), Some(RawRollOff::Soft))
        );
        assert_eq!(
            (c.hdr_peak_nits, c.chroma_noise_reduction, c.flashing_pixel, c.lut, c.hdrx, c.rmd.as_deref()),
            (Some(1000), true, RawFlashing::Off, false, false, None)
        );
        assert_eq!(
            (c.hdr_peak_range, c.hdr_peak_default, c.tone_map_default, c.rolloff_default),
            ((1000, 10000), 1000, Some(RawToneMap::Medium), Some(RawRollOff::Medium))
        );
        assert_eq!((c.cdl[0], c.cdl[1], c.cdl[2], c.cdl[3]), (1.0, 1.0, 0.0, 1.0));
        assert_eq!(c.rmd_path, "/footage/DSC_0001.RMD");
        // an RMD in effect, an HDRx clip, recorded values without a code here
        let other = text(HEAD, "800 5600 0 0", "0 0 0 0 0 0 0 1 1 1 1 2 1 1000 10000 1000 0 0 1 1 0 1 1 0 1 1 0 1", "/clips/A001.RMD");
        let o = Metadata::parse(other.as_bytes()).unwrap().clip;
        assert_eq!((o.color_space, o.gamma, o.tone_map, o.hdr_peak_nits), (None, None, None, None));
        assert_eq!((o.lut, o.lut_enabled, o.cdl_enabled, o.curves, o.hdrx, o.rmd.as_deref()), (true, true, true, true, true, Some("/clips/A001.RMD")));
        let cpu = Metadata::parse(text("FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 0 0", "800 5600 0 0", REC, "").as_bytes()).unwrap();
        assert_eq!(cpu.as_shot.iso, 800);
        // the recorded lens squeeze: none from a helper that sends no line, the exact ratio from
        // one that does, square pixels for a value that is no usable ratio
        assert_eq!((m.par, cpu.par), ((1, 1), (1, 1)));
        // the squeeze line follows the RMD path
        let head = text(HEAD, "800 5600 0 0", REC, "/footage/DSC_0001.RMD");
        for (line, par) in [
            ("1.33", (133, 100)),
            ("1.33000004", (133, 100)),
            ("1.33333337", (4, 3)),
            ("2", (2, 1)),
            ("1", (1, 1)),
            ("0", (1, 1)),
            ("", (1, 1)),
            ("-1.33", (1, 1)),
            ("nan", (1, 1)),
            ("inf", (1, 1)),
            ("1e30", (1, 1)),
            ("garbage", (1, 1)),
        ] {
            assert_eq!(Metadata::parse(format!("{head}\n{line}").as_bytes()).unwrap().par, par, "{line}");
        }
        // older helpers are refused: their requests would be misread
        for older in [
            "FCRD8 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453440",
            "FCRD7 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453440",
            "FCRD6 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453440",
            "FCRD5 6048 3402 300 30000 1001 2 48000 480480 1 32 4",
        ] {
            assert!(Metadata::parse(text(older, "800 5600 0 0", REC, "").as_bytes()).is_err(), "{older}");
        }
        assert!(Metadata::parse(b"FCRD8 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453440\n01:00:00;00\nCUDA test device\n800 5600 0 0").is_err());
        assert!(Metadata::parse(format!("{HEAD}\n01:00:00;00\nCUDA test device\n800 5600 0 0").as_bytes()).is_err(), "no recorded line");
        assert!(Metadata::parse(format!("{HEAD}\n01:00:00;00\nCUDA test device").as_bytes()).is_err());
        for raw in [
            "",
            "800",
            "800 5600 0",
            "800 5600 0 0 0",
            "0 5600 0 0",
            "-800 5600 0 0",
            "800 nan 0 0",
            "800 inf 0 0",
            "800 5600 1e9 0",
            "800 5600 0 -200",
            "8e2 5600 0 0",
        ] {
            assert!(Metadata::parse(text(HEAD, raw, REC, "").as_bytes()).is_err(), "{raw:?}");
        }
        // hostile recorded settings: wrong counts, codes out of range, non-finite CDL, a path with
        // a control character
        for rec in [
            "",
            "1 2 3 4",
            "5 2 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 7 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 5 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 6 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 2 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 1 5 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 1 1 0 0 0 0 17 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 -1 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 nan 1 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1e9 0 1 1 0 1 1 0 1",
            "1 2 3 4 1000 1 1 0 0 0 0 1 0 1000 10000 1000 3 3 1 1 0 1 1 0 1 1 0 1 7",
        ] {
            assert!(Metadata::parse(text(HEAD, "800 5600 0 0", rec, "").as_bytes()).is_err(), "{rec:?}");
        }
        assert!(Metadata::parse(text(HEAD, "800 5600 0 0", REC, "a\u{7}b").as_bytes()).is_err());
        for s in [
            "FCRD9 0 2 1 24 1 0 0 0 0 0 4 4096",
            "FCRD9 2 2 1 0 1 0 0 0 0 0 4 4096",
            "FCRD9 2 2 1 24 0 0 0 0 0 0 4 4096",
            "FCRD9 2 2 1 24 1 17 48000 10 1 32 4 4096",
            "FCRD9 2 2 1 24 1 2 0 10 1 32 4 4096",
            "FCRD9 2 2 1 24 1 2 48000 10 1 32 65 4096",
            // shared slots must hold a full frame, in whole pages, within reason, and only a
            // GPU helper has them
            "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123449344",
            "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 4 123453442",
            "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 4 0",
            "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 0 123453440",
            "FCRD9 6048 3402 300 30000 1001 2 48000 480480 1 32 64 4294967296",
            "FCRD9 2 2 1 24 1 0 0 0 0 0 1 201334784",
        ] {
            assert!(Metadata::parse(text(s, "800 5600 0 0", REC, "").as_bytes()).is_err(), "{s}");
        }
        for end in 0..valid.len() {
            assert!(std::panic::catch_unwind(|| Metadata::parse(&valid[..end])).is_ok());
        }
        for i in 0..valid.len() {
            let mut mutated = valid.to_vec();
            mutated[i] ^= 0xff;
            assert!(std::panic::catch_unwind(|| Metadata::parse(&mutated)).is_ok());
        }
    }
    #[test]
    fn start_up_replies_name_the_slots_and_reject_hostile_values() {
        let gpu = HubInfo::parse(b"FCRD9 5 123453440 3\nCUDA Example GPU").unwrap();
        assert_eq!((gpu.slots, gpu.slot_bytes, gpu.threads), (5, 123_453_440, 3));
        assert_eq!(gpu.processing, "CUDA Example GPU");
        let cpu = HubInfo::parse(b"FCRD9 0 0 3\nCPU (explicit override)").unwrap();
        assert_eq!((cpu.slots, cpu.slot_bytes), (0, 0));
        for bad in [
            "FCRD7 5 123453440 3\nCUDA x",
            "FCRD8 5 123453440 3\nCUDA x",
            "FCRD9 5 123453440\nCUDA x",
            "FCRD9 5 123453440 3 1\nCUDA x",
            "FCRD9 65 123453440 3\nCUDA x",
            "FCRD9 5 123453441 3\nCUDA x",
            "FCRD9 5 0 3\nCUDA x",
            "FCRD9 0 4096 3\nCPU (explicit override)",
            "FCRD9 5 123453440 0\nCUDA x",
            "FCRD9 5 123453440 65\nCUDA x",
            "FCRD9 64 4294967296 3\nCUDA x",
            "FCRD9 5 123453440 3",
            "FCRD9 5 123453440 3\nsomething else",
            "FCRD9 -5 123453440 3\nCUDA x",
        ] {
            assert!(HubInfo::parse(bad.as_bytes()).is_err(), "{bad:?}");
        }
        let valid = b"FCRD9 5 123453440 3\nCUDA Example GPU";
        for end in 0..valid.len() {
            assert!(std::panic::catch_unwind(|| HubInfo::parse(&valid[..end])).is_ok());
        }
    }
    #[test]
    fn float_audio_preserves_headroom_and_integer_audio_sign() {
        let bytes: Vec<_> = [2.5f32, -4.0, 0.25, 0.5].iter().flat_map(|v| v.to_be_bytes()).collect();
        assert_eq!(audio_samples(&bytes, 2, 2, true).unwrap(), vec![vec![2.5, 0.25], vec![-4.0, 0.5]]);
        assert_eq!(audio_samples(&i32::MIN.to_be_bytes(), 1, 1, false).unwrap()[0], vec![-1.0]);
        assert!(audio_samples(&f32::NAN.to_be_bytes(), 1, 1, true).is_err());
        assert!(audio_samples(&bytes[..15], 2, 2, true).is_err());
        assert!(audio_samples(&[], 0, 1, false).is_err());
    }
    #[test]
    #[ignore = "diagnostic conversion benchmark; run without other workloads"]
    fn benchmark_half_conversion() {
        let pixels = 3024 * 1701;
        let bytes: Vec<_> = (0..pixels * 3).flat_map(|sample| ((sample % 0x4400) as u16).to_le_bytes()).collect();
        let start = Instant::now();
        let texels = video_texels(&bytes, pixels).unwrap();
        let repack = start.elapsed();
        let start = Instant::now();
        let floats = VideoFrame::rgba_f16(3024, 1701, texels).to_linear_f32();
        let widen = start.elapsed();
        assert_eq!(floats.len(), pixels * 4);
        println!("half conversion: repack {:.3} ms, widen to f32 {:.3} ms", repack.as_secs_f64() * 1000.0, widen.as_secs_f64() * 1000.0);
    }
    #[test]
    fn half_rgb_preserves_linear_highlights() {
        let bytes: Vec<_> = [0x3c00u16, 0x4000, 0xbc00].iter().flat_map(|v| v.to_le_bytes()).collect();
        let texels = video_texels(&bytes, 1).unwrap();
        assert_eq!(texels, vec![0x00, 0x3c, 0x00, 0x40, 0x00, 0xbc, 0x00, 0x3c]);
        assert_eq!(VideoFrame::rgba_f16(1, 1, texels).to_linear_f32(), vec![1.0, 2.0, -1.0, 1.0]);
        assert_eq!(filmcraft_frame::half_to_f32(1), 2.0f32.powi(-24));
        assert!(video_texels(&bytes[..5], 1).is_err());
        // infinity and NaN in any channel are rejected; the largest finite half is not
        assert!(video_texels(&[0, 124, 0, 0, 0, 0], 1).is_err());
        assert!(video_texels(&[0, 0, 0, 0, 1, 0xfc], 1).is_err());
        assert!(video_texels(&[0xff, 0x7b, 0xff, 0xfb, 0, 0], 1).is_ok());
        // 16-bit code values (every other gamma) are linearised through the curve's table and reject
        // nothing but a wrong size
        let codes: Vec<u8> = [0u16, 32768, 65535].iter().flat_map(|v| v.to_le_bytes()).collect();
        let lin = |bytes: &[u8], g: RawGamma| VideoFrame::rgba_f16(1, 1, video_linear(bytes, 1, linearise_table(g)).unwrap()).to_linear_f32();
        let got = lin(&codes, RawGamma::Bt1886);
        let want = filmcraft_color::srgb_to_linear(32768.0 / 65535.0);
        assert!(got[0] == 0.0 && (got[1] - want).abs() < want * 1e-3 && (got[2] - 1.0).abs() < 1e-3 && got[3] == 1.0, "{got:?}");
        assert_eq!(video_linear(&[0, 124, 0, 0, 0, 0], 1, linearise_table(RawGamma::Srgb)).map(|v| v.len()).ok(), Some(8), "a NaN half is a plain code value");
        assert!(video_linear(&codes[..5], 1, linearise_table(RawGamma::Srgb)).is_err());
        assert!(video_linear(&codes, 2, linearise_table(RawGamma::Srgb)).is_err());
        assert!(video_linear(&codes, 1, &[0; 16]).is_err(), "a table that is not one per code value");
    }
    /// Every gamma's table is the compositor's own curve: mid grey (18 % scene light, or the
    /// display's mid grey for the display curves) encoded by the curve comes back as itself, and
    /// every code value gives a finite half (PQ peaks at 10 000 cd/m² over reference white).
    #[test]
    fn linearised_code_values_round_trip_mid_grey() {
        use filmcraft_color::transform::encode_channel;
        for gamma in RawGamma::ALL.into_iter().filter(|g| *g != RawGamma::Linear) {
            let table = linearise_table(gamma);
            assert_eq!(table.len(), 65536);
            assert!(table.iter().all(|h| filmcraft_frame::half_to_f32(*h).is_finite()), "{gamma:?}");
            let code = (encode_channel(0.18, gamma.curve(), filmcraft_color::Range::Full) * 65535.0).round() as u16;
            let back = filmcraft_frame::half_to_f32(table[usize::from(code)]);
            assert!((back - 0.18).abs() < 0.18 * 2e-3, "{gamma:?}: code {code} → {back}");
            // monotonic: a brighter code is never darker light
            assert!(table.windows(2).all(|w| filmcraft_frame::half_to_f32(w[1]) >= filmcraft_frame::half_to_f32(w[0])), "{gamma:?}");
        }
        let top = filmcraft_frame::half_to_f32(linearise_table(RawGamma::St2084)[65535]);
        assert!((top - 10_000.0 / 203.0).abs() < 0.1, "{top}");
    }
    #[test]
    fn response_ids_route_out_of_order_and_discard_cancelled_receivers() {
        let mut pending = Pending::default();
        let mut receivers = Vec::new();
        for id in 1..=4 {
            let (send, receive) = mpsc::sync_channel(1);
            pending.requests.insert(id, PendingRequest::new(send));
            receivers.push(receive);
        }
        drop(receivers.pop()); // ID 4 was cancelled, but its response must still be drained.
        for id in [4, 2, 3, 1] {
            pending.dispatch(id, Ok(Reply::Inline(vec![id as u8]))).unwrap();
        }
        for (index, receiver) in receivers.iter().enumerate() {
            assert_eq!(receiver.recv().unwrap().unwrap().into_inline().unwrap(), vec![(index + 1) as u8]);
        }
        assert!(pending.requests.is_empty());
        assert!(pending.dispatch(1, Ok(Reply::Inline(vec![]))).is_err()); // duplicate
        assert!(pending.dispatch(u64::MAX, Ok(Reply::Inline(vec![]))).is_err()); // unknown
    }
    #[test]
    fn requests_wait_for_a_free_slot_instead_of_failing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let registry = Registry::default();
        let (a, _keep_a) = mpsc::sync_channel(1);
        let (b, _keep_b) = mpsc::sync_channel(1);
        assert_eq!(registry.register(2, a).unwrap(), 1);
        assert_eq!(registry.register(2, b).unwrap(), 2);
        // A third caller waits; cancelled while waiting, it registers nothing.
        let cancel = Arc::new(AtomicBool::new(false));
        let waited = std::thread::scope(|scope| {
            let (registry, cancel) = (&registry, &cancel);
            let waiter = scope.spawn(move || {
                let (c, _receiver) = mpsc::sync_channel(1);
                filmcraft_media::cancel::with_cancel(cancel, || registry.register(2, c))
            });
            std::thread::sleep(Duration::from_millis(60));
            assert!(!waiter.is_finished());
            cancel.store(true, Ordering::Relaxed);
            waiter.join().unwrap()
        });
        assert!(matches!(waited, Err(MediaError::Cancelled)));
        assert_eq!(registry.lock().requests.len(), 2);
        // A reply frees a slot and wakes the next waiter.
        let woken = std::thread::scope(|scope| {
            let registry = &registry;
            let waiter = scope.spawn(move || {
                let (c, _receiver) = mpsc::sync_channel(1);
                registry.register(2, c)
            });
            std::thread::sleep(Duration::from_millis(60));
            assert!(!waiter.is_finished());
            registry.dispatch(1, Ok(Reply::Inline(vec![]))).unwrap();
            waiter.join().unwrap()
        });
        assert_eq!(woken.unwrap(), 3);
        // A failed worker wakes waiters with its error instead of leaving them for the deadline.
        let failed = std::thread::scope(|scope| {
            let registry = &registry;
            let waiter = scope.spawn(move || {
                let (c, _receiver) = mpsc::sync_channel(1);
                registry.register(2, c)
            });
            std::thread::sleep(Duration::from_millis(60));
            registry.fail("helper exited");
            waiter.join().unwrap()
        });
        assert!(failed.unwrap_err().to_string().contains("helper exited"));
    }
    #[test]
    fn throughput_is_the_best_recent_in_flight_over_latency() {
        let mut pending = Pending::default();
        assert!(pending.rate().is_none());
        // `outstanding` others are still in flight when the reply arrives (never answered here)
        let mut fillers = Vec::new();
        let answer = |pending: &mut Pending, in_flight: usize, ms: u64, ok: bool| {
            let (send, _receiver) = mpsc::sync_channel(1);
            let id = pending.next + 1;
            pending.next = id;
            let request = PendingRequest { sender: send, started: Instant::now() - Duration::from_millis(ms), in_flight };
            pending.requests.insert(id, request);
            pending.dispatch(id, if ok { Ok(Reply::Inline(vec![])) } else { Err(failure("gpu")) }).unwrap();
        };
        // a helper with four slots: one request at a time (the SDK idles, 220 ms a frame) says
        // nothing about its capacity
        pending.slots = 4;
        for id in 1_000..1_003 {
            let (send, receiver) = mpsc::sync_channel(1);
            fillers.push(receiver);
            pending.requests.insert(id, PendingRequest::new(send));
        }
        for _ in 0..3 {
            answer(&mut pending, 1, 220, true);
        }
        assert!(pending.rate().is_none());
        // four in flight at 160 ms each: 25 fps, once three such replies are in
        answer(&mut pending, 4, 160, true);
        answer(&mut pending, 4, 160, true);
        assert!(pending.rate().is_none());
        for _ in 0..2 {
            answer(&mut pending, 4, 160, true);
        }
        let busy = pending.rate().unwrap();
        assert!((24.0..26.0).contains(&busy), "{busy}");
        // a lull does not lower it; a fast saturated reply moves the mean, not the reading
        answer(&mut pending, 1, 220, true);
        assert_eq!(pending.rate(), Some(busy));
        answer(&mut pending, 4, 120, true);
        let bumped = pending.rate().unwrap();
        assert!((26.0..28.0).contains(&bumped), "{bumped}");
        // failed replies and the start-up reply are not throughput
        answer(&mut pending, 8, 1, false);
        assert_eq!(pending.rate(), Some(bumped));
        // five in flight (one waiting in the helper's pipe) at 200 ms each is the same 25 fps:
        // the wait for a slot is part of the latency, so all five count
        for _ in 0..4 {
            answer(&mut pending, 5, 200, true);
        }
        let piped = pending.rate().unwrap();
        assert!((24.0..27.0).contains(&piped), "{piped}");
        // the window is bounded, and through a long lull the last saturated reading stands
        for _ in 0..(2 * RATE_WINDOW) {
            answer(&mut pending, 2, 100, true);
        }
        assert_eq!(pending.answered.len(), RATE_WINDOW);
        assert_eq!(pending.rate(), Some(piped));
        // a burst into an idle helper: registered with six in flight, but answered with the
        // pipeline drained behind it (a free slot, no queueing), so its 60 fps is not a reading
        for id in 1_000..1_003 {
            pending.requests.remove(&id);
        }
        for _ in 0..3 {
            answer(&mut pending, 6, 100, true);
        }
        assert_eq!(pending.rate(), Some(piped));
        drop(fillers);
    }
    #[test]
    fn development_settings_are_sent_as_small_range_checked_integers() {
        use filmcraft_media::{Hundredths, RawQuality};
        // half space gamma toneMap rolloff peak options flashing | flags iso kelvin tint exposure
        assert_eq!(wire(&RawDevelop::default(), Some(false)).unwrap(), "0 0 0 0 0 0 4 0 0 0 0 0 0", "linear, as shot, the curves as recorded");
        let bt1886 = RawDevelop { gamma: RawGamma::Bt1886, tone_map: RawToneMap::High, rolloff: RawRollOff::VerySoft, ..Default::default() };
        assert_eq!(wire(&bt1886, Some(true)).unwrap(), "1 0 1 4 5 0 4 0 0 0 0 0 0");
        assert_eq!(wire(&bt1886, None).unwrap(), "0 1 4 5 0 4 0 0 0 0 0 0", "an RMD write has no decode quality");
        let pq = RawDevelop {
            color_space: RawColorSpace::Rec2020,
            gamma: RawGamma::St2084,
            hdr_peak_nits: Some(1000),
            chroma_noise_reduction: Some(true),
            flashing_pixel: Some(RawFlashing::Strong),
            metadata_curves: false,
            creative_lut: true,
            cdl: true,
            quality: Some(RawQuality::HalfGood),
            ..Default::default()
        };
        assert_eq!(wire(&pq, Some(false)).unwrap(), "0 1 4 0 0 1000 27 4 0 0 0 0 0");
        let off = RawDevelop { chroma_noise_reduction: Some(false), flashing_pixel: Some(RawFlashing::Off), ..Default::default() };
        assert_eq!(wire(&off, None).unwrap(), "0 0 0 0 0 5 1 0 0 0 0 0", "chroma off is set (1) without its value (2)");
        // every code is within the helper's tables
        for space in RawColorSpace::ALL {
            for gamma in RawGamma::ALL.into_iter().filter(|g| RawDevelop::supports(space, *g)) {
                let fields: Vec<u64> =
                    wire(&RawDevelop { color_space: space, gamma, ..Default::default() }, None).unwrap().split(' ').map(|f| f.parse().unwrap()).collect();
                assert!(fields[0] <= 3 && fields[1] <= 5, "{space:?} {gamma:?}");
            }
        }
        // the RAW adjustments: a flag word, then ISO, Kelvin, tint and exposure in hundredths
        let all = RawDevelop { iso: Some(800), kelvin: Some(4300), tint: Some(Hundredths(-1250)), exposure: Some(Hundredths(67)), ..bt1886 };
        assert_eq!(wire(&all, None).unwrap(), "0 1 4 5 0 4 0 15 800 4300 -1250 67");
        let some = RawDevelop { kelvin: Some(100_000), exposure: Some(Hundredths(-800)), ..Default::default() };
        assert_eq!(wire(&some, None).unwrap(), "0 0 0 0 0 4 0 10 0 100000 0 -800");
        // out-of-range values and unsupported combinations never reach the helper
        for bad in [
            RawDevelop { iso: Some(1250), ..Default::default() },
            RawDevelop { kelvin: Some(1699), ..Default::default() },
            RawDevelop { tint: Some(Hundredths(10_001)), ..Default::default() },
            RawDevelop { exposure: Some(Hundredths(801)), ..Default::default() },
            RawDevelop { hdr_peak_nits: Some(50), ..Default::default() },
            RawDevelop { gamma: RawGamma::Log3G10, ..Default::default() },
            RawDevelop { color_space: RawColorSpace::P3D65, gamma: RawGamma::Hlg, ..Default::default() },
        ] {
            assert!(wire(&bad, None).is_err(), "{bad:?}");
        }
        // frames are linear light in the developed primaries
        assert_eq!(frame_color(&RawDevelop::default()).transfer, Transfer::Linear);
        assert_eq!(frame_color(&bt1886).transfer, Transfer::Linear);
        assert_eq!(frame_color(&pq).primaries, filmcraft_color::Primaries::Bt2020);
        // the decode quality caps a preview's decode, a full-size request is always full
        assert_eq!(capped_divisor(6048, 3402, 0.5, 1), 2);
        assert_eq!(capped_divisor(6048, 3402, 0.5, 4), 4);
        assert_eq!(capped_divisor(6048, 3402, 0.06, 2), 16, "a smaller preview still decodes smaller");
        assert_eq!(capped_divisor(6048, 3402, 0.99, 16), 16);
        assert_eq!(capped_divisor(15, 15, 0.9, 16), 8, "a clip too small for the quality falls back");
        assert_eq!(capped_divisor(1, 1, 0.5, 16), 1);
    }
    #[test]
    fn fatal_helper_error_after_start_up_carries_its_message() {
        let mut pending = Pending::default();
        let error = pending.dispatch(0, Err(failure("decode request exceeds line limit"))).unwrap_err();
        assert!(error.to_string().contains("line limit"), "{error}");
        assert!(pending.dispatch(0, Ok(Reply::Inline(b"FCRD8".to_vec()))).is_err());
    }
    #[test]
    fn deadlines_include_cancelled_requests_and_failure_wakes_every_waiter() {
        let mut pending = Pending::default();
        let (send, cancelled) = mpsc::sync_channel(1);
        pending.requests.insert(1, PendingRequest::new(send));
        drop(cancelled);
        assert!(pending.expired(Duration::ZERO));
        let (send, waiting) = mpsc::sync_channel(1);
        pending.requests.insert(2, PendingRequest::new(send));
        pending.fail("helper exited");
        assert!(waiting.recv_timeout(Duration::from_millis(100)).unwrap().is_err());
        assert!(pending.requests.is_empty());
        assert!(pending.failed.is_some());
    }
    /// A reply header as the helper writes it (FCRD6).
    fn header(status: u32, id: u64, length: u64, slot: u32) -> Vec<u8> {
        let mut packet = status.to_le_bytes().to_vec();
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&length.to_le_bytes());
        packet.extend_from_slice(&slot.to_le_bytes());
        packet
    }
    /// Stands in for the helper's stdin: what the app wrote, readable by the test.
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    /// The helper's side of the shared frames (a section of `slots` pages) and the app's view
    /// of it, with a sink for the app's `F` commands.
    fn shared_frames(tag: &str, slots: u32) -> (shm::TestSection, OnceLock<Arc<SharedFrames>>, Arc<Mutex<Vec<u8>>>) {
        let name = format!("filmcraft-red-test-{tag}-{}", std::process::id());
        let section = shm::TestSection::create(&name, slots as usize * 4096).unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        let mapping = shm::Mapping::open(&name, slots as usize * 4096).unwrap();
        let shared =
            OnceLock::from(Arc::new(SharedFrames { mapping, slots, slot_bytes: 4096, commands: Arc::new(Mutex::new(Box::new(Sink(written.clone())))) }));
        (section, shared, written)
    }
    #[test]
    fn protocol_rejects_truncation_and_oversized_replies() {
        let mut packet = header(0, 42, 3, 0);
        packet.extend_from_slice(b"abc");
        let (id, bytes) = read_reply(&mut &packet[..], &OnceLock::new()).unwrap();
        assert_eq!(id, 42);
        assert_eq!(bytes.unwrap().into_inline().unwrap(), b"abc");
        for end in 0..packet.len() {
            assert!(read_reply(&mut &packet[..end], &OnceLock::new()).is_err());
        }
        for i in 0..packet.len() {
            let mut changed = packet.clone();
            changed[i] ^= 0xff;
            assert!(std::panic::catch_unwind(|| read_reply(&mut &changed[..], &OnceLock::new())).is_ok());
        }
        let mut error_packet = packet.clone();
        error_packet[..4].copy_from_slice(&1u32.to_le_bytes());
        let (id, result) = read_reply(&mut &error_packet[..], &OnceLock::new()).unwrap();
        assert_eq!(id, 42);
        // an error answers its request only: the helper serves on
        assert!(matches!(result, Ok(Reply::Error(ref message)) if message == "abc"));
        // an inline reply names no slot, a status beyond the protocol's is rejected, a shared
        // frame needs shared memory
        let mut slotted = packet.clone();
        slotted[20] = 1;
        assert!(read_reply(&mut &slotted[..], &OnceLock::new()).is_err());
        let mut unknown = packet.clone();
        unknown[0] = 3;
        assert!(read_reply(&mut &unknown[..], &OnceLock::new()).is_err());
        assert!(read_reply(&mut &header(STATUS_SHARED, 42, 3, 0)[..], &OnceLock::new()).is_err());
        packet[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(read_reply(&mut &packet[..], &OnceLock::new()).is_err());
    }
    #[test]
    fn shared_frames_are_read_in_place_and_handed_back() {
        let (section, shared, written) = shared_frames("slots", 2);
        section.write(4096, b"frame one").unwrap();
        section.write(0, b"frame zero").unwrap();
        // the reply carries only the header; the bytes are the slot's, exactly `length` of them
        let (id, reply) = read_reply(&mut &header(STATUS_SHARED, 7, 9, 1)[..], &shared).unwrap();
        assert_eq!(id, 7);
        let Reply::Slot(lease) = reply.unwrap() else { panic!("shared frame expected") };
        assert_eq!(lease.bytes().unwrap(), b"frame one");
        assert!(written.lock().unwrap().is_empty());
        // the helper may change a slot it has not handed over; this one is read only when freed
        section.write(0, b"FRAME ZERO").unwrap();
        drop(lease);
        assert_eq!(std::str::from_utf8(&written.lock().unwrap()).unwrap(), "F 1\n");
        let (_, reply) = read_reply(&mut &header(STATUS_SHARED, 8, 10, 0)[..], &shared).unwrap();
        let Reply::Slot(lease) = reply.unwrap() else { panic!("shared frame expected") };
        assert_eq!(lease.bytes().unwrap(), b"FRAME ZERO");
        // a frame nobody waits for still hands its slot back: dispatched into a dropped
        // channel, or discarded with an unknown id
        let mut pending = Pending::default();
        let (send, receiver) = mpsc::sync_channel(1);
        pending.requests.insert(8, PendingRequest::new(send));
        drop(receiver);
        pending.dispatch(8, Ok(Reply::Slot(lease))).unwrap();
        assert_eq!(std::str::from_utf8(&written.lock().unwrap()).unwrap(), "F 1\nF 0\n");
        // bounds: a slot beyond the count or a length beyond the slot is the helper's fault, and
        // the mapping checks every read against the bytes it mapped
        assert!(read_reply(&mut &header(STATUS_SHARED, 9, 1, 2)[..], &shared).is_err());
        assert!(read_reply(&mut &header(STATUS_SHARED, 9, 4097, 1)[..], &shared).is_err());
        assert!(shared.get().unwrap().mapping.slice(4096, 4097).is_err());
        assert!(shared.get().unwrap().mapping.slice(usize::MAX, 1).is_err());
        assert_eq!(shared.get().unwrap().mapping.slice(8192, 0).unwrap(), b"");
        assert_eq!(shared.get().unwrap().mapping.len(), 8192);
        // audio and metadata never come in a slot
        let (_, reply) = read_reply(&mut &header(STATUS_SHARED, 10, 4, 0)[..], &shared).unwrap();
        assert!(reply.unwrap().into_inline().is_err());
        assert_eq!(std::str::from_utf8(&written.lock().unwrap()).unwrap(), "F 1\nF 0\nF 0\n");
    }
    #[test]
    fn shared_frame_mapping_needs_the_section_at_the_declared_size() {
        let name = format!("filmcraft-red-test-size-{}", std::process::id());
        assert!(shm::Mapping::open(&name, 4096).is_err());
        let section = shm::TestSection::create(&name, 8192).unwrap();
        assert!(shm::Mapping::open(&name, 8192 + 4096).is_err());
        assert!(shm::Mapping::open(&name, 0).is_err());
        assert!(shm::Mapping::open("not a valid name", 4096).is_err());
        let mapping = shm::Mapping::open(&name, 8192).unwrap();
        section.write(8000, b"tail").unwrap();
        assert_eq!(mapping.slice(8000, 4).unwrap(), b"tail");
        // the view outlives the section's creator's handle, as it must outlive a dead helper
        drop(section);
        assert_eq!(mapping.slice(8000, 4).unwrap(), b"tail");
    }
}
