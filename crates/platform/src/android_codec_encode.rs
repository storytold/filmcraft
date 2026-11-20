//! Android MediaCodec H.264 with flexible YUV input and FilmCraft's existing MP4 muxer.

use std::time::{Duration, Instant};

use filmcraft_export::{
    BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, H264Pass, H264Profile, HardwareEncoding,
    Multiplexer, Stage, VideoEncoder,
};
use filmcraft_isobmff::{AvcConfig, SampleEntry};
use filmcraft_time::FrameRate;
use jni::{
    JNIEnv,
    objects::{GlobalRef, JByteBuffer, JObject, JObjectArray, JValue},
};

use crate::android_media::{FLEXIBLE_420, codec_names, with_env};
use crate::android_ui::Result;

const NAME: &str = "Android MediaCodec";
const MIME: &str = "video/avc";
const CODEC: &str = "android/media/MediaCodec";
const FORMAT: &str = "android/media/MediaFormat";
const KEY_FRAME: i32 = 1;
const CONFIG: i32 = 2;
const EOS: i32 = 4;
const PARTIAL: i32 = 8;
const RETRY: i32 = -1;
const FORMAT_CHANGED: i32 = -2;
const BUFFERS_CHANGED: i32 = -3;
const WAIT: Duration = Duration::from_secs(2);
const POLL_US: i64 = 10_000;
const MAX_PACKET: usize = 32 << 20;

#[derive(Clone, Copy)]
struct Settings {
    size: (u16, u16),
    timescale: u32,
    duration: u32,
    bitrate: i32,
    bitrate_mode: i32,
    /// Android's AVCProfile constant and its corresponding SPS profile_idc.
    profile: (i32, u8),
    key_interval: f32,
    mov: bool,
}

impl Settings {
    fn new(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<Self> {
        if s.signal.is_hdr() || s.pixel_aspect.is_some_and(|(n, d)| n != d) || s.field_order != FieldOrder::Progressive {
            return Err("requires progressive SDR with square pixels".into());
        }
        if !(16..=4096).contains(&w) || !(16..=4096).contains(&h) || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            return Err("requires even dimensions between 16 and 4096 pixels".into());
        }
        if s.h264_intra_only || s.h264_level.is_some() || !matches!(s.h264_pass, H264Pass::Single) {
            return Err("does not support all-intra, a fixed H.264 level, or multiple passes".into());
        }
        let profile = match s.h264_profile {
            H264Profile::Baseline => (1, 66),
            H264Profile::Main => (2, 77),
            H264Profile::High => (8, 100),
            H264Profile::High10 => return Err("does not support High 10".into()),
        };
        let bitrate_mode = match s.bitrate_mode {
            BitrateMode::Cbr => 2,
            BitrateMode::Vbr1Pass => 1,
            _ => return Err("requires one-pass VBR or CBR".into()),
        };
        let (timescale, duration) = (u32::try_from(rate.num)?, u32::try_from(rate.den)?);
        let fps = f64::from(timescale) / f64::from(duration);
        if !(1.0..=120.0).contains(&fps) {
            return Err("unsupported frame rate".into());
        }
        let bitrate = i32::try_from(u64::from(s.bitrate_kbps) * 1000)?;
        if bitrate < 100_000 {
            return Err("hardware encoding requires at least 100 kbps".into());
        }
        Ok(Self {
            size: (w as u16, h as u16),
            timescale,
            duration,
            bitrate,
            bitrate_mode,
            profile,
            key_interval: s.keyframe_distance.map_or(2.0, |n| (f64::from(n) / fps) as f32),
            mov: s.multiplexer != Multiplexer::Mp4,
        })
    }

    fn timestamp(self, index: u64) -> Result<i64> {
        Ok(i64::try_from(i128::from(index) * i128::from(self.duration) * 1_000_000 / i128::from(self.timescale))?)
    }
}

pub(super) fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<filmcraft_export::Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 || s.hardware_encoding != HardwareEncoding::Auto {
        return None;
    }
    match Settings::new(w, h, rate, s).and_then(open_verified) {
        Ok(encoder) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(encoder)))
        }
        Err(why) => {
            log::info!("{NAME} encoding declined: {why}");
            filmcraft_export::note_hw_encode_declined_by(NAME, &why.to_string());
            None
        }
    }
}

