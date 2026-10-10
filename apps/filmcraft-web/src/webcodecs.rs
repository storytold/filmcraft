//! WebCodecs (hardware) video decoding for MP4/MOV, with our own decoders as the fallback.
//!
//! WebCodecs decodes asynchronously, and our [`VideoDecoder`] trait is synchronous, so the
//! WebCodecs path plugs in one level up, as a media source: [`reader_opener`] (registered ahead of
//! the built-in openers with `filmcraft_codecs::register_reader_opener`) opens H.264, HEVC, VP9 and
//! AV1 MP4/MOV files as a [`WcSource`]. Its audio and media info come from our own
//! [`filmcraft_codecs::Mp4Source`]; its video frames from a browser `VideoDecoder`:
//!
//! - A frame request finds the frame in the decoded-frame cache, or (re)starts a decode session at
//!   the sample's sync sample, feeds samples up to a little past it (read through the Blob chunk
//!   cache), marks the request [`filmcraft_media::pending`] and returns; the frame server retries
//!   the job on a later frame, when the decoder's output callback has delivered the picture.
//! - A session that has not delivered the frame it was started for is not restarted by other
//!   requests (thumbnails and the monitor would otherwise keep resetting each other).
//! - Native YUV planes feed the desktop frame/compositor contract. Unsupported browser formats or geometry use Canvas RGBA.
//! - Any decoder error or unsupported configuration switches the source to our decoder.
//!
//! `?nowebcodecs` disables it. Support is probed at start-up with `VideoDecoder.isConfigSupported`.
//!
//! [`VideoDecoder`]: filmcraft_codecs::VideoDecoder

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_isobmff::{CodecConfig, Mp4File, TrackKind};
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, SharedReader, SharedSource};
use filmcraft_time::Tick;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

mod pixels;

#[derive(Default)]
struct Transfers {
    native: u64,
    canvas: u64,
    closed: u64,
    stale: u64,
    bytes: u64,
    native_ms: f64,
    canvas_ms: f64,
    reasons: BTreeMap<&'static str, u64>,
}

/// Samples fed past the wanted one (decoders hold pictures for reordering).
const LOOKAHEAD: usize = 8;
/// Decoded frames kept per source.
const CACHE_FRAMES: usize = 40;
/// Chunks allowed in the decoder's queue before feeding pauses.
const MAX_QUEUE: u32 = 24;
/// A session that delivered nothing for this long may be restarted by another request.
const STALE_MS: f64 = 2000.0;

