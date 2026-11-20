//! Android MediaCodec H.264 and HEVC decoding. Java's flexible YUV Image interface describes
//! each plane independently, including the strides of interleaved chroma on some drivers.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, PixelData, VideoFrame, pool};
use filmcraft_isobmff::SampleEntry;
use jni::{
    JNIEnv,
    objects::{GlobalRef, JByteBuffer, JObject, JObjectArray, JValue},
};

use crate::HybridDecoder;
use crate::android_media::{FLEXIBLE_420, codec_names, positive, with_env};
use crate::android_ui::Result as AndroidResult;
use crate::annexb::to_annex_b;
use crate::hybrid::FallibleFlush;

const TRY_AGAIN: i32 = -1;
const FORMAT_CHANGED: i32 = -2;
const BUFFERS_CHANGED: i32 = -3;
const BUFFER_CODEC_CONFIG: i32 = 2;
const BUFFER_EOS: i32 = 4;
const MAX_IN_FLIGHT: usize = 32;
const MAX_SAMPLE_SIZE: usize = 32 << 20;
const MAX_CONFIG_SIZE: usize = 1 << 20;
const DEQUEUE_TIMEOUT: Duration = Duration::from_millis(450);

pub(super) fn factory(entry: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = NalStreamInfo::from_entry(entry)?.ok()?;
    match Decoder::new(info.clone()) {
        Ok(decoder) => Some(Ok(Box::new(HybridDecoder::with_fallible_flush(Box::new(decoder), entry.clone(), info)))),
        Err(reason) => {
            log::debug!("Android hardware decoding declined for {}: {reason}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

pub(super) fn supports(entry: &SampleEntry) -> bool {
    NalStreamInfo::from_entry(entry).and_then(|info| info.ok()).is_some_and(|info| Decoder::new(info).is_ok())
}

struct Decoder {
    info: NalStreamInfo,
    name: String,
    mime: &'static str,
    codec: Option<GlobalRef>,
    // MediaCodec echoes a microsecond timestamp but the sample entry does not know its track's
    // timescale. Use a nonnegative, order-preserving label and retain the exact original ticks.
    origin: Option<i64>,
    pending: BTreeMap<i64, i64>,
    last_pts: Option<i64>,
    need_headers: bool,
    failed: bool,
}

impl Decoder {
    fn new(info: NalStreamInfo) -> AndroidResult<Self> {
        let (width, height) = info.coded;
        let mime = match (info.codec, info.profile_idc) {
            (NalCodec::H264, 66 | 77 | 100) => "video/avc",
            (NalCodec::Hevc, 1) => "video/hevc",
            _ => return Err("codec profile is unsupported".into()),
        };
        let (x, y, w, h) = info.crop;
        if info.chroma_format_idc != 1
            || info.bit_depth_luma != 8
            || info.bit_depth_chroma != 8
            || info.interlaced
            || !(1..=4096).contains(&width)
            || !(1..=4096).contains(&height)
            || u64::from(width) * u64::from(height) > 4096 * 2304
            || w == 0
            || h == 0
            || x > width.saturating_sub(w)
            || y > height.saturating_sub(h)
            || w > width
            || h > height
        {
            return Err("only progressive 8-bit 4:2:0 pictures through 4K are supported".into());
        }
        let names = with_env(|env| codec_names(env, mime, false, FLEXIBLE_420, (w as i32, h as i32)))?;
        for name in names {
            match open(&name, mime, &info) {
                Ok(codec) => {
                    return Ok(Self {
                        info,
                        name,
                        mime,
                        codec: Some(codec),
                        origin: None,
                        pending: BTreeMap::new(),
                        last_pts: None,
                        need_headers: false,
                        failed: false,
                    });
                }
                Err(reason) => log::debug!("Android decoder {name} could not start: {reason}"),
            }
        }
        Err("no compatible hardware MediaCodec decoder".into())
    }

    fn codec(&mut self) -> AndroidResult<GlobalRef> {
        if self.codec.is_none() {
            self.codec = Some(open(&self.name, self.mime, &self.info)?);
            self.need_headers = false;
        }
        self.codec.clone().ok_or_else(|| "Android MediaCodec decoder is unavailable".into())
    }

    fn decode_sample(&mut self, sample: &[u8], pts: i64) -> AndroidResult<Vec<DecodedFrame>> {
        if sample.len() > MAX_SAMPLE_SIZE {
            return Err("Android MediaCodec sample is too large".into());
        }
        let codec = self.codec()?;
        let packet = to_annex_b(&self.info, sample, self.need_headers)?;
        let origin = *self.origin.get_or_insert(pts);
        let stamp = pts
            .checked_sub(origin)
            .and_then(|delta| delta.checked_add(1 << 42))
            .filter(|stamp| (0..(1 << 52)).contains(stamp))
            .ok_or("Android MediaCodec timestamp is out of range")?;
        let has_picture = self.info.nal_types(sample).iter().any(|&kind| match self.info.codec {
            NalCodec::H264 => (1..=5).contains(&kind),
            NalCodec::Hevc => kind < 32,
        });
        let mut frames = Vec::new();
        with_env(|env| {
            let codec = codec.as_obj();
            let buffer_info = new_buffer_info(env)?;
            let deadline = Instant::now() + DEQUEUE_TIMEOUT;
            let index = self.input_index(env, codec, &buffer_info, deadline, &mut frames)?;
            if has_picture && (self.pending.len() >= MAX_IN_FLIGHT || self.pending.insert(stamp, pts).is_some()) {
                return Err("Android MediaCodec discarded a frame or received a duplicate timestamp".into());
            }
            let len = i32::try_from(packet.len())?;
            write_input(env, codec, index, &packet)?;
            queue_input(env, codec, index, len, stamp, 0)?;
            self.need_headers = false;
            if self.drain_available(env, codec, &buffer_info, &mut frames)? {
                return Err("Android MediaCodec stopped before the end of the stream".into());
            }
            Ok(())
        })?;
        Ok(frames)
    }

    fn input_index(
        &mut self,
        env: &mut JNIEnv<'_>,
        codec: &JObject<'_>,
        buffer_info: &JObject<'_>,
        deadline: Instant,
        frames: &mut Vec<DecodedFrame>,
    ) -> AndroidResult<i32> {
        loop {
            let index = env.call_method(codec, "dequeueInputBuffer", "(J)I", &[JValue::Long(3000)])?.i()?;
            if index >= 0 {
                return Ok(index);
            }
            if index != TRY_AGAIN || Instant::now() >= deadline {
                return Err("Android MediaCodec timed out waiting for an input buffer".into());
            }
            if self.drain_available(env, codec, buffer_info, frames)? {
                return Err("Android MediaCodec ended before accepting its input".into());
            }
        }
    }

    fn drain_available(&mut self, env: &mut JNIEnv<'_>, codec: &JObject<'_>, buffer_info: &JObject<'_>, frames: &mut Vec<DecodedFrame>) -> AndroidResult<bool> {
        for _ in 0..=MAX_IN_FLIGHT {
            let index = dequeue_output(env, codec, buffer_info, 0)?;
            match index {
                TRY_AGAIN => return Ok(false),
                FORMAT_CHANGED | BUFFERS_CHANGED => continue,
                0.. => {
                    if self.take_output(env, codec, buffer_info, index, frames)? {
                        return Ok(true);
                    }
                }
                _ => return Err(format!("Android MediaCodec output returned {index}").into()),
            }
        }
        Err("Android MediaCodec returned too many buffers for an input frame".into())
    }

    fn take_output(
        &mut self,
        env: &mut JNIEnv<'_>,
        codec: &JObject<'_>,
        buffer_info: &JObject<'_>,
        index: i32,
        frames: &mut Vec<DecodedFrame>,
    ) -> AndroidResult<bool> {
        let read = env.with_local_frame(24, |env| -> AndroidResult<_> {
            let flags = env.get_field(buffer_info, "flags", "I")?.i()?;
            let size = env.get_field(buffer_info, "size", "I")?.i()?;
            let eos = flags & BUFFER_EOS != 0;
            if size == 0 || flags & BUFFER_CODEC_CONFIG != 0 {
                return Ok(eos);
            }
            let stamp = env.get_field(buffer_info, "presentationTimeUs", "J")?.j()?;
            let pts = self.pending.remove(&stamp).ok_or("Android MediaCodec changed a presentation timestamp")?;
            if self.last_pts.is_some_and(|last| last > pts) {
                return Err("Android MediaCodec returned pictures outside presentation order".into());
            }
            if frames.len() >= MAX_IN_FLIGHT {
                return Err("Android MediaCodec returned too many pictures at once".into());
            }
            let frame = read_output_image(env, codec, index, &self.info)?;
            self.last_pts = Some(pts);
            frames.push(DecodedFrame { pts, frame, draft: false });
            Ok(eos)
        });
        clear_exception(env);
        let released = env.call_method(codec, "releaseOutputBuffer", "(IZ)V", &[JValue::Int(index), JValue::Bool(0)]);
        let eos = read?;
        released?;
        Ok(eos)
    }

    fn drain_end(&mut self) -> AndroidResult<Vec<DecodedFrame>> {
        if self.failed || self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let codec = self.codec.clone().ok_or("Android MediaCodec session ended before its output arrived")?;
        let mut frames = Vec::new();
        with_env(|env| {
            let codec = codec.as_obj();
            let buffer_info = new_buffer_info(env)?;
            let deadline = Instant::now() + DEQUEUE_TIMEOUT;
            let index = self.input_index(env, codec, &buffer_info, deadline, &mut frames)?;
            queue_input(env, codec, index, 0, 0, BUFFER_EOS)?;
            while Instant::now() < deadline {
                let index = dequeue_output(env, codec, &buffer_info, 3000)?;
                match index {
                    TRY_AGAIN | FORMAT_CHANGED | BUFFERS_CHANGED => {}
                    0.. => {
                        if self.take_output(env, codec, &buffer_info, index, &mut frames)? {
                            return if self.pending.is_empty() { Ok(()) } else { Err("Android MediaCodec lost pictures at the end of the stream".into()) };
                        }
                    }
                    _ => return Err(format!("Android MediaCodec output returned {index}").into()),
                }
            }
            Err("Android MediaCodec timed out finishing its last frames".into())
        })?;
        Ok(frames)
    }

    fn release(&mut self) {
        if let Some(codec) = self.codec.take() {
            let _ = with_env(|env| {
                env.call_method(codec.as_obj(), "release", "()V", &[])?;
                Ok(())
            });
        }
    }
}

impl VideoDecoder for Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.decode_sample(sample, pts).map_err(|reason| {
            self.failed = true;
            CodecError::Decode(format!("Android MediaCodec: {reason}"))
        })
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        match self.try_flush() {
            Ok(frames) => frames,
            Err(reason) => {
                log::warn!("Could not finish Android MediaCodec decoding: {reason}");
                Vec::new()
            }
        }
    }

    fn reset(&mut self) {
        if let Some(codec) = self.codec.as_ref() {
            let flushed = with_env(|env| {
                env.call_method(codec.as_obj(), "flush", "()V", &[])?;
                Ok(())
            });
            if flushed.is_err() {
                self.release();
            }
        }
        self.origin = None;
        self.pending.clear();
        self.last_pts = None;
        self.need_headers = true;
        self.failed = false;
    }

    fn name(&self) -> &str {
        "Android MediaCodec"
    }
}

impl FallibleFlush for Decoder {
    fn try_flush(&mut self) -> Result<Vec<DecodedFrame>> {
        self.drain_end().map_err(|reason| {
            self.failed = true;
            CodecError::Decode(format!("Android MediaCodec: {reason}"))
        })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.release();
    }
}

fn clear_exception(env: &mut JNIEnv<'_>) {
    if matches!(env.exception_check(), Ok(true)) {
        let _ = env.exception_clear();
    }
}

fn open(name: &str, mime: &str, info: &NalStreamInfo) -> AndroidResult<GlobalRef> {
    with_env(|env| {
        let codec = create_decoder(env, name)?;
        let result = configure_decoder(env, &codec, mime, info).and_then(|()| Ok(env.new_global_ref(&codec)?));
        if result.is_err() {
            clear_exception(env);
            let _ = env.call_method(&codec, "release", "()V", &[]);
        }
        result
    })
}

fn create_decoder<'a>(env: &mut JNIEnv<'a>, name: &str) -> AndroidResult<JObject<'a>> {
    let name = env.new_string(name)?;
    Ok(env
        .call_static_method("android/media/MediaCodec", "createByCodecName", "(Ljava/lang/String;)Landroid/media/MediaCodec;", &[JValue::Object(&name)])?
        .l()?)
}