fn open_verified(settings: Settings) -> Result<Encoder> {
    let (w, h) = settings.size;
    let names = with_env(|env| codec_names(env, MIME, true, FLEXIBLE_420, (i32::from(w), i32::from(h))))?;
    let mut why: Box<dyn std::error::Error> = "no hardware AVC encoder advertises flexible YUV for this size".into();
    for name in names.into_iter().take(4) {
        match probe(&name, settings) {
            Ok(encoder) => {
                log::info!("{NAME} encoding through {name}");
                return Ok(encoder);
            }
            Err(err) => {
                log::info!("{NAME} encoder {name} did not pass its input/output probe: {err}");
                why = err;
            }
        }
    }
    Err(why)
}

/// Exercise actual buffers and output at the requested size before selecting hardware. A codec
/// can advertise a format and accept configure() without ever producing a usable frame.
fn probe(name: &str, settings: Settings) -> Result<Encoder> {
    let mut probe = Encoder::open(name, settings)?;
    let (w, h) = settings.size;
    let y = vec![16; usize::from(w) * usize::from(h)];
    let uv = vec![128; y.len() / 4];
    let mut packets = probe.submit([&y, &uv, &uv])?;
    packets.extend(probe.finish()?);
    if packets.len() != 1 || !packets[0].key || !probe.params.complete() {
        return Err("hardware did not produce a keyframe and valid AVC parameter sets".into());
    }
    let parameters = probe.params.clone();
    drop(probe);
    let mut encoder = Encoder::open(name, settings)?;
    // Some codecs emit their first output after the first batch has begun. The muxer may
    // already need these headers by then. Refuse newly observed headers that differ from them.
    encoder.params = parameters;
    encoder.probe_header = true;
    Ok(encoder)
}

#[derive(Clone, Default)]
struct Params {
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
}

impl Params {
    fn complete(&self) -> bool {
        !self.sps.is_empty() && !self.pps.is_empty()
    }

    fn add(&mut self, nal: &[u8], expected_profile: u8, frozen: bool) -> Result<()> {
        let sets = match nal[0] & 31 {
            7 if nal.len() >= 4 && nal[1] == expected_profile => &mut self.sps,
            7 => return Err("AVC encoder did not honor the requested profile".into()),
            8 if nal.len() >= 2 => &mut self.pps,
            _ => return Err("invalid AVC parameter set".into()),
        };
        if !sets.iter().any(|set| set == nal) {
            if frozen || sets.len() >= 16 || nal.len() > usize::from(u16::MAX) {
                return Err("unexpected AVC parameter-set change".into());
            }
            sets.push(nal.to_vec());
        }
        Ok(())
    }
}

struct Encoder {
    codec: GlobalRef,
    info: GlobalRef,
    settings: Settings,
    params: Params,
    probe_header: bool,
    sent: u64,
    received: u64,
    eos: bool,
    yuv: [Vec<u8>; 3],
}

impl Encoder {
    fn open(name: &str, settings: Settings) -> Result<Self> {
        with_env(|env| {
            let mime = env.new_string(MIME)?;
            let name = env.new_string(name)?;
            let (w, h) = settings.size;
            let format = env
                .call_static_method(
                    FORMAT,
                    "createVideoFormat",
                    "(Ljava/lang/String;II)Landroid/media/MediaFormat;",
                    &[JValue::Object(&mime), JValue::Int(i32::from(w)), JValue::Int(i32::from(h))],
                )?
                .l()?;
            for (key, value) in [
                ("color-format", FLEXIBLE_420),
                ("bitrate", settings.bitrate),
                ("bitrate-mode", settings.bitrate_mode),
                ("profile", settings.profile.0),
                ("max-bframes", 0),
                ("color-standard", 1),
                ("color-transfer", 3),
                ("color-range", 2),
            ] {
                format_value(env, &format, key, JValue::Int(value))?;
            }
            let fps = settings.timescale as f32 / settings.duration as f32;
            format_value(env, &format, "frame-rate", JValue::Float(fps))?;
            format_value(env, &format, "i-frame-interval", JValue::Float(settings.key_interval))?;
            let info = env.new_object("android/media/MediaCodec$BufferInfo", "()V", &[])?;
            let info = env.new_global_ref(info)?;
            let codec = env.call_static_method(CODEC, "createByCodecName", "(Ljava/lang/String;)Landroid/media/MediaCodec;", &[JValue::Object(&name)])?.l()?;
            let started = (|| -> Result<GlobalRef> {
                env.call_method(
                    &codec,
                    "configure",
                    "(Landroid/media/MediaFormat;Landroid/view/Surface;Landroid/media/MediaCrypto;I)V",
                    &[JValue::Object(&format), JValue::Object(&JObject::null()), JValue::Object(&JObject::null()), JValue::Int(1)],
                )?;
                env.call_method(&codec, "start", "()V", &[])?;
                Ok(env.new_global_ref(&codec)?)
            })();
            if started.is_err() {
                let _ = env.exception_clear();
                let _ = env.call_method(&codec, "release", "()V", &[]);
            }
            Ok(Self {
                codec: started?,
                info,
                settings,
                params: Params::default(),
                probe_header: false,
                sent: 0,
                received: 0,
                eos: false,
                yuv: Default::default(),
            })
        })
    }