thread_local! {
    /// Codec families the browser decodes ("avc", "hevc", "vp9", "av1").
    static SUPPORTED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static SESSIONS: RefCell<HashMap<u32, Session>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    static CANVAS: RefCell<Option<(web_sys::OffscreenCanvas, web_sys::OffscreenCanvasRenderingContext2d)>> = const { RefCell::new(None) };
    static DECODED: Cell<u64> = const { Cell::new(0) };
    static FALLBACKS: Cell<u64> = const { Cell::new(0) };
    static SOURCES: Cell<u64> = const { Cell::new(0) };
    static TRANSFERS: RefCell<Transfers> = RefCell::new(Transfers::default());
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

fn get(o: &JsValue, k: &str) -> JsValue {
    js_sys::Reflect::get(o, &k.into()).unwrap_or(JsValue::UNDEFINED)
}

fn call(o: &JsValue, method: &str, args: &[&JsValue]) -> Result<JsValue, JsValue> {
    let f: js_sys::Function = get(o, method).dyn_into()?;
    let a = js_sys::Array::new();
    for x in args {
        a.push(x);
    }
    f.apply(o, &a)
}

fn ctor(name: &str) -> Option<js_sys::Function> {
    get(&js_sys::global(), name).dyn_into().ok()
}

fn obj(pairs: &[(&str, JsValue)]) -> JsValue {
    let o = js_sys::Object::new();
    for (k, v) in pairs {
        let _ = js_sys::Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

/// WebCodecs codec string and decoder `description` for a sample entry.
pub fn codec_string(c: &CodecConfig, fourcc: &str) -> Option<(&'static str, String, Option<Vec<u8>>)> {
    match c {
        CodecConfig::Avc(a) => Some(("avc", format!("avc1.{:02x}{:02x}{:02x}", a.profile, a.compatibility, a.level), Some(a.to_bytes()))),
        CodecConfig::Hevc(h) => {
            let space = ["", "A", "B", "C"][(h.general_profile_space & 3) as usize];
            let compat = h.general_profile_compatibility_flags.reverse_bits();
            let mut s = format!(
                "{}.{space}{}.{compat:x}.{}{}",
                if fourcc == "hev1" { "hev1" } else { "hvc1" },
                h.general_profile_idc,
                if h.general_tier_flag { 'H' } else { 'L' },
                h.general_level_idc
            );
            let bytes = h.general_constraint_indicator_flags.to_be_bytes();
            let cons = &bytes[2..8];
            let last = cons.iter().rposition(|b| *b != 0);
            if let Some(l) = last {
                for b in &cons[..=l] {
                    s.push_str(&format!(".{b:x}"));
                }
            }
            Some(("hevc", s, Some(h.to_bytes())))
        }
        CodecConfig::Vp9(v) => Some(("vp9", format!("vp09.{:02}.{:02}.{:02}", v.profile, v.level, v.bit_depth.max(8)), None)),
        CodecConfig::Av1(a) => {
            let depth = if a.twelve_bit {
                12
            } else if a.high_bitdepth {
                10
            } else {
                8
            };
            Some(("av1", format!("av01.{}.{:02}{}.{depth:02}", a.seq_profile, a.seq_level_idx_0, if a.seq_tier_0 { 'H' } else { 'M' }), Some(a.to_bytes())))
        }
        _ => None,
    }
}

/// Probe which codec families the browser decodes (sets `filmcraft.info().webcodecs`).
pub async fn probe() {
    let Some(vd) = ctor("VideoDecoder") else {
        crate::set_info("webcodecs", json!(false));
        return;
    };
    if web_sys::window().and_then(|w| w.location().search().ok()).is_some_and(|s| s.contains("nowebcodecs")) {
        crate::set_info("webcodecs", json!("disabled"));
        return;
    }
    let mut ok = Vec::new();
    for (family, codec) in [("avc", "avc1.640028"), ("hevc", "hvc1.1.6.L120.90"), ("vp9", "vp09.00.40.08"), ("av1", "av01.0.08M.08")] {
        let cfg = obj(&[("codec", codec.into()), ("codedWidth", 1920.into()), ("codedHeight", 1080.into())]);
        let Ok(p) = call(&vd, "isConfigSupported", &[&cfg]).and_then(|p| p.dyn_into::<js_sys::Promise>()) else { continue };
        if let Ok(r) = JsFuture::from(p).await
            && get(&r, "supported").as_bool() == Some(true)
        {
            ok.push(family);
        }
    }
    crate::set_info("webcodecs", json!(ok));
    SUPPORTED.with(|s| *s.borrow_mut() = ok);
}

/// Register the WebCodecs opener (when the browser decodes anything).
pub fn install() {
    if SUPPORTED.with(|s| !s.borrow().is_empty()) {
        filmcraft_codecs::register_reader_opener(reader_opener);
    }
}

/// Live counters for `filmcraft.info()`.
pub fn stats() -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = SESSIONS.with(|ss| {
        ss.borrow()
            .iter()
            .map(|(id, s)| {
                json!({"id": id, "start": s.start, "next": s.next, "target": s.target, "needsKey": s.needs_key, "flushing": s.flushing.get(),
                    "errored": s.errored.get(), "queue": s.queue(), "copies": s.copying.get(), "idleMs": now_ms() - s.last_output.get()})
            })
            .collect()
    });
    let transfers = TRANSFERS.with(|s| {
        let s = s.borrow();
        json!({"nativeFrames":s.native,"canvasFrames":s.canvas,"closedFrames":s.closed,"staleFrames":s.stale,"nativeBytes":s.bytes,"nativeMs":s.native_ms,"canvasMs":s.canvas_ms,"canvasReasons":s.reasons})
    });
    json!({"sources": SOURCES.with(Cell::get), "framesDecoded": DECODED.with(Cell::get), "fallbacks": FALLBACKS.with(Cell::get), "sessions": sessions,"transfers":transfers})
}

/// A [`SharedReader`] as the demuxer's byte source.
struct ReaderSrc(SharedReader);

impl filmcraft_isobmff::ByteSource for ReaderSrc {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

/// MP4/MOV with a WebCodecs-decodable video track → [`WcSource`].
pub fn reader_opener(name: &str, head: &[u8], reader: &SharedReader) -> Option<Result<SharedSource, MediaError>> {
    if !filmcraft_codecs::mp4::sniff(head) {
        return None;
    }
    let inner: SharedSource = match filmcraft_codecs::Mp4Source::open_reader(name, reader.clone()) {
        Ok(s) => Arc::new(s),
        Err(e) => return Some(Err(e.into())),
    };
    let file = match filmcraft_isobmff::open(ReaderSrc(reader.clone())) {
        Ok(f) => f,
        Err(_) => return Some(Ok(inner)),
    };
    let Some(track) = file.tracks.iter().position(|t| t.kind == TrackKind::Video && !t.samples.is_empty()) else { return Some(Ok(inner)) };
    let Some(entry) = file.tracks[track].entries.first() else { return Some(Ok(inner)) };
    let fourcc = entry.format.to_string();
    let Some((family, codec, description)) = codec_string(&entry.codec, &fourcc) else { return Some(Ok(inner)) };
    if !SUPPORTED.with(|s| s.borrow().contains(&family)) {
        return Some(Ok(inner));
    }
    let info = inner.info().clone();
    // the decoder wants the coded size; `info` reports the display size after the track's rotation
    let rotation = file.tracks[track].display_rotation().unwrap_or(0);
    let (w, h) = info.video.as_ref().map(|v| if rotation % 2 == 1 { (v.height, v.width) } else { (v.width, v.height) }).unwrap_or((0, 0));
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    SOURCES.with(|c| c.set(c.get() + 1));
    log::info!("{name}: video decoded with WebCodecs ({codec})");
    let color = info.video.as_ref().map(|v| v.color).unwrap_or_default();
    let par = info.video.as_ref().map(|v| v.par).unwrap_or((1, 1));
    Some(Ok(Arc::new(WcSource {
        inner,
        info,
        reader: reader.clone(),
        file,
        track,
        id,
        config: Config { codec, description, width: w, height: h, rotation, color, par },
        state: Mutex::new(Cache::default()),
    })))
}

#[derive(Clone)]
struct Config {
    codec: String,
    description: Option<Vec<u8>>,
    width: u32,
    height: u32,
    /// Clockwise quarter turns from the track matrix, applied to decoded frames.
    rotation: u8,
    color: filmcraft_color::ColorInfo,
    par: (u32, u32),
}

#[derive(Default)]
struct Cache {
    frames: BTreeMap<i64, Arc<VideoFrame>>,
    failed: bool,
}

/// The browser decoder of one source (UI thread only; JS objects are not `Send`).
struct Session {
    decoder: JsValue,
    /// Sync sample the session started at, next sample to feed.
    start: usize,
    next: usize,
    /// The decoder must restart at a key frame (new, or after a flush).
    needs_key: bool,
    flushing: Rc<Cell<bool>>,
    errored: Rc<Cell<bool>>,
    /// The frame (pts) this session was started for, and when it last delivered anything.
    target: i64,
    last_output: Rc<Cell<f64>>,
    /// Highest presentation time output since the session (re)started.
    out_max: Rc<Cell<i64>>,
    out: Rc<RefCell<Vec<(i64, VideoFrame)>>>,
    copying: Rc<Cell<u32>>,
    generation: Rc<Cell<u64>>,
    alive: Rc<Cell<bool>>,
    _closures: (Closure<dyn FnMut(JsValue)>, Closure<dyn FnMut(JsValue)>),
}

/// Draw a WebCodecs `VideoFrame` and read it back as RGBA.
fn to_rgba(frame: &JsValue) -> Option<VideoFrame> {
    let w = get(frame, "displayWidth").as_f64()? as u32;
    let h = get(frame, "displayHeight").as_f64()? as u32;
    CANVAS.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            let canvas = web_sys::OffscreenCanvas::new(w.max(1), h.max(1)).ok()?;
            let opts = obj(&[("willReadFrequently", true.into())]);
            let ctx = canvas.get_context_with_context_options("2d", &opts).ok()??.dyn_into::<web_sys::OffscreenCanvasRenderingContext2d>().ok()?;
            *c = Some((canvas, ctx));
        }
        let (canvas, ctx) = c.as_ref()?;
        if canvas.width() != w || canvas.height() != h {
            canvas.set_width(w);
            canvas.set_height(h);
        }
        call(ctx, "drawImage", &[frame, &0.into(), &0.into(), &w.into(), &h.into()]).ok()?;
        let img = ctx.get_image_data(0.0, 0.0, w as f64, h as f64).ok()?;
        Some(VideoFrame::rgba8(w, h, img.data().0))
    })
}

