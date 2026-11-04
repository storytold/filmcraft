//! Audio playback over the PulseAudio native protocol, in pure Rust (Linux).
//!
//! Desktop Linux routes audio through PipeWire (whose `pipewire-pulse` speaks this protocol on
//! the same socket) or PulseAudio itself, so one Unix-socket client reaches both, including the
//! sinks that only exist inside the sound server (Bluetooth, virtual sinks), which ALSA device
//! enumeration can never see. cpal's ALSA path through the `pipewire-alsa` bridge also dies
//! unrecoverably on its first underrun (`alsa::poll() returned POLLERR`, storytold/filmcraft#106),
//! so this is the primary output; the caller falls back to cpal when no socket is reachable.
//!
//! Protocol serialization comes from the `pulseaudio` crate (MIT, pure Rust, no C library). The
//! server clocks playback: it sends `Request` messages and we answer with sample data, so there
//! is no real-time callback thread on our side. Everything runs on one worker thread; the
//! handshake uses socket timeouts so a hung daemon fails the stream instead of hanging a caller.
//!
//! When the server goes away mid-stream (`systemctl --user restart pipewire-pulse`, a crashed
//! daemon) the worker ends and hands the mixer callback back ([`Playback::ended`],
//! [`Playback::into_fill`]), so the caller can reconnect and carry on with the same mixer.

#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::io::{BufReader, Cursor, Read};
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use pulseaudio::protocol;

/// Socket operations during connect/handshake fail after this long (a hung daemon).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// A server request larger than this is treated as broken (frames, not bytes).
const MAX_REQUEST_FRAMES: usize = 1 << 20;
/// A server message longer than this is treated as broken, before anything is allocated for it.
const MAX_MESSAGE_BYTES: u32 = 1 << 20;
/// Between timing reports the clock moves on with the wall clock, as libpulse interpolates a
/// stream's position, for at most this long: a server that stops answering stops the clock.
const MAX_INTERPOLATION: Duration = Duration::from_millis(100);
/// Request tags: the sink listing before a stream starts, and the latency queries during it.
const SINKS_TAG: u32 = 3;
const SINK_TAG: u32 = 4;
const LATENCY_TAG: u32 = 100;

/// One output device as the sound server reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sink {
    /// Server-internal name, used to address the sink (`bluez_output.…`, `alsa_output.…`).
    pub name: String,
    /// Human-readable description shown to the user ("WH-1000XM4", "Family 17h HD Audio").
    pub description: String,
    /// Its channel count (2 for stereo, 6 for 5.1, 8 for 7.1).
    pub channels: u8,
    /// Which speaker each of its channels feeds.
    map: protocol::ChannelMap,
}

impl Sink {
    fn from_info(s: protocol::SinkInfo) -> Self {
        let name = s.name.to_string_lossy().into_owned();
        let description = s.description.map(|d| d.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).unwrap_or_else(|| name.clone());
        Sink { name, description, channels: s.channel_map.num_channels(), map: s.channel_map }
    }
}

/// The mixer callback a [`Playback`] plays: `fill(buffer, channels)` with interleaved f32.
pub type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// Whether a PulseAudio-protocol server (PipeWire or PulseAudio) is reachable: its socket exists
/// and accepts a connection. A socket file left behind by a stopped server (`pipewire-pulse`
/// not running) refuses connections, and then the caller should use another output.
pub fn available() -> bool {
    server_socket().is_some_and(|p| reachable(&p))
}

/// The server's Unix socket, found as libpulse finds it: the first `unix:` (or absolute path)
/// entry of `$PULSE_SERVER`, else `$PULSE_RUNTIME_PATH/native`, else
/// `$XDG_RUNTIME_DIR/pulse/native`. A `$PULSE_SERVER` with only network servers (`tcp:host`,
/// `host:port`) gives `None`: this client speaks to Unix sockets only, and the ALSA output, whose
/// `pulse` plugin is libpulse, follows the variable to that server instead.
pub fn server_socket() -> Option<PathBuf> {
    socket_from(|k| std::env::var(k).ok())
}