fn configure_decoder(env: &mut JNIEnv<'_>, codec: &JObject<'_>, mime: &str, info: &NalStreamInfo) -> AndroidResult<()> {
    let mime = env.new_string(mime)?;
    let (_, _, w, h) = info.crop;
    let format = env
        .call_static_method(
            "android/media/MediaFormat",
            "createVideoFormat",
            "(Ljava/lang/String;II)Landroid/media/MediaFormat;",
            &[JValue::Object(&mime), JValue::Int(w as i32), JValue::Int(h as i32)],
        )?
        .l()?;
    let color = env.new_string("color-format")?;
    env.call_method(&format, "setInteger", "(Ljava/lang/String;I)V", &[JValue::Object(&color), JValue::Int(FLEXIBLE_420)])?;
    set_codec_data(env, &format, info)?;
    let none = JObject::null();
    env.call_method(
        codec,
        "configure",
        "(Landroid/media/MediaFormat;Landroid/view/Surface;Landroid/media/MediaCrypto;I)V",
        &[JValue::Object(&format), JValue::Object(&none), JValue::Object(&none), JValue::Int(0)],
    )?;
    env.call_method(codec, "start", "()V", &[])?;
    Ok(())
}

fn set_codec_data(env: &mut JNIEnv<'_>, format: &JObject<'_>, info: &NalStreamInfo) -> AndroidResult<()> {
    let mut data = [Vec::new(), Vec::new()];
    let mut total = 0usize;
    for nal in &info.parameter_sets {
        let Some(&first) = nal.first() else { continue };
        total = total.saturating_add(nal.len()).saturating_add(4);
        if total > MAX_CONFIG_SIZE {
            return Err("Android MediaCodec configuration exceeds its size limit".into());
        }
        let slot = usize::from(info.codec == NalCodec::H264 && info.nal_type(first) == 8);
        let bytes = data.get_mut(slot).ok_or("Unexpected Android MediaCodec configuration slot")?;
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(nal);
    }
    for (bytes, key) in data.iter().zip(["csd-0", "csd-1"]) {
        if bytes.is_empty() {
            continue;
        }
        let key = env.new_string(key)?;
        let array = env.byte_array_from_slice(bytes)?;
        let buffer = env.call_static_method("java/nio/ByteBuffer", "wrap", "([B)Ljava/nio/ByteBuffer;", &[JValue::Object(&array)])?.l()?;
        env.call_method(format, "setByteBuffer", "(Ljava/lang/String;Ljava/nio/ByteBuffer;)V", &[JValue::Object(&key), JValue::Object(&buffer)])?;
    }
    Ok(())
}