impl Session {
    fn new(cfg: &Config) -> Result<Session, JsValue> {
        let vd = ctor("VideoDecoder").ok_or("no VideoDecoder")?;
        let out: Rc<RefCell<Vec<(i64, VideoFrame)>>> = Rc::default();
        let errored = Rc::new(Cell::new(false));
        let last_output = Rc::new(Cell::new(now_ms()));
        let out_max = Rc::new(Cell::new(i64::MIN));
        let copying = Rc::new(Cell::new(0u32));
        let generation = Rc::new(Cell::new(0u64));
        let alive = Rc::new(Cell::new(true));
        let (o, lo, om) = (out.clone(), last_output.clone(), out_max.clone());
        let (copies, epoch, active, config, errors) = (copying.clone(), generation.clone(), alive.clone(), cfg.clone(), errored.clone());
        let on_output = Closure::<dyn FnMut(JsValue)>::new(move |frame: JsValue| {
            let ts = get(&frame, "timestamp").as_f64().unwrap_or(0.0) as i64;
            let expected = epoch.get();
            let (o, lo, om, copies, epoch, active, config, errors) =
                (o.clone(), lo.clone(), om.clone(), copies.clone(), epoch.clone(), active.clone(), config.clone(), errors.clone());
            copies.set(copies.get().saturating_add(1));
            wasm_bindgen_futures::spawn_local(async move {
                let begin = now_ms();
                let converted = match pixels::read(&frame, &config).await {
                    Ok((f, bytes)) => {
                        TRANSFERS.with(|s| {
                            let mut s = s.borrow_mut();
                            s.native = s.native.saturating_add(1);
                            s.bytes = s.bytes.saturating_add(bytes as u64);
                            s.native_ms += now_ms() - begin;
                        });
                        Some(f)
                    }
                    Err(reason) => {
                        let begin = now_ms();
                        let f = to_rgba(&frame);
                        TRANSFERS.with(|s| {
                            let mut s = s.borrow_mut();
                            s.canvas = s.canvas.saturating_add(1);
                            s.canvas_ms += now_ms() - begin;
                            let count = s.reasons.entry(reason).or_default();
                            *count = count.saturating_add(1);
                        });
                        f
                    }
                };
                let _ = call(&frame, "close", &[]);
                copies.set(copies.get().saturating_sub(1));
                TRANSFERS.with(|s| {
                    let mut s = s.borrow_mut();
                    s.closed = s.closed.saturating_add(1);
                });
                if !active.get() || epoch.get() != expected {
                    TRANSFERS.with(|s| {
                        let mut s = s.borrow_mut();
                        s.stale = s.stale.saturating_add(1);
                    });
                    return;
                }
                if let Some(f) = converted {
                    o.borrow_mut().push((ts, f));
                    om.set(om.get().max(ts));
                    DECODED.with(|c| c.set(c.get().saturating_add(1)));
                } else {
                    errors.set(true);
                }
                lo.set(now_ms());
                crate::repaint();
            });
        });
        let e = errored.clone();
        let on_error = Closure::<dyn FnMut(JsValue)>::new(move |err: JsValue| {
            log::warn!("WebCodecs decoder error: {err:?}");
            e.set(true);
            crate::repaint();
        });
        let init = obj(&[("output", on_output.as_ref().clone()), ("error", on_error.as_ref().clone())]);
        let decoder = js_sys::Reflect::construct(&vd, &js_sys::Array::of1(&init))?;
        let s = Session {
            decoder,
            start: 0,
            next: 0,
            needs_key: true,
            flushing: Rc::new(Cell::new(false)),
            errored,
            target: i64::MIN,
            last_output,
            out_max,
            out,
            copying,
            generation,
            alive,
            _closures: (on_output, on_error),
        };
        s.configure(cfg)?;
        Ok(s)
    }