/// [`server_socket`] with the environment read through `env`.
fn socket_from(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(servers) = env("PULSE_SERVER").filter(|v| !v.trim().is_empty()) {
        let local = servers.split_whitespace().find_map(|entry| {
            // an entry may name the machine it is for: `{machine-id}unix:/path`
            let entry = if entry.starts_with('{') { entry.split_once('}').map_or("", |(_, rest)| rest) } else { entry };
            let path = entry.strip_prefix("unix:").unwrap_or(entry);
            path.starts_with('/').then(|| PathBuf::from(path))
        });
        if local.is_none() {
            log::info!("PULSE_SERVER names no Unix socket ({servers}); not using the native sound-server client");
        }
        return local;
    }
    let runtime = env("PULSE_RUNTIME_PATH").filter(|v| !v.is_empty()).map(PathBuf::from).map(|d| d.join("native"));
    let xdg = env("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("pulse").join("native"));
    runtime.into_iter().chain(xdg).find(|p| p.exists())
}

/// Whether a Unix socket at `path` accepts a connection.
fn reachable(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Connect, authenticate and name the client. All socket operations time out after `timeout`.
fn connect(timeout: Duration) -> Result<(BufReader<UnixStream>, u16), String> {
    let path = server_socket().ok_or("no PipeWire/PulseAudio socket")?;
    connect_at(&path, timeout)
}

/// [`connect`] to the server socket at `path`.
fn connect_at(path: &Path, timeout: Duration) -> Result<(BufReader<UnixStream>, u16), String> {
    handshake(open(path, timeout)?)
}

/// Open the server socket at `path`; its operations time out after `timeout`.
fn open(path: &Path, timeout: Duration) -> Result<UnixStream, String> {
    let stream = UnixStream::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Authenticate and name the client on an open socket.
fn handshake(stream: UnixStream) -> Result<(BufReader<UnixStream>, u16), String> {
    let mut sock = BufReader::new(stream);
    let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
    let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
    protocol::write_command_message(sock.get_mut(), 0, &protocol::Command::Auth(auth), protocol::MAX_VERSION).map_err(|e| e.to_string())?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION).map_err(|e| e.to_string())?;
    let version = protocol::MAX_VERSION.min(reply.version);
    let mut props = protocol::Props::new();
    if let Ok(name) = CString::new("FilmCraft") {
        props.set(protocol::Prop::ApplicationName, name);
    }
    protocol::write_command_message(sock.get_mut(), 1, &protocol::Command::SetClientName(props), version).map_err(|e| e.to_string())?;
    let _ = protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version).map_err(|e| e.to_string())?;
    Ok((sock, version))
}

/// The sound server's output devices, or an error after at most `total`, however the daemon
/// behaves: the listing runs on its own thread, so a caller never waits longer than that.
pub fn list_sinks(total: Duration) -> Result<Vec<Sink>, String> {
    list_sinks_from(None, total)
}

/// [`list_sinks`] from the server socket at `path` (`None`: the one the environment names).
fn list_sinks_from(path: Option<PathBuf>, total: Duration) -> Result<Vec<Sink>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("filmcraft-pulse-list".into())
        .spawn(move || {
            let connected = match &path {
                Some(p) => connect_at(p, total),
                None => connect(total),
            };
            let _ = tx.send(connected.and_then(|(mut sock, version)| sinks_on(&mut sock, version)));
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(total).map_err(|_| "the sound server did not list its outputs in time".to_string())?
}

/// The output devices, asked on an open connection.
fn sinks_on(sock: &mut BufReader<UnixStream>, version: u16) -> Result<Vec<Sink>, String> {
    protocol::write_command_message(sock.get_mut(), SINKS_TAG, &protocol::Command::GetSinkInfoList, version).map_err(|e| e.to_string())?;
    let (_, sinks) = protocol::read_reply_message::<protocol::SinkInfoList>(sock, version).map_err(|e| e.to_string())?;
    Ok(sinks.into_iter().map(Sink::from_info).collect())
}

/// The default output, asked on an open connection; `None` if the server has none or refuses.
fn default_sink_on(sock: &mut BufReader<UnixStream>, version: u16) -> Option<Sink> {
    let ask = protocol::Command::GetSinkInfo(protocol::GetSinkInfo { index: None, name: Some(protocol::DEFAULT_SINK.to_owned()) });
    protocol::write_command_message(sock.get_mut(), SINK_TAG, &ask, version).ok()?;
    let (_, info) = protocol::read_reply_message::<protocol::SinkInfo>(sock, version).ok()?;
    Some(Sink::from_info(info))
}

/// The output `want` names (by server name or description), or the default output when it
/// names none or none matches, asked on an open connection.
fn target_on(sock: &mut BufReader<UnixStream>, version: u16, want: Option<&str>) -> Result<Option<Sink>, String> {
    if let Some(want) = want
        && let Some(sink) = sinks_on(sock, version)?.into_iter().find(|s| s.name == want || s.description == want)
    {
        return Ok(Some(sink));
    }
    Ok(default_sink_on(sock, version))
}

/// The channel count of the output `want` names (or of the default output), or an error after at
/// most `total`, like [`list_sinks`]. Playback mixes for this many channels.
pub fn output_channels(want: Option<String>, total: Duration) -> Result<u8, String> {
    output_channels_from(None, want, total)
}

/// [`output_channels`] from the server socket at `path` (`None`: the one the environment names).
fn output_channels_from(path: Option<PathBuf>, want: Option<String>, total: Duration) -> Result<u8, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("filmcraft-pulse-list".into())
        .spawn(move || {
            let connected = match &path {
                Some(p) => connect_at(p, total),
                None => connect(total),
            };
            let found = connected.and_then(|(mut sock, version)| target_on(&mut sock, version, want.as_deref()));
            let _ = tx.send(found.and_then(|s| s.map(|s| s.channels).filter(|&c| c > 0).ok_or_else(|| "the sound server has no output".to_string())));
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(total).map_err(|_| "the sound server did not describe its output in time".to_string())?
}

/// The speakers a stream of `channels` feeds. The mixer writes a 5.1 mix as L, R, C, LFE, Ls, Rs
/// (and stereo as L, R) into the first channels, so the map names those positions first, taken
/// from the sink's own map (its surrounds may be rear or side speakers), then the sink's other
/// positions. The server remaps by position, so every channel lands on its speaker unmixed.
/// Without the sink's map (or for another channel count) the standard positions are used.
fn stream_map(channels: u8, sink: Option<&protocol::ChannelMap>) -> protocol::ChannelMap {
    use protocol::ChannelPosition as P;
    match channels {
        1 => return protocol::ChannelMap::mono(),
        2 => return protocol::ChannelMap::stereo(),
        _ => {}
    }
    let mixer: [&[P]; 6] = [&[P::FrontLeft], &[P::FrontRight], &[P::FrontCenter], &[P::Lfe], &[P::RearLeft, P::SideLeft], &[P::RearRight, P::SideRight]];
    if let Some(sink) = sink.filter(|m| m.num_channels() == channels) {
        let have: Vec<P> = sink.into_iter().collect();
        let mut order: Vec<P> = Vec::with_capacity(have.len());
        for wanted in mixer {
            if let Some(p) = wanted.iter().find(|p| have.contains(p) && !order.contains(p)) {
                order.push(*p);
            }
        }
        for p in &have {
            if !order.contains(p) {
                order.push(*p);
            }
        }
        // A map naming one position twice is not a speaker layout: use the standard one.
        if order.len() == have.len() {
            return protocol::ChannelMap::new(order);
        }
    }
    let standard = [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight, P::SideLeft, P::SideRight];
    let aux = [
        P::Aux0,
        P::Aux1,
        P::Aux2,
        P::Aux3,
        P::Aux4,
        P::Aux5,
        P::Aux6,
        P::Aux7,
        P::Aux8,
        P::Aux9,
        P::Aux10,
        P::Aux11,
        P::Aux12,
        P::Aux13,
        P::Aux14,
        P::Aux15,
        P::Aux16,
        P::Aux17,
        P::Aux18,
        P::Aux19,
        P::Aux20,
        P::Aux21,
        P::Aux22,
        P::Aux23,
    ];
    protocol::ChannelMap::new(standard.into_iter().chain(aux).take(usize::from(channels)))
}

/// A playback stream. Construction returns immediately; the connection, stream setup and the
/// request/answer loop all run on the worker thread, and failures set [`Playback::failed`].
pub struct Playback {
    /// Frames handed to the server.
    sent: Arc<AtomicU64>,
    /// Frames heard: the worker publishes it from each timing report, and it never goes back.
    heard: Arc<AtomicU64>,
    /// When the last report came in, in nanoseconds since `epoch` (0: none yet).
    reported_at: Arc<AtomicU64>,
    epoch: Instant,
    rate: u32,
    /// The highest clock reading given out, so interpolation never moves it back.
    shown: AtomicU64,
    failed: Arc<AtomicBool>,
    /// The worker has returned: the stream is over (stopped, or the server went away).
    ended: Arc<AtomicBool>,
    /// The server created the stream (it may have ended since).
    created: Arc<AtomicBool>,
    /// The channel count of the output the stream plays on, once the worker has looked it up
    /// (0 before), for the caller's next stream.
    sink_channels: Arc<AtomicU8>,
    /// The worker's socket, kept for `shutdown` so `stop` can unblock a waiting read.
    socket: Arc<Mutex<Option<UnixStream>>>,
    /// Returns the mixer callback when it ends, unless the callback panicked.
    thread: Option<std::thread::JoinHandle<Option<Fill>>>,
}

impl Playback {
    /// Start playing: `fill(buffer, channels)` is called on the worker thread with interleaved
    /// f32 whenever the server asks for more data. `sink` names an output by its server name or
    /// its description; it is looked up on the worker, and `None` (or no match) follows the
    /// default output. `request_frames` sizes the server's requests (the I/O buffer
    /// size preference).
    pub fn start(rate: u32, channels: u8, sink: Option<String>, request_frames: u32, fill: Fill) -> Result<Self, String> {
        Self::spawn(None, rate, channels, sink, request_frames, fill)
    }

    /// [`Playback::start`] on the server socket at `path` (`None`: the one the environment names).
    fn spawn(path: Option<PathBuf>, rate: u32, channels: u8, sink: Option<String>, request_frames: u32, mut fill: Fill) -> Result<Self, String> {
        if rate == 0 || channels == 0 {
            return Err("invalid sample rate or channel count".into());
        }
        let sent = Arc::new(AtomicU64::new(0));
        let heard = Arc::new(AtomicU64::new(0));
        let reported_at = Arc::new(AtomicU64::new(0));
        let epoch = Instant::now();
        let failed = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        let created = Arc::new(AtomicBool::new(false));
        let sink_channels = Arc::new(AtomicU8::new(0));
        let socket = Arc::new(Mutex::new(None));
        let thread = {
            let (sent, heard, failed, ended, socket) = (sent.clone(), heard.clone(), failed.clone(), ended.clone(), socket.clone());
            let (created, sink_channels, reported_at) = (created.clone(), sink_channels.clone(), reported_at.clone());
            std::thread::Builder::new()
                .name("filmcraft-pulse".into())
                .spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let counters =
                            Counters { sent: &sent, heard: &heard, created: &created, sink_channels: &sink_channels, reported_at: &reported_at, epoch };
                        run(path, rate, channels, sink, request_frames, &mut fill, &counters, &failed, &socket)
                    }));
                    let fill = match result {
                        Ok(Ok(())) => Some(fill),
                        Ok(Err(e)) => {
                            // `Drop` sets `failed` before it shuts the socket: then this is a stop.
                            if !failed.swap(true, Ordering::SeqCst) && !e.is_empty() {
                                log::warn!("pulse playback: {e}");
                            }
                            Some(fill)
                        }
                        // The mixer panicked: it cannot be trusted with another stream.
                        Err(_) => {
                            failed.store(true, Ordering::SeqCst);
                            log::warn!("pulse playback: the audio mixer failed; playing without sound");
                            None
                        }
                    };
                    ended.store(true, Ordering::SeqCst);
                    fill
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self { sent, heard, reported_at, epoch, rate, shown: AtomicU64::new(0), failed, ended, created, sink_channels, socket, thread: Some(thread) })
    }

    /// Frames heard since `start`: those handed to the server, less those still in its buffer
    /// and the sink's latency, from the server's timing reports. `None` once the stream has
    /// failed, so the caller's clock falls back to the wall clock.
    pub fn played_frames(&self) -> Option<u64> {
        (!self.failed.load(Ordering::SeqCst)).then(|| self.heard_frames())
    }

    /// [`Playback::played_frames`], also after the stream ended.
    pub fn heard_frames(&self) -> u64 {
        self.heard.load(Ordering::SeqCst)
    }

    /// The playback clock: [`Playback::heard_frames`] moved on with the wall clock since the last
    /// report (at most [`MAX_INTERPOLATION`], and never past the frames sent), so the picture
    /// does not trail the sound by the time between reports. Never goes back. `None` once the
    /// stream has failed; an ended stream holds its last report.
    pub fn clock_frames(&self) -> Option<u64> {
        if self.failed.load(Ordering::SeqCst) {
            return None;
        }
        let heard = self.heard_frames();
        let at = self.reported_at.load(Ordering::SeqCst);
        if at == 0 || self.ended() {
            return Some(self.shown.fetch_max(heard, Ordering::SeqCst).max(heard));
        }
        let since = self.epoch.elapsed().saturating_sub(Duration::from_nanos(at)).min(MAX_INTERPOLATION);
        let ahead = u64::try_from(since.as_nanos().saturating_mul(u128::from(self.rate)) / 1_000_000_000).unwrap_or(0);
        let now = heard.saturating_add(ahead).min(self.frames_sent()).max(heard);
        Some(self.shown.fetch_max(now, Ordering::SeqCst).max(now))
    }

    /// Frames handed to the server since `start`, also after the stream ended.
    pub fn frames_sent(&self) -> u64 {
        self.sent.load(Ordering::SeqCst)
    }

    /// Whether the stream is over: the server closed the connection, refused it, or reported
    /// an error. [`Playback::into_fill`] then returns the mixer for a new stream.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Whether the server created the stream. A stream that ended before then never played:
    /// the server refused it, or its output was gone.
    pub fn created(&self) -> bool {
        self.created.load(Ordering::SeqCst)
    }

    /// The channel count of the output this stream plays on, once known.
    pub fn sink_channels(&self) -> Option<u8> {
        Some(self.sink_channels.load(Ordering::SeqCst)).filter(|&c| c > 0)
    }

    /// Stop the stream and take back its mixer callback (`None` if the callback panicked).
    pub fn into_fill(mut self) -> Option<Fill> {
        self.shutdown();
        self.thread.take().and_then(|t| t.join().ok().flatten())
    }

    /// Mark the stream dead so the worker treats the socket teardown as an ordinary exit, then
    /// unblock any read it is sitting in.
    fn shutdown(&self) {
        self.failed.store(true, Ordering::SeqCst);
        if let Some(sock) = self.socket.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The worker: handshake, stream creation, then answer the server's requests until the socket
/// is shut down by [`Playback::drop`] or an error ends the stream. A connection the server
/// closes is an error (the caller may reconnect); a shutdown from `Drop` is not, and `Drop`
/// marks it beforehand so it is not reported.
fn run(
    path: Option<PathBuf>,
    rate: u32,
    channels: u8,
    sink: Option<String>,
    request_frames: u32,
    fill: &mut Fill,
    counters: &Counters<'_>,
    stopped: &AtomicBool,
    shared_socket: &Mutex<Option<UnixStream>>,
) -> Result<(), String> {
    let path = match path {
        Some(p) => p,
        None => server_socket().ok_or("no PipeWire/PulseAudio socket")?,
    };
    let stream = open(&path, HANDSHAKE_TIMEOUT)?;
    // Publish the socket before the handshake, so a stop can always shut it down and never waits
    // on a slow server. `shutdown` marks the stream stopped before it takes the lock, so a stop
    // that came first is seen here, and one that comes later finds the socket.
    {
        let mut slot = shared_socket.lock().unwrap_or_else(PoisonError::into_inner);
        if stopped.load(Ordering::SeqCst) {
            return Err(String::new());
        }
        *slot = Some(stream.try_clone().map_err(|e| e.to_string())?);
    }
    let (mut sock, version) = handshake(stream)?;
    // The chosen output (or the default one), looked up here rather than on the caller's
    // thread, for its name and its speakers. A stream for the default output is created on
    // `@DEFAULT_SINK@`, so the server keeps it on whatever the default is.
    let named = match sink {
        Some(want) => sinks_on(&mut sock, version)?.into_iter().find(|s| s.name == want || s.description == want),
        None => None,
    };
    let target = match &named {
        Some(s) => Some(s.clone()),
        None => default_sink_on(&mut sock, version),
    };
    if let Some(t) = &target {
        counters.sink_channels.store(t.channels, Ordering::SeqCst);
    }
    let channel_map = stream_map(channels, target.as_ref().map(|t| &t.map));
    let sink = named.map(|s| s.name);
    let stride = usize::from(channels) * 4;
    let request_bytes = request_frames.saturating_mul(stride as u32).max(256);
    let params = protocol::PlaybackStreamParams {
        sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels, sample_rate: rate },
        channel_map,
        sink_name: sink.and_then(|s| CString::new(s).ok()).or_else(|| Some(protocol::DEFAULT_SINK.to_owned())),
        buffer_attr: protocol::stream::BufferAttr {
            max_length: u32::MAX,
            target_length: request_bytes.saturating_mul(4),
            pre_buffering: u32::MAX,
            minimum_request_length: request_bytes,
            fragment_size: u32::MAX,
        },
        ..Default::default()
    };
    protocol::write_command_message(sock.get_mut(), 10, &protocol::Command::CreatePlaybackStream(params), version).map_err(|e| e.to_string())?;
    let (_, info) = protocol::read_reply_message::<protocol::CreatePlaybackStreamReply>(&mut sock, version).map_err(|e| e.to_string())?;
    counters.created.store(true, Ordering::SeqCst);

    // The stream is set up: drop the timeouts. From here the server paces us, and a stop shuts
    // the published socket down, which unblocks any waiting read.
    sock.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
    sock.get_ref().set_write_timeout(None).map_err(|e| e.to_string())?;

    let mut scratch: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut send = |sock: &mut BufReader<UnixStream>, length: usize| -> Result<(), String> {
        let frames = (length / stride).clamp(1, MAX_REQUEST_FRAMES);
        let samples = frames * usize::from(channels);
        if scratch.try_reserve(samples.saturating_sub(scratch.len())).is_err() || bytes.try_reserve((samples * 4).saturating_sub(bytes.len())).is_err() {
            return Err("audio buffer allocation failed".into());
        }
        scratch.resize(samples, 0.0);
        fill(&mut scratch[..samples], usize::from(channels));
        bytes.clear();
        for sample in &scratch[..samples] {
            let v = if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 };
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        protocol::write_memblock(sock.get_mut(), info.channel, &bytes, 0).map_err(|e| e.to_string())?;
        counters.sent.fetch_add(frames as u64, Ordering::SeqCst);
        Ok(())
    };
    // One latency query at a time, after a block is sent.
    let mut asked = false;
    let ask_latency = |sock: &mut BufReader<UnixStream>, asked: &mut bool| -> Result<(), String> {
        if !*asked {
            let query = protocol::Command::GetPlaybackLatency(protocol::LatencyParams { channel: info.channel, now: SystemTime::now() });
            protocol::write_command_message(sock.get_mut(), LATENCY_TAG, &query, version).map_err(|e| e.to_string())?;
            *asked = true;
        }
        Ok(())
    };

    // The reply told us how much to send before playback starts.
    send(&mut sock, info.requested_bytes as usize)?;
    ask_latency(&mut sock, &mut asked)?;
    let mut warned_underflow = false;
    loop {
        // A shutdown from `stop` also surfaces here as an error; `Drop` has marked it, so the
        // caller does not report it.
        let msg = match read_message(&mut sock, version)? {
            Message::Latency(latency) => {
                asked = false;
                // One value for readers, and never backwards: a report with more latency than the
                // last holds the clock still instead of moving it back.
                let heard = heard_frames(&latency, rate, stride).min(counters.sent.load(Ordering::SeqCst));
                counters.heard.fetch_max(heard, Ordering::SeqCst);
                let at = u64::try_from(counters.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX).max(1);
                counters.reported_at.store(at, Ordering::SeqCst);
                continue;
            }
            Message::Data => continue,
            Message::Command(cmd) => *cmd,
        };
        match msg {
            protocol::Command::Request(protocol::Request { channel, length }) if channel == info.channel => {
                send(&mut sock, length as usize)?;
                ask_latency(&mut sock, &mut asked)?;
            }
            // Underruns are survivable here, unlike on the ALSA bridge: the server keeps
            // requesting and playback continues, so note the first one and carry on.
            protocol::Command::Underflow(_) if !warned_underflow => {
                warned_underflow = true;
                log::debug!("pulse playback: underflow");
            }
            // The server ends the stream on its own when its sink goes away or the stream is moved
            // and cannot follow; nothing else will come on this channel.
            protocol::Command::PlaybackStreamKilled(channel) if channel == info.channel => {
                return Err("the sound server ended the stream".into());
            }
            protocol::Command::Error(e) => return Err(format!("server error: {e:?}")),
            _ => {}
        }
    }
}

/// The counters a stream shares with its [`Playback`].
struct Counters<'a> {
    sent: &'a AtomicU64,
    heard: &'a AtomicU64,
    reported_at: &'a AtomicU64,
    epoch: Instant,
    created: &'a AtomicBool,
    sink_channels: &'a AtomicU8,
}