fn new_buffer_info<'a>(env: &mut JNIEnv<'a>) -> AndroidResult<JObject<'a>> {
    Ok(env.new_object("android/media/MediaCodec$BufferInfo", "()V", &[])?)
}

fn dequeue_output(env: &mut JNIEnv<'_>, codec: &JObject<'_>, info: &JObject<'_>, timeout: i64) -> AndroidResult<i32> {
    Ok(env.call_method(codec, "dequeueOutputBuffer", "(Landroid/media/MediaCodec$BufferInfo;J)I", &[JValue::Object(info), JValue::Long(timeout)])?.i()?)
}

fn write_input(env: &mut JNIEnv<'_>, codec: &JObject<'_>, index: i32, packet: &[u8]) -> AndroidResult<()> {
    let buffer = JByteBuffer::from(env.call_method(codec, "getInputBuffer", "(I)Ljava/nio/ByteBuffer;", &[JValue::Int(index)])?.l()?);
    let limit = positive(env.call_method(&buffer, "limit", "()I", &[])?.i()?)?;
    if packet.len() > limit.min(env.get_direct_buffer_capacity(&buffer)?) {
        return Err("Android MediaCodec input buffer is smaller than the compressed frame".into());
    }
    let ptr = env.get_direct_buffer_address(&buffer)?;
    // SAFETY: MediaCodec owns this direct writable input buffer and has exclusively leased it to
    // the decoder until queueInputBuffer. The verified capacity covers the Rust packet.
    unsafe { std::ptr::copy_nonoverlapping(packet.as_ptr(), ptr, packet.len()) };
    Ok(())
}