    fn configure(&self, cfg: &Config) -> Result<(), JsValue> {
        let mut pairs = vec![
            ("codec", JsValue::from_str(&cfg.codec)),
            ("codedWidth", cfg.width.into()),
            ("codedHeight", cfg.height.into()),
            ("optimizeForLatency", true.into()),
        ];
        if let Some(d) = &cfg.description {
            pairs.push(("description", js_sys::Uint8Array::from(&d[..]).into()));
        }
        call(&self.decoder, "configure", &[&obj(&pairs)]).map(|_| ())
    }

    fn queue(&self) -> u32 {
        get(&self.decoder, "decodeQueueSize").as_f64().unwrap_or(0.0) as u32
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.set(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        let _ = call(&self.decoder, "close", &[]);
    }
}

/// An MP4/MOV source whose video is decoded by the browser.
pub struct WcSource {
    inner: SharedSource,
    info: MediaInfo,
    reader: SharedReader,
    file: Mp4File,
    track: usize,
    id: u32,
    config: Config,
    state: Mutex<Cache>,
}

enum Drive {
    /// Work is under way (or bytes are loading): retry later.
    Waiting,
    /// The decoder failed: use our own.
    Failed,
}

impl WcSource {
    /// Make progress towards sample `i` (presentation `want`).
    fn drive(&self, i: usize, want: i64) -> Drive {
        let t = &self.file.tracks[self.track];
        let n = t.samples.len();
        SESSIONS.with(|ss| {
            let mut ss = ss.borrow_mut();
            let s = match ss.entry(self.id) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => match Session::new(&self.config) {
                    Ok(s) => v.insert(s),
                    Err(e) => {
                        log::warn!("WebCodecs: {e:?}");
                        return Drive::Failed;
                    }
                },
            };
            if s.errored.get() {
                return Drive::Failed;
            }
            if s.flushing.get() {
                return Drive::Waiting;
            }
            let key = t.sync_sample_before(i);
            // The session reaches the frame by feeding on: it has not output it yet (a frame it
            // already output but the cache dropped needs a restart).
            let covers = !s.needs_key && s.start <= i && key <= s.next && (i >= s.next || want > s.out_max.get());
            if !covers {
                // Don't interrupt a session that is still working towards its own frame, unless it
                // has gone quiet for STALE_MS (a copyTo that never settles must not wedge it).
                let fresh = now_ms() - s.last_output.get() < STALE_MS;
                let busy = fresh && (s.copying.get() > 0 || (!s.needs_key && s.target != i64::MIN && s.out_max.get() < s.target));
                if busy {
                    return Drive::Waiting;
                }
                if !s.needs_key || s.next > 0 {
                    s.generation.set(s.generation.get().wrapping_add(1));
                    let _ = call(&s.decoder, "reset", &[]);
                    if s.configure(&self.config).is_err() {
                        return Drive::Failed;
                    }
                }
                s.out.borrow_mut().clear();
                s.start = key;
                s.next = key;
                s.needs_key = false;
                s.target = want;
                s.out_max.set(i64::MIN);
                s.last_output.set(now_ms());
            } else if s.target == i64::MIN {
                s.target = want;
            }
            let upto = (i + LOOKAHEAD).min(n - 1);
            while s.next <= upto && s.queue().saturating_add(s.copying.get()) < MAX_QUEUE {
                let smp = &t.samples[s.next];
                let mut data = vec![0u8; smp.size as usize];
                match self.reader.read_at(smp.offset, &mut data) {
                    Ok(()) => {}
                    // the bytes are being fetched (pending is marked): feed on the next try
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Drive::Waiting,
                    Err(_) => return Drive::Failed,
                }
                let Some(chunk_ctor) = ctor("EncodedVideoChunk") else { return Drive::Failed };
                let init = obj(&[
                    ("type", (if smp.is_sync || s.next == s.start { "key" } else { "delta" }).into()),
                    ("timestamp", (smp.pts as f64).into()),
                    ("duration", (smp.duration as f64).into()),
                    ("data", js_sys::Uint8Array::from(&data[..]).into()),
                ]);
                let Ok(chunk) = js_sys::Reflect::construct(&chunk_ctor, &js_sys::Array::of1(&init)) else { return Drive::Failed };
                if call(&s.decoder, "decode", &[&chunk]).is_err() {
                    return Drive::Failed;
                }
                s.next += 1;
            }
            // End of the stream: pictures held for reordering only come out on a flush.
            if s.next >= n && s.queue() == 0 {
                s.flushing.set(true);
                s.needs_key = true;
                if let Ok(p) = call(&s.decoder, "flush", &[]).and_then(|p| p.dyn_into::<js_sys::Promise>()) {
                    let f = s.flushing.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let _ = JsFuture::from(p).await;
                        f.set(false);
                        crate::repaint();
                    });
                } else {
                    s.flushing.set(false);
                }
            }
            Drive::Waiting
        })
    }

    /// Move decoded pictures into the cache.
    fn collect(&self, c: &mut Cache, want: i64) -> Result<(), MediaError> {
        let out = SESSIONS.with(|ss| {
            let ss = ss.borrow();
            let Some(s) = ss.get(&self.id) else { return Vec::new() };
            std::mem::take(&mut *s.out.borrow_mut())
        });
        for (pts, f) in out {
            let f = if self.config.rotation != 0 { f.rotated(self.config.rotation).map_err(MediaError::Decode)? } else { f };
            c.frames.insert(pts, Arc::new(f));
        }
        while c.frames.len() > CACHE_FRAMES {
            let Some(far) = c.frames.keys().copied().max_by_key(|p| (p - want).abs()) else { break };
            c.frames.remove(&far);
        }
        // the session's own frame came out: other requests may restart it now
        SESSIONS.with(|ss| {
            if let Some(s) = ss.borrow_mut().get_mut(&self.id)
                && s.out_max.get() >= s.target
            {
                s.target = i64::MIN;
            }
        });
        Ok(())
    }
}