    fn input(&mut self, planes: Option<[&[u8]; 3]>) -> Result<Vec<EncodedPacket>> {
        let deadline = Instant::now() + WAIT;
        let mut out = Vec::new();
        let pts = self.settings.timestamp(self.sent)?;
        let (w, h) = self.settings.size;
        while Instant::now() < deadline {
            let accepted = with_env(|env| {
                let codec = self.codec.as_obj();
                let index = env.call_method(codec, "dequeueInputBuffer", "(J)I", &[JValue::Long(POLL_US)])?.i()?;
                if index == RETRY {
                    return Ok(false);
                }
                if index < 0 {
                    return Err(format!("invalid encoder input index {index}").into());
                }
                let (bytes, flag) = if let Some(planes) = planes {
                    write_image(env, codec, index, (w, h), planes)?;
                    (i32::from(w) * i32::from(h) * 3 / 2, 0)
                } else {
                    (0, EOS)
                };
                env.call_method(
                    codec,
                    "queueInputBuffer",
                    "(IIIJI)V",
                    &[JValue::Int(index), JValue::Int(0), JValue::Int(bytes), JValue::Long(pts), JValue::Int(flag)],
                )?;
                Ok(true)
            })?;
            if accepted {
                self.sent += u64::from(planes.is_some());
                return Ok(out);
            }
            self.drain(&mut out, 0, deadline)?;
        }
        Err("timed out waiting for an AVC input buffer".into())
    }

    fn submit(&mut self, planes: [&[u8]; 3]) -> Result<Vec<EncodedPacket>> {
        let mut output = self.input(Some(planes))?;
        // Most calls are nonblocking. If the codec builds up a backlog, give it time to drain.
        let timeout = if self.sent - self.received >= 4 { POLL_US } else { 0 };
        self.drain(&mut output, timeout, Instant::now() + WAIT)?;
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut output = self.input(None)?;
        let deadline = Instant::now() + WAIT;
        while !self.eos && Instant::now() < deadline {
            self.drain(&mut output, POLL_US, deadline)?;
        }
        if !self.eos || self.sent != self.received {
            return Err(format!("AVC encoder did not finish all frames ({}/{})", self.received, self.sent).into());
        }
        Ok(output)
    }

    fn drain(&mut self, packets: &mut Vec<EncodedPacket>, mut timeout: i64, deadline: Instant) -> Result<()> {
        while !self.eos && Instant::now() < deadline {
            let next = with_env(|env| output(env, self.codec.as_obj(), self.info.as_obj(), timeout))?;
            timeout = 0;
            let Output::Buffer(buffer) = next else {
                if matches!(next, Output::Pending) {
                    break;
                }
                continue;
            };
            self.eos = buffer.flags & EOS != 0;
            if buffer.flags & PARTIAL != 0 {
                return Err("encoder returned a partial AVC output buffer".into());
            }
            if buffer.bytes.is_empty() {
                continue;
            }
            if let Some(packet) = self.packet(buffer)? {
                packets.push(packet);
            }
        }
        Ok(())
    }

    fn packet(&mut self, buffer: Buffer) -> Result<Option<EncodedPacket>> {
        let mut data = Vec::new();
        let mut key = buffer.flags & KEY_FRAME != 0;
        for nal in annex_b(&buffer.bytes)? {
            let kind = nal[0] & 31;
            if matches!(kind, 7 | 8) {
                self.params.add(nal, self.settings.profile.1, self.probe_header || self.received != 0)?;
            } else if buffer.flags & CONFIG == 0 {
                key |= kind == 5;
                data.extend_from_slice(&u32::try_from(nal.len())?.to_be_bytes());
                data.extend_from_slice(nal);
            }
        }
        if data.is_empty() {
            return Ok(None);
        }
        if self.received >= self.sent || self.settings.timestamp(self.received)?.abs_diff(buffer.pts) > 1 || !self.params.complete() {
            return Err("AVC output frames are missing, reordered or lack parameter sets".into());
        }
        if self.received == 0 && !key {
            return Err("AVC output did not start with a keyframe".into());
        }
        self.received += 1;
        Ok(Some(EncodedPacket { data, key, duration: self.settings.duration, composition_offset: 0 }))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Err(e) = with_env(|env| {
            env.call_method(self.codec.as_obj(), "release", "()V", &[])?;
            Ok(())
        }) {
            log::warn!("{NAME} encoder cleanup: {e}");
        }
    }
}