fn queue_input(env: &mut JNIEnv<'_>, codec: &JObject<'_>, index: i32, size: i32, stamp: i64, flags: i32) -> AndroidResult<()> {
    env.call_method(codec, "queueInputBuffer", "(IIIJI)V", &[JValue::Int(index), JValue::Int(0), JValue::Int(size), JValue::Long(stamp), JValue::Int(flags)])?;
    Ok(())
}

fn read_output_image(env: &mut JNIEnv<'_>, codec: &JObject<'_>, index: i32, info: &NalStreamInfo) -> AndroidResult<VideoFrame> {
    let image = env.call_method(codec, "getOutputImage", "(I)Landroid/media/Image;", &[JValue::Int(index)])?.l()?;
    if image.is_null() {
        return Err("Android MediaCodec did not return a flexible YUV image".into());
    }
    let result = copy_image(env, &image, info);
    clear_exception(env);
    let closed = env.call_method(&image, "close", "()V", &[]);
    let frame = result?;
    closed?;
    Ok(frame)
}

fn image_region(env: &mut JNIEnv<'_>, image: &JObject<'_>, info: &NalStreamInfo) -> AndroidResult<(u32, u32, u32, u32)> {
    let width = u32::try_from(env.call_method(image, "getWidth", "()I", &[])?.i()?)?;
    let height = u32::try_from(env.call_method(image, "getHeight", "()I", &[])?.i()?)?;
    let rect = env.call_method(image, "getCropRect", "()Landroid/graphics/Rect;", &[])?.l()?;
    let mut coords = [0; 4];
    for (value, field) in coords.iter_mut().zip(["left", "top", "right", "bottom"]) {
        *value = u32::try_from(env.get_field(&rect, field, "I")?.i()?)?;
    }
    let [left, top, right, bottom] = coords;
    if right <= left || bottom <= top || right > width || bottom > height {
        return Err("Android MediaCodec returned invalid image bounds".into());
    }
    let (x, y, w, h) = info.crop;
    if (right - left, bottom - top) == (w, h) {
        return Ok((left, top, w, h));
    }
    if (left, top) == (0, 0) && (right, bottom) == info.coded {
        return Ok((x, y, w, h));
    }
    Err("Android MediaCodec output dimensions changed".into())
}