impl Drop for WcSource {
    fn drop(&mut self) {
        SESSIONS.with(|ss| {
            if let Ok(mut ss) = ss.try_borrow_mut() {
                ss.remove(&self.id);
            }
        });
    }
}

impl MediaSource for WcSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>, MediaError> {
        let mut c = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if c.failed {
            drop(c);
            return self.inner.video_frame(req);
        }
        let t = &self.file.tracks[self.track];
        let n = t.samples.len();
        // nearest, as `Mp4Source` looks frames up (sample times are rounded to the timescale)
        let target = req.time.max(Tick::ZERO).to_rational_round(1, t.timescale.max(1) as i64);
        let i = t.sample_at_presentation_time(target).unwrap_or(n - 1).min(n - 1);
        let want = t.samples[i].pts;
        self.collect(&mut c, want)?;
        if let Some(f) = c.frames.get(&want) {
            return Ok(f.clone());
        }
        match self.drive(i, want) {
            Drive::Waiting => {
                self.collect(&mut c, want)?;
                if let Some(f) = c.frames.get(&want) {
                    return Ok(f.clone());
                }
                filmcraft_media::pending::mark();
                Err(MediaError::Decode("WebCodecs: decoding".into()))
            }
            Drive::Failed => {
                log::warn!("{}: WebCodecs failed, using FilmCraft's decoder", self.info.name);
                FALLBACKS.with(|f| f.set(f.get() + 1));
                c.failed = true;
                drop(c);
                self.inner.video_frame(req)
            }
        }
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.inner.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer, MediaError> {
        self.inner.audio_stream(stream, start, frames, sample_rate)
    }
}