fn encode_error(e: Box<dyn std::error::Error>) -> ExportError {
    ExportError::Encode(format!("{NAME}: {e}. Retry with hardware encoding off if this happens again."))
}

impl VideoEncoder for Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = self.settings.size;
        let mut entry = SampleEntry::avc(AvcConfig::new(self.params.sps.clone(), self.params.pps.clone(), 4), w, h);
        ColorSignal::default().apply_to(&mut entry, self.settings.mov);
        entry
    }

    fn timescale(&self) -> u32 {
        self.settings.timescale
    }

    fn encode(&mut self, frame: &EncoderFrame) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        let (w, h) = (usize::from(self.settings.size.0), usize::from(self.settings.size.1));
        if frame.hdr.is_some() || frame.index != self.sent || (frame.width as usize, frame.height as usize) != (w, h) || frame.rgba.len() != w * h * 4 {
            return Err(ExportError::Encode("invalid RGBA frame for the Android AVC encoder".into()));
        }
        let [mut y, mut u, mut v] = std::mem::take(&mut self.yuv);
        filmcraft_export::timed(Stage::Convert, || filmcraft_export::rgba_to_yuv420_8(frame.rgba, w, h, &mut y, &mut u, &mut v));
        let encoded = self.submit([&y, &u, &v]);
        self.yuv = [y, u, v];
        let encoded = encoded.map_err(encode_error)?;
        filmcraft_export::note_hw_encode_frame();
        Ok(encoded)
    }

    fn flush(&mut self) -> filmcraft_export::Result<Vec<EncodedPacket>> {
        self.finish().map_err(encode_error)
    }

    fn hardware(&self) -> Option<&'static str> {
        Some(NAME)
    }
}

fn format_value(env: &mut JNIEnv, format: &JObject, key: &str, value: JValue<'_, '_>) -> Result<()> {
    let key = env.new_string(key)?;
    let (method, kind) = if matches!(value, JValue::Float(_)) { ("setFloat", "(Ljava/lang/String;F)V") } else { ("setInteger", "(Ljava/lang/String;I)V") };
    env.call_method(format, method, kind, &[JValue::Object(&key), value])?;
    Ok(())
}

fn write_image(env: &mut JNIEnv, codec: &JObject, index: i32, size: (u16, u16), planes: [&[u8]; 3]) -> Result<()> {
    let image = env.call_method(codec, "getInputImage", "(I)Landroid/media/Image;", &[JValue::Int(index)])?.l()?;
    if image.is_null() || env.call_method(&image, "getFormat", "()I", &[])?.i()? != 35 {
        return Err("codec did not expose a writable YUV_420_888 input image".into());
    }
    let rect = env.call_method(&image, "getCropRect", "()Landroid/graphics/Rect;", &[])?.l()?;
    let left = env.get_field(&rect, "left", "I")?.i()?;
    let top = env.get_field(&rect, "top", "I")?.i()?;
    let right = env.get_field(&rect, "right", "I")?.i()?;
    let bottom = env.get_field(&rect, "bottom", "I")?.i()?;
    if (left, top, right, bottom) != (0, 0, i32::from(size.0), i32::from(size.1)) {
        return Err("unexpected AVC input image crop".into());
    }
    let image_planes = JObjectArray::from(env.call_method(&image, "getPlanes", "()[Landroid/media/Image$Plane;", &[])?.l()?);
    if env.get_array_length(&image_planes)? != 3 {
        return Err("AVC input image does not have three planes".into());
    }
    for (i, src) in planes.into_iter().enumerate() {
        let p = env.get_object_array_element(&image_planes, i as i32)?;
        let scale = if i == 0 { 1 } else { 2 };
        write_plane(env, &p, src, (usize::from(size.0) / scale, usize::from(size.1) / scale))?;
    }
    Ok(())
}