fn copy_image(env: &mut JNIEnv<'_>, image: &JObject<'_>, info: &NalStreamInfo) -> AndroidResult<VideoFrame> {
    const YUV_420_888: i32 = 35;
    if env.call_method(image, "getFormat", "()I", &[])?.i()? != YUV_420_888 {
        return Err("Android MediaCodec returned an unsupported pixel format".into());
    }
    let (x, y, w, h) = image_region(env, image, info)?;
    if x % 2 != 0 || y % 2 != 0 {
        return Err("Android MediaCodec crop has a fractional chroma offset".into());
    }
    let planes = JObjectArray::from(env.call_method(image, "getPlanes", "()[Landroid/media/Image$Plane;", &[])?.l()?);
    if env.get_array_length(&planes)? != 3 {
        return Err("Android MediaCodec did not return three YUV planes".into());
    }
    let luma = (x as usize, y as usize, w as usize, h as usize);
    let chroma = ((x / 2) as usize, (y / 2) as usize, w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    let data = PixelData::Yuv8 {
        planes: [Arc::new(copy_plane(env, &planes, 0, luma)?), Arc::new(copy_plane(env, &planes, 1, chroma)?), Arc::new(copy_plane(env, &planes, 2, chroma)?)],
        chroma: Chroma::C420,
        alpha: None,
    };
    Ok(VideoFrame { width: w, height: h, data, color: info.color, par: info.par, pts: filmcraft_time::Tick::ZERO })
}

fn copy_plane(env: &mut JNIEnv<'_>, planes: &JObjectArray<'_>, index: i32, region: (usize, usize, usize, usize)) -> AndroidResult<Vec<u8>> {
    let plane = env.get_object_array_element(planes, index)?;
    let row_stride = positive(env.call_method(&plane, "getRowStride", "()I", &[])?.i()?)?;
    let pixel_stride = positive(env.call_method(&plane, "getPixelStride", "()I", &[])?.i()?)?;
    let buffer = JByteBuffer::from(env.call_method(&plane, "getBuffer", "()Ljava/nio/ByteBuffer;", &[])?.l()?);
    let position = usize::try_from(env.call_method(&buffer, "position", "()I", &[])?.i()?)?;
    let limit = positive(env.call_method(&buffer, "limit", "()I", &[])?.i()?)?;
    if position > limit || limit > env.get_direct_buffer_capacity(&buffer)? || limit > isize::MAX as usize {
        return Err("Android MediaCodec plane buffer bounds are invalid".into());
    }
    let ptr = env.get_direct_buffer_address(&buffer)?;
    // SAFETY: Java guarantees the plane's direct buffer stays readable until its owning Image
    // is closed. The image is held by the caller until all three planes are copied.
    let source = unsafe { std::slice::from_raw_parts(ptr.add(position), limit - position) };
    tight_plane(source, (row_stride, pixel_stride), region)
}

fn tight_plane(source: &[u8], (rows, pixels): (usize, usize), (x, y, w, h): (usize, usize, usize, usize)) -> AndroidResult<Vec<u8>> {
    let row_span = w.checked_sub(1).and_then(|w| w.checked_mul(pixels)).and_then(|n| n.checked_add(1)).ok_or("Android MediaCodec plane width overflows")?;
    let x = x.checked_mul(pixels).ok_or("Android MediaCodec plane offset overflows")?;
    if x.checked_add(row_span).is_none_or(|end| end > rows) {
        return Err("Android MediaCodec row is too short for its crop".into());
    }
    let mut output = pool::take_u8(w.checked_mul(h).ok_or("Android MediaCodec plane size overflows")?);
    for (row, target) in output.chunks_exact_mut(w).enumerate() {
        let start = y.checked_add(row).and_then(|row| row.checked_mul(rows)).and_then(|offset| offset.checked_add(x));
        let range = start.and_then(|start| Some(start..start.checked_add(row_span)?)).ok_or("Android MediaCodec row offset overflows")?;
        let data = source.get(range).ok_or("Android MediaCodec plane is smaller than its crop")?;
        if pixels == 1 {
            target.copy_from_slice(data);
        } else {
            for (pixel, value) in target.iter_mut().zip(data.iter().step_by(pixels)) {
                *pixel = *value;
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::tight_plane;

    #[test]
    fn yuv_plane_crop_respects_both_strides_and_checks_the_last_row() {
        let pixels = [10, 90, 11, 91, 12, 92, 255, 255, 20, 93, 21, 94, 22, 95];
        assert_eq!(&tight_plane(&pixels, (8, 2), (1, 0, 2, 2)).unwrap(), &[11, 12, 21, 22]);
        assert!(tight_plane(&pixels[..12], (8, 2), (1, 0, 2, 2)).is_err());
        assert!(tight_plane(&pixels, (4, 2), (1, 0, 2, 2)).is_err());
    }
}