/// One message from the server.
enum Message {
    Command(Box<protocol::Command>),
    /// The reply to a latency query.
    Latency(protocol::PlaybackLatency),
    /// A memblock, which a playback stream does not expect; skipped.
    Data,
}

/// Read one message whole. Its length is checked before anything is allocated, and it is parsed
/// from its own buffer, so a broken or hostile message ends the stream instead of running the
/// reader off the message boundaries.
fn read_message(sock: &mut BufReader<UnixStream>, version: u16) -> Result<Message, String> {
    let closed = |e: &dyn std::fmt::Display| format!("the sound server closed the connection: {e}");
    let desc = protocol::read_descriptor(sock).map_err(|e| closed(&e))?;
    if desc.length > MAX_MESSAGE_BYTES {
        return Err(format!("the sound server sent a {} byte message", desc.length));
    }
    let mut body = vec![0; desc.length as usize];
    sock.read_exact(&mut body).map_err(|e| closed(&e))?;
    if desc.channel != u32::MAX {
        return Ok(Message::Data);
    }
    let unreadable = |e: protocol::ProtocolError| format!("unreadable message from the sound server: {e}");
    let mut body = Cursor::new(body);
    let (tag, cmd) = protocol::Command::read_tag_prefixed(&mut body, version).map_err(unreadable)?;
    if matches!(cmd, protocol::Command::Reply) && tag == LATENCY_TAG {
        let latency = protocol::TagStructReader::new(&mut body, version).read::<protocol::PlaybackLatency>().map_err(unreadable)?;
        return Ok(Message::Latency(latency));
    }
    Ok(Message::Command(Box::new(cmd)))
}

