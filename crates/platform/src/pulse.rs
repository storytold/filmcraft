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

use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pulseaudio::protocol;

/// Socket operations during connect/handshake fail after this long (a hung daemon).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// A server request larger than this is treated as broken (frames, not bytes).
const MAX_REQUEST_FRAMES: usize = 1 << 20;

/// One output device as the sound server reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Sink {
    /// Server-internal name, used to address the sink (`bluez_output.…`, `alsa_output.…`).
    pub name: String,
    /// Human-readable description shown to the user ("WH-1000XM4", "Family 17h HD Audio").
    pub description: String,
}

/// Whether a PulseAudio-protocol server (PipeWire or PulseAudio) looks reachable.
pub fn available() -> bool {
    pulseaudio::socket_path_from_env().is_some()
}

/// Connect, authenticate and name the client. All socket operations time out after `timeout`.
fn connect(timeout: Duration) -> Result<(BufReader<UnixStream>, u16), String> {
    let path = pulseaudio::socket_path_from_env().ok_or("no PipeWire/PulseAudio socket")?;
    let stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
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

/// The sound server's output devices. Bounded by `timeout`; never hangs on a stuck daemon.
pub fn list_sinks(timeout: Duration) -> Result<Vec<Sink>, String> {
    let (mut sock, version) = connect(timeout)?;
    protocol::write_command_message(sock.get_mut(), 2, &protocol::Command::GetSinkInfoList, version).map_err(|e| e.to_string())?;
    let (_, sinks) = protocol::read_reply_message::<protocol::SinkInfoList>(&mut sock, version).map_err(|e| e.to_string())?;
    Ok(sinks
        .into_iter()
        .map(|s| {
            let name = s.name.to_string_lossy().into_owned();
            let description = s.description.map(|d| d.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).unwrap_or_else(|| name.clone());
            Sink { name, description }
        })
        .collect())
}

/// A playback stream. Construction returns immediately; the connection, stream setup and the
/// request/answer loop all run on the worker thread, and failures set [`Playback::failed`].
pub struct Playback {
    played: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    /// The worker's socket, kept for `shutdown` so `stop` can unblock a waiting read.
    socket: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Playback {
    /// Start playing: `fill(buffer, channels)` is called on the worker thread with interleaved
    /// f32 whenever the server asks for more data. `sink` is a server-internal sink name; `None`
    /// follows the default output. `request_frames` sizes the server's requests (the I/O buffer
    /// size preference).
    pub fn start(rate: u32, channels: u8, sink: Option<String>, request_frames: u32, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<Self, String> {
        if rate == 0 || channels == 0 {
            return Err("invalid sample rate or channel count".into());
        }
        let played = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let thread = {
            let (played, failed, socket) = (played.clone(), failed.clone(), socket.clone());
            std::thread::Builder::new()
                .name("filmcraft-pulse".into())
                .spawn(move || {
                    if let Err(e) = run(rate, channels, sink, request_frames, fill, &played, &socket)
                        && !failed.swap(true, Ordering::SeqCst)
                        && !e.is_empty()
                    {
                        log::warn!("pulse playback: {e}");
                    }
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self { played, failed, socket, thread: Some(thread) })
    }

    /// Frames handed to the server since `start`. `None` once the stream has failed, so the
    /// caller's clock falls back to the wall clock.
    pub fn played_frames(&self) -> Option<u64> {
        (!self.failed.load(Ordering::SeqCst)).then(|| self.played.load(Ordering::SeqCst))
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // Mark the stream dead so the worker treats the socket teardown as an ordinary exit,
        // then unblock any read it is sitting in.
        self.failed.store(true, Ordering::SeqCst);
        if let Some(sock) = self.socket.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = sock.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The worker: handshake, stream creation, then answer the server's requests until the socket
/// is shut down by [`Playback::drop`] or an error ends the stream.
fn run(
    rate: u32,
    channels: u8,
    sink: Option<String>,
    request_frames: u32,
    mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>,
    played: &AtomicU64,
    shared_socket: &Mutex<Option<UnixStream>>,
) -> Result<(), String> {
    let (mut sock, version) = connect(HANDSHAKE_TIMEOUT)?;
    let stride = usize::from(channels) * 4;
    let request_bytes = request_frames.saturating_mul(stride as u32).max(256);
    let params = protocol::PlaybackStreamParams {
        sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels, sample_rate: rate },
        channel_map: if channels == 1 { protocol::ChannelMap::mono() } else { protocol::ChannelMap::stereo() },
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

    // The handshake is done: publish the socket so `stop` can shut it down, and drop the
    // timeouts; from here the server paces us and a shutdown unblocks any waiting read.
    let handle = sock.get_ref().try_clone().map_err(|e| e.to_string())?;
    handle.set_read_timeout(None).map_err(|e| e.to_string())?;
    handle.set_write_timeout(None).map_err(|e| e.to_string())?;
    *shared_socket.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);

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
        played.fetch_add(frames as u64, Ordering::SeqCst);
        Ok(())
    };

    // The reply told us how much to send before playback starts.
    send(&mut sock, info.requested_bytes as usize)?;
    let mut warned_underflow = false;
    loop {
        let (_, msg) = match protocol::read_command_message(&mut sock, version) {
            Ok(m) => m,
            // A shutdown from `stop` surfaces as a read error; that is the normal exit.
            Err(_) => return Ok(()),
        };
        match msg {
            protocol::Command::Request(protocol::Request { channel, length }) if channel == info.channel => {
                send(&mut sock, length as usize)?;
            }
            // Underruns are survivable here, unlike on the ALSA bridge: the server keeps
            // requesting and playback continues, so note the first one and carry on.
            protocol::Command::Underflow(_) => {
                if !warned_underflow {
                    warned_underflow = true;
                    log::debug!("pulse playback: underflow");
                }
            }
            protocol::Command::Error(e) => return Err(format!("server error: {e:?}")),
            _ => {}
        }
    }
}