fn write_plane(env: &mut JNIEnv, plane: &JObject, src: &[u8], size: (usize, usize)) -> Result<()> {
    let row = usize::try_from(env.call_method(plane, "getRowStride", "()I", &[])?.i()?)?;
    let pixel = usize::try_from(env.call_method(plane, "getPixelStride", "()I", &[])?.i()?)?;
    let buffer = JByteBuffer::from(env.call_method(plane, "getBuffer", "()Ljava/nio/ByteBuffer;", &[])?.l()?);
    let pos = usize::try_from(env.call_method(&buffer, "position", "()I", &[])?.i()?)?;
    let limit = usize::try_from(env.call_method(&buffer, "limit", "()I", &[])?.i()?)?.min(env.get_direct_buffer_capacity(&buffer)?);
    let (w, h) = size;
    let row_bytes = (w - 1).checked_mul(pixel).and_then(|n| n.checked_add(1)).ok_or("AVC input plane row overflows")?;
    let end = (h - 1).checked_mul(row).and_then(|n| n.checked_add(pos)).and_then(|n| n.checked_add(row_bytes)).ok_or("AVC input plane overflows")?;
    if pixel == 0 || row_bytes > row || end > limit || src.len() != w * h {
        return Err("AVC input plane is smaller than the picture".into());
    }
    let ptr = env.get_direct_buffer_address(&buffer)?;
    for (i, src_row) in src.chunks_exact(w).enumerate() {
        // SAFETY: start and end were checked against the direct buffer's live allocation above.
        // The image is client-owned until it is queued. Chroma planes may alias each other; no
        // Rust reference covers the shared allocation and one plane is written at a time.
        unsafe {
            let dest = ptr.add(pos + i * row);
            if pixel == 1 {
                std::ptr::copy_nonoverlapping(src_row.as_ptr(), dest, w);
            } else {
                for (x, &value) in src_row.iter().enumerate() {
                    dest.add(x * pixel).write(value);
                }
            }
        }
    }
    Ok(())
}

struct Buffer {
    bytes: Vec<u8>,
    flags: i32,
    pts: i64,
}

enum Output {
    Pending,
    Changed,
    Buffer(Buffer),
}

fn output(env: &mut JNIEnv, codec: &JObject, info: &JObject, timeout: i64) -> Result<Output> {
    let index =
        env.call_method(codec, "dequeueOutputBuffer", "(Landroid/media/MediaCodec$BufferInfo;J)I", &[JValue::Object(info), JValue::Long(timeout)])?.i()?;
    match index {
        RETRY => return Ok(Output::Pending),
        FORMAT_CHANGED | BUFFERS_CHANGED => return Ok(Output::Changed),
        i if i < 0 => return Err(format!("invalid encoder output index {i}").into()),
        _ => {}
    }
    let read = (|| -> Result<Buffer> {
        let flags = env.get_field(info, "flags", "I")?.i()?;
        let pts = env.get_field(info, "presentationTimeUs", "J")?.j()?;
        let size = usize::try_from(env.get_field(info, "size", "I")?.i()?)?;
        if size == 0 {
            return Ok(Buffer { bytes: Vec::new(), flags, pts });
        }
        let pos = usize::try_from(env.get_field(info, "offset", "I")?.i()?)?;
        let bytes = JByteBuffer::from(env.call_method(codec, "getOutputBuffer", "(I)Ljava/nio/ByteBuffer;", &[JValue::Int(index)])?.l()?);
        if size > MAX_PACKET || pos.checked_add(size).ok_or("AVC output range overflows")? > env.get_direct_buffer_capacity(&bytes)? {
            return Err("AVC output packet exceeds buffer capacity".into());
        }
        let ptr = env.get_direct_buffer_address(&bytes)?;
        // SAFETY: this direct buffer is owned by the client until releaseOutputBuffer below.
        // We checked the offset and size against its capacity while the buffer is live.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.add(pos), size) }.to_vec();
        Ok(Buffer { bytes, flags, pts })
    })();
    if read.is_err() {
        let _ = env.exception_clear();
    }
    let released = env.call_method(codec, "releaseOutputBuffer", "(IZ)V", &[JValue::Int(index), JValue::Bool(0)]);
    let read = read?;
    released?;
    Ok(Output::Buffer(read))
}

/// MediaCodec AVC uses start-code-prefixed NAL units. MP4 carries a length per NAL instead.
fn annex_b(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut start = None;
    let mut zeros = 0usize;
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == 0 {
            zeros += 1;
            continue;
        }
        if byte == 1 && zeros >= 2 {
            if let Some(begin) = start {
                let nal = &bytes[begin..i - zeros];
                if nal.is_empty() || out.len() >= 1024 {
                    return Err("invalid AVC start codes".into());
                }
                out.push(nal);
            } else if i != zeros {
                return Err("unexpected data before the first AVC start code".into());
            }
            start = Some(i + 1);
        }
        zeros = 0;
    }
    let begin = start.ok_or("AVC output has no Annex B start code")?;
    let last = &bytes[begin..bytes.len() - zeros];
    if last.is_empty() {
        return Err("empty AVC NAL unit".into());
    }
    out.push(last);
    Ok(out)
}