/// Frames a latency report says are heard: those the sink has read from the stream, less the
/// sink's own latency, as libpulse computes a stream's position. The read position is the
/// server's own, so it does not depend on which of our blocks had reached the server when it
/// answered (subtracting the buffered data from the frames sent counted the blocks sent after the
/// query twice: the server had them, and the clock ran about 25 ms behind the sound).
fn heard_frames(latency: &protocol::PlaybackLatency, rate: u32, stride: usize) -> u64 {
    let read = u64::try_from(latency.read_offset).unwrap_or(0) / stride.max(1) as u64;
    let in_sink = latency.sink_usec.saturating_mul(u64::from(rate)) / 1_000_000;
    read.saturating_sub(in_sink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    #[test]
    fn the_server_socket_is_found_as_libpulse_finds_it() {
        let dir = std::env::temp_dir().join(format!("fc-pulse-env-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pulse")).unwrap();
        std::fs::write(dir.join("pulse/native"), b"").unwrap();
        std::fs::write(dir.join("native"), b"").unwrap();
        let d = dir.to_string_lossy().into_owned();
        let env =
            |pairs: &'static [(&'static str, &'static str)], d: String| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.replace("$D", &d));
        let at = |p: &str| Some(PathBuf::from(p.replace("$D", &d)));
        assert_eq!(socket_from(env(&[("XDG_RUNTIME_DIR", "$D")], d.clone())), at("$D/pulse/native"));
        assert_eq!(socket_from(env(&[("PULSE_RUNTIME_PATH", "$D"), ("XDG_RUNTIME_DIR", "/nowhere")], d.clone())), at("$D/native"));
        // a runtime path without a socket goes on to the next place, instead of stopping
        assert_eq!(socket_from(env(&[("PULSE_RUNTIME_PATH", "/nowhere"), ("XDG_RUNTIME_DIR", "$D")], d.clone())), at("$D/pulse/native"));
        // PULSE_SERVER: unix:, a bare path, a machine-id prefix, the first local entry of a list
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "unix:/run/other/native"), ("XDG_RUNTIME_DIR", "$D")], d.clone())), at("/run/other/native"));
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "/run/other/native")], d.clone())), at("/run/other/native"));
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "{abc123}unix:/run/x")], d.clone())), at("/run/x"));
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "tcp:10.0.0.2:4713 unix:/run/x")], d.clone())), at("/run/x"));
        // only network servers: none, rather than the local socket the variable points away from
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "tcp:10.0.0.2:4713"), ("XDG_RUNTIME_DIR", "$D")], d.clone())), None);
        assert_eq!(socket_from(env(&[("PULSE_SERVER", "media-box"), ("XDG_RUNTIME_DIR", "$D")], d.clone())), None);
        assert_eq!(socket_from(env(&[], d.clone())), None, "nothing set");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A socket path of its own under the temp dir (Unix socket paths are short).
    fn socket_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("fc-pulse-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_stale_socket_file_is_not_a_reachable_server() {
        let path = socket_path("stale");
        assert!(!reachable(&path), "no socket file");
        let listener = UnixListener::bind(&path).unwrap();
        assert!(reachable(&path), "a listening server");
        // The server stops but its socket file stays, as `pipewire-pulse` leaves it when stopped.
        drop(listener);
        assert!(path.exists());
        assert!(!reachable(&path), "a stale socket file refuses connections");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stream_the_server_drops_ends_and_returns_its_mixer() {
        let path = socket_path("drop");
        let listener = UnixListener::bind(&path).unwrap();
        // The server accepts the connection, then goes away (a restarted pipewire-pulse).
        let server = std::thread::spawn(move || drop(listener.accept()));
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();
        let fill: Fill = Box::new(move |buf: &mut [f32], _| {
            buf.fill(0.25);
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !playback.ended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(playback.ended(), "the worker must notice the closed connection");
        assert_eq!(playback.played_frames(), None, "a dead stream gives up the clock");
        let mut fill = playback.into_fill().expect("the mixer survives a dropped connection");
        let mut buf = [0.0; 4];
        fill(&mut buf, 2);
        assert_eq!(buf, [0.25; 4]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stream_the_server_kills_ends_and_returns_its_mixer() {
        // The server keeps the connection open after killing the stream, so only the message can
        // end it: before, it fell through to the catch-all arm and the clock froze.
        let path = socket_path("killed");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || serve_one_stream_then_kill(listener));
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !playback.ended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(playback.ended(), "a killed stream ends");
        assert_eq!(playback.played_frames(), None);
        assert!(playback.into_fill().is_some(), "the mixer comes back for a new stream");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    type Sock = BufReader<UnixStream>;
    const V: u16 = protocol::MAX_VERSION;

    /// Accepts one client and answers its handshake.
    fn handshake(listener: &UnixListener) -> Sock {
        let (stream, _) = listener.accept().unwrap();
        let mut sock = BufReader::new(stream);
        let (seq, _) = protocol::read_command_message(&mut sock, V).unwrap();
        protocol::write_reply_message(sock.get_mut(), seq, &protocol::AuthReply { version: V, ..Default::default() }, V).unwrap();
        let (seq, _) = protocol::read_command_message(&mut sock, V).unwrap();
        protocol::write_reply_message(sock.get_mut(), seq, &protocol::SetClientNameReply { client_id: 1 }, V).unwrap();
        sock
    }

    /// A sink with these speakers.
    fn sink_info(name: &str, positions: &[protocol::ChannelPosition]) -> protocol::SinkInfo {
        let map = protocol::ChannelMap::new(positions.iter().copied());
        protocol::SinkInfo {
            name: CString::new(name).unwrap(),
            sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels: map.num_channels(), sample_rate: 48_000 },
            channel_map: map,
            ..Default::default()
        }
    }

    /// The next command, after answering a default-output lookup with a stereo sink.
    fn next_command(sock: &mut Sock) -> (u32, protocol::Command) {
        use protocol::ChannelPosition as P;
        let (seq, cmd) = protocol::read_command_message(sock, V).unwrap();
        if !matches!(cmd, protocol::Command::GetSinkInfo(_)) {
            return (seq, cmd);
        }
        protocol::write_reply_message(sock.get_mut(), seq, &sink_info("alsa_output.default", &[P::FrontLeft, P::FrontRight]), V).unwrap();
        protocol::read_command_message(sock, V).unwrap()
    }

    /// Answers stream creation, asking for `requested_bytes` up front, and takes that first block.
    fn create_stream(sock: &mut Sock, requested_bytes: u32) {
        let (seq, _) = next_command(sock);
        let reply = protocol::CreatePlaybackStreamReply { requested_bytes, ..Default::default() };
        protocol::write_reply_message(sock.get_mut(), seq, &reply, V).unwrap();
        let desc = protocol::read_descriptor(sock).unwrap();
        let mut block = vec![0; desc.length as usize];
        sock.read_exact(&mut block).unwrap();
    }

    /// Answers one client's handshake and stream creation and takes the first block of samples;
    /// returns the connection.
    fn serve_one_stream(listener: UnixListener) -> Sock {
        let mut sock = handshake(&listener);
        create_stream(&mut sock, 4096);
        sock
    }

    /// Holds the connection open until the client hangs up.
    fn hold(mut sock: Sock) {
        let mut rest = Vec::new();
        let _ = sock.read_to_end(&mut rest);
    }

    /// A raw control message: a descriptor claiming `length` bytes, then `payload`.
    fn write_raw(sock: &mut Sock, length: u32, payload: &[u8]) {
        let mut desc = Vec::with_capacity(20);
        desc.extend_from_slice(&length.to_be_bytes());
        desc.extend_from_slice(&u32::MAX.to_be_bytes());
        desc.extend_from_slice(&0u64.to_be_bytes());
        desc.extend_from_slice(&0u32.to_be_bytes());
        std::io::Write::write_all(sock.get_mut(), &desc).unwrap();
        std::io::Write::write_all(sock.get_mut(), payload).unwrap();
    }

    /// Waits up to 10 s for `done`.
    fn wait_for(done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    /// A stream the server ends with `misbehave` after its first block, holding the connection
    /// open, so only the message can end it: the stream must end and return its mixer.
    fn ends_on(name: &str, misbehave: fn(&mut Sock)) {
        let path = socket_path(name);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut sock = serve_one_stream(listener);
            misbehave(&mut sock);
            hold(sock);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.ended()), "{name}: the stream ends");
        assert_eq!(playback.played_frames(), None, "{name}");
        assert!(playback.into_fill().is_some(), "{name}: the mixer comes back for a new stream");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_message_longer_than_the_cap_ends_the_stream_before_anything_is_allocated() {
        ends_on("oversized", |sock| write_raw(sock, u32::MAX - 15, &[]));
        // a descriptor claiming u32::MAX bytes, and one byte over the cap
        ends_on("oversized-max", |sock| write_raw(sock, u32::MAX, &[]));
        ends_on("oversized-cap", |sock| write_raw(sock, MAX_MESSAGE_BYTES + 1, &[]));
    }

    #[test]
    fn a_message_that_does_not_parse_ends_the_stream() {
        ends_on("junk", |sock| write_raw(sock, 8, &[0xff; 8]));
    }

    #[test]
    fn a_server_that_stops_answering_the_handshake_ends_the_stream() {
        let path = socket_path("silent");
        let listener = UnixListener::bind(&path).unwrap();
        // Accepts, then never answers: a hung daemon.
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(HANDSHAKE_TIMEOUT + Duration::from_secs(2));
            drop(stream);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
        let started = Instant::now();
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.ended()), "the stream ends");
        assert!(started.elapsed() < HANDSHAKE_TIMEOUT + Duration::from_secs(1), "within the handshake timeout");
        assert!(playback.into_fill().is_some());
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stop_during_the_handshake_does_not_wait_for_the_server() {
        // Before, the socket was published only after the handshake: a stop during it closed
        // nothing, the worker then went on into the request loop, and the stop waited on it for
        // good, freezing the caller.
        let path = socket_path("early-stop");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // A slow server: it answers the handshake only after the stop.
            std::thread::sleep(Duration::from_millis(600));
            let mut sock = BufReader::new(stream);
            let _ = protocol::read_command_message(&mut sock, V);
            let _ = protocol::write_reply_message(sock.get_mut(), 0, &protocol::AuthReply { version: V, ..Default::default() }, V);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let stopping = Instant::now();
        drop(playback);
        assert!(stopping.elapsed() < Duration::from_millis(400), "the stop shuts the socket instead of waiting on the server");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn listing_outputs_on_a_server_that_stops_answering_gives_up_in_time() {
        let path = socket_path("list");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });
        let started = Instant::now();
        assert!(list_sinks_from(Some(path.clone()), Duration::from_millis(300)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1), "the caller waits at most the total it asked for");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_clock_counts_frames_heard_not_frames_sent() {
        // Before, the clock was the frames handed to the server, so it ran ahead of the sound by
        // the server's buffer and the sink's latency, and the picture led the audio.
        let path = socket_path("latency");
        let listener = UnixListener::bind(&path).unwrap();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            create_stream(&mut sock, 32_768);
            let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
            assert!(matches!(cmd, protocol::Command::GetPlaybackLatency(_)), "the client asks after its first block");
            wait.recv().unwrap();
            // 8192 bytes still in the buffer (1024 stereo f32 frames) and 10 ms in the sink (480).
            let latency = protocol::PlaybackLatency {
                sink_usec: 10_000,
                source_usec: 0,
                playing: true,
                local_time: SystemTime::UNIX_EPOCH,
                remote_time: SystemTime::UNIX_EPOCH,
                write_offset: 32_768,
                read_offset: 24_576,
                underrun_for: 0,
                playing_for: 0,
            };
            protocol::write_reply_message(sock.get_mut(), seq, &latency, V).unwrap();
            // One more block of 512 frames, then a report with more latency: 1536 buffered, 960 in
            // the sink, so 4608 - 2496 = 2112 heard, less than before.
            let request = |sock: &mut Sock| {
                let more = protocol::Command::Request(protocol::Request { channel: 0, length: 4096 });
                protocol::write_command_message(sock.get_mut(), u32::MAX, &more, V).unwrap();
                let desc = protocol::read_descriptor(sock).unwrap();
                let mut block = vec![0; desc.length as usize];
                sock.read_exact(&mut block).unwrap();
            };
            request(&mut sock);
            let (seq, _) = protocol::read_command_message(&mut sock, V).unwrap();
            let later = protocol::PlaybackLatency { sink_usec: 20_000, write_offset: 36_864, read_offset: 24_576, ..latency };
            protocol::write_reply_message(sock.get_mut(), seq, &later, V).unwrap();
            // A further block: once the client has sent it, it has read the report before it.
            request(&mut sock);
            hold(sock);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.frames_sent() == 4096), "the first block is 4096 frames");
        assert_eq!(playback.played_frames(), Some(0), "nothing is heard before the server reports");
        go.send(()).unwrap();
        assert!(wait_for(|| playback.played_frames() == Some(4096 - 1024 - 480)), "heard = sent - buffered - sink latency");
        assert!(wait_for(|| playback.frames_sent() == 5120));
        assert_eq!(playback.played_frames(), Some(2592), "more latency holds the clock still instead of moving it back");
        drop(playback);
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_chosen_output_is_looked_up_on_the_stream_s_own_connection() {
        // Before, the caller listed the outputs itself to map the chosen one to a sink, on the UI
        // thread, with a connection of its own.
        for (want, expect) in [("Speakers", "alsa_output.speakers"), ("alsa_output.speakers", "alsa_output.speakers"), ("Gone", "@DEFAULT_SINK@")] {
            let path = socket_path("lookup");
            let listener = UnixListener::bind(&path).unwrap();
            let server = std::thread::spawn(move || {
                let mut sock = handshake(&listener);
                let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
                assert!(matches!(cmd, protocol::Command::GetSinkInfoList));
                let speakers = protocol::SinkInfo {
                    name: CString::new("alsa_output.speakers").unwrap(),
                    description: Some(CString::new("Speakers").unwrap()),
                    ..Default::default()
                };
                protocol::write_reply_message(sock.get_mut(), seq, &vec![speakers], V).unwrap();
                let (_, cmd) = next_command(&mut sock);
                let protocol::Command::CreatePlaybackStream(params) = cmd else { panic!("expected stream creation") };
                params.sink_name.map(|n| n.to_string_lossy().into_owned())
            });
            let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
            let playback = Playback::spawn(Some(path.clone()), 48_000, 2, Some(want.into()), 512, fill).unwrap();
            assert_eq!(server.join().unwrap().as_deref(), Some(expect), "{want}");
            assert!(wait_for(|| playback.ended()));
            drop(playback);
            let _ = std::fs::remove_file(&path);
        }
    }

    /// One stream, then the connection closes (a restarted `pipewire-pulse`).
    fn serve_one_stream_then_close(listener: UnixListener) {
        drop(serve_one_stream(listener));
    }

    /// One stream, then the server kills it and holds the connection until the client hangs up.
    fn serve_one_stream_then_kill(listener: UnixListener) {
        let mut sock = serve_one_stream(listener);
        let killed = protocol::Command::PlaybackStreamKilled(0);
        protocol::write_command_message(sock.get_mut(), u32::MAX, &killed, V).unwrap();
        hold(sock);
    }

    #[test]
    fn a_running_stream_the_server_drops_ends_and_returns_its_mixer() {
        // Before the fix the worker took a closed connection for a normal stop: the stream never
        // ended, its clock froze, and the mixer was gone with the thread, so playback could not
        // reconnect after `systemctl --user restart pipewire-pulse`.
        let path = socket_path("running");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || serve_one_stream_then_close(listener));
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !playback.ended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(playback.ended(), "a closed connection ends the stream");
        assert_eq!(playback.frames_sent(), 512, "the first 4096 bytes are 512 stereo f32 frames");
        assert_eq!(playback.played_frames(), None);
        assert!(playback.into_fill().is_some(), "the mixer comes back for a new stream");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_refused_connection_ends_the_stream_without_losing_the_mixer() {
        let path = socket_path("refused");
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
        let playback = Playback::spawn(Some(path), 48_000, 2, None, 512, fill).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !playback.ended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(playback.ended());
        assert!(playback.into_fill().is_some());
    }

    #[test]
    fn the_stream_s_speakers_follow_the_sink_in_the_mixer_s_order() {
        use protocol::ChannelPosition as P;
        let map = |p: &[P]| protocol::ChannelMap::new(p.iter().copied());
        let positions = |m: protocol::ChannelMap| m.into_iter().collect::<Vec<_>>();
        // The mixer writes 5.1 as L, R, C, LFE, Ls, Rs.
        let pipewire_51 = [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::SideLeft, P::SideRight];
        assert_eq!(positions(stream_map(6, Some(&map(&pipewire_51)))), pipewire_51, "PipeWire's 5.1 (side surrounds)");
        let alsa_51 = [P::FrontLeft, P::FrontRight, P::RearLeft, P::RearRight, P::FrontCenter, P::Lfe];
        assert_eq!(
            positions(stream_map(6, Some(&map(&alsa_51)))),
            [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight],
            "ALSA's 5.1 order"
        );
        let s71 = [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight, P::SideLeft, P::SideRight];
        assert_eq!(positions(stream_map(8, Some(&map(&s71)))), s71, "7.1: the 5.1 mix on the front, centre, LFE and rear, the sides last");
        let quad = [P::FrontLeft, P::FrontRight, P::RearLeft, P::RearRight];
        assert_eq!(positions(stream_map(4, Some(&map(&quad)))), quad, "stereo plays on the front pair");
        let interface = [P::Aux0, P::Aux1, P::Aux2, P::Aux3, P::Aux4, P::Aux5];
        assert_eq!(positions(stream_map(6, Some(&map(&interface)))), interface, "an audio interface's own channel order");
        // Without the sink's map, or for another channel count, the standard positions.
        assert_eq!(positions(stream_map(6, None)), [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight]);
        assert_eq!(positions(stream_map(6, Some(&map(&s71)))).len(), 6);
        assert_eq!(positions(stream_map(2, Some(&map(&s71)))), [P::FrontLeft, P::FrontRight]);
        assert_eq!(positions(stream_map(1, None)), [P::Mono]);
        assert_eq!(stream_map(32, None).num_channels(), 32);
        let twice = [P::FrontLeft, P::FrontLeft, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight];
        assert_eq!(positions(stream_map(6, Some(&map(&twice)))), positions(stream_map(6, None)), "a map that names a speaker twice");
    }

    #[test]
    fn a_stream_on_a_surround_output_reports_its_channels_and_maps_its_speakers() {
        // Before, every stream was stereo: a 5.1 output got a downmix and Settings showed 2.
        use protocol::ChannelPosition as P;
        let path = socket_path("surround");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
            let protocol::Command::GetSinkInfo(ask) = cmd else { panic!("expected the default output lookup") };
            assert_eq!(ask.name.as_deref(), Some(protocol::DEFAULT_SINK));
            let alsa_51 = [P::FrontLeft, P::FrontRight, P::RearLeft, P::RearRight, P::FrontCenter, P::Lfe];
            protocol::write_reply_message(sock.get_mut(), seq, &sink_info("alsa_output.surround51", &alsa_51), V).unwrap();
            let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
            let protocol::Command::CreatePlaybackStream(params) = cmd else { panic!("expected stream creation") };
            let reply = protocol::CreatePlaybackStreamReply { requested_bytes: 6 * 4 * 256, ..Default::default() };
            protocol::write_reply_message(sock.get_mut(), seq, &reply, V).unwrap();
            hold(sock);
            params
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 6, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.created()), "the stream is created");
        assert_eq!(playback.sink_channels(), Some(6), "the output has six channels");
        drop(playback);
        let params = server.join().unwrap();
        assert_eq!(params.sample_spec.channels, 6);
        assert_eq!(params.channel_map.into_iter().collect::<Vec<_>>(), [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight]);
        assert_eq!(params.sink_name.as_deref(), Some(protocol::DEFAULT_SINK), "the stream follows the default output");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_output_s_channel_count_is_asked_with_a_time_limit() {
        use protocol::ChannelPosition as P;
        let path = socket_path("channels");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
            assert!(matches!(cmd, protocol::Command::GetSinkInfo(_)));
            let s71 = [P::FrontLeft, P::FrontRight, P::FrontCenter, P::Lfe, P::RearLeft, P::RearRight, P::SideLeft, P::SideRight];
            protocol::write_reply_message(sock.get_mut(), seq, &sink_info("alsa_output.surround71", &s71), V).unwrap();
            hold(sock);
        });
        assert_eq!(output_channels_from(Some(path.clone()), None, Duration::from_secs(1)), Ok(8));
        server.join().unwrap();
        // A server that accepts and then never answers.
        let listener = UnixListener::bind(&path).unwrap_or_else(|_| {
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path).unwrap()
        });
        let silent = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(1));
            drop(stream);
        });
        let started = Instant::now();
        assert!(output_channels_from(Some(path.clone()), None, Duration::from_millis(300)).is_err());
        assert!(started.elapsed() < Duration::from_millis(800));
        silent.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stream_the_server_refuses_ends_without_being_created() {
        // The caller falls back to another output for a stream that never played.
        let path = socket_path("refused-create");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            let (seq, _) = next_command(&mut sock);
            protocol::write_command_message(sock.get_mut(), seq, &protocol::Command::Error(protocol::PulseError::NoEntity), V).unwrap();
            hold(sock);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.0));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.ended()), "a refused stream ends");
        assert!(!playback.created());
        assert_eq!(playback.sink_channels(), Some(2), "the output was found before the refusal");
        assert!(playback.into_fill().is_some(), "the mixer comes back for another output");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_report_that_includes_blocks_sent_after_the_query_counts_them_once() {
        // Before, the clock was frames sent less the buffered data less the frames sent after the
        // query; the server's report already held those frames, so they were subtracted twice and
        // the picture trailed the sound (by about 25 ms on PulseAudio 15 and 17).
        let path = socket_path("after-query");
        let listener = UnixListener::bind(&path).unwrap();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            create_stream(&mut sock, 32_768);
            let (seq, cmd) = protocol::read_command_message(&mut sock, V).unwrap();
            assert!(matches!(cmd, protocol::Command::GetPlaybackLatency(_)));
            // Ask for one more block before answering: it reaches the server first.
            let more = protocol::Command::Request(protocol::Request { channel: 0, length: 4096 });
            protocol::write_command_message(sock.get_mut(), u32::MAX, &more, V).unwrap();
            let desc = protocol::read_descriptor(&mut sock).unwrap();
            let mut block = vec![0; desc.length as usize];
            sock.read_exact(&mut block).unwrap();
            wait.recv().unwrap();
            // 4608 frames written (both blocks), 3072 read by the sink, no sink latency.
            let latency = protocol::PlaybackLatency {
                sink_usec: 0,
                source_usec: 0,
                playing: true,
                local_time: SystemTime::UNIX_EPOCH,
                remote_time: SystemTime::UNIX_EPOCH,
                write_offset: 36_864,
                read_offset: 24_576,
                underrun_for: 0,
                playing_for: 0,
            };
            protocol::write_reply_message(sock.get_mut(), seq, &latency, V).unwrap();
            hold(sock);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.frames_sent() == 4608), "both blocks sent before the report");
        go.send(()).unwrap();
        assert!(wait_for(|| playback.played_frames() == Some(3072)), "heard = read by the sink: {:?}", playback.played_frames());
        drop(playback);
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn between_reports_the_clock_moves_with_the_wall_clock_for_a_while() {
        // Before, the clock stood still between reports, and the picture trailed the sound by
        // the time since the last one.
        let path = socket_path("interpolate");
        let listener = UnixListener::bind(&path).unwrap();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut sock = handshake(&listener);
            create_stream(&mut sock, 131_072);
            let (seq, _) = protocol::read_command_message(&mut sock, V).unwrap();
            wait.recv().unwrap();
            // 16384 frames written, 1024 read by the sink, no sink latency.
            let latency = protocol::PlaybackLatency {
                sink_usec: 0,
                source_usec: 0,
                playing: true,
                local_time: SystemTime::UNIX_EPOCH,
                remote_time: SystemTime::UNIX_EPOCH,
                write_offset: 131_072,
                read_offset: 8_192,
                underrun_for: 0,
                playing_for: 0,
            };
            protocol::write_reply_message(sock.get_mut(), seq, &latency, V).unwrap();
            hold(sock);
        });
        let fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        let playback = Playback::spawn(Some(path.clone()), 48_000, 2, None, 512, fill).unwrap();
        assert!(wait_for(|| playback.frames_sent() == 16_384));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(playback.clock_frames(), Some(0), "no interpolation before the first report");
        go.send(()).unwrap();
        assert!(wait_for(|| playback.heard_frames() == 1024));
        std::thread::sleep(Duration::from_millis(40));
        let soon = playback.clock_frames().unwrap();
        assert!((1024 + 1_500..=1024 + 4_800).contains(&soon), "40 ms after the report the clock moved on with it: {soon}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(playback.clock_frames(), Some(1024 + 4_800), "at most 100 ms past the report");
        assert_eq!(playback.played_frames(), Some(1024), "the report itself");
        drop(playback);
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
