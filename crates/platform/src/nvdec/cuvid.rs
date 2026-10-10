//! Synchronous NVDEC parser callbacks and CUDA readback. All driver handles stay here.
use std::cell::Cell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use filmcraft_codecs::hw::NalCodec;
use filmcraft_frame::VideoFrame;

use super::ffi::*;
use crate::biplanar::{Biplanar, Geometry, to_frame};
use crate::nvenc::device::Device;

const MAX_SURFACES: u32 = 64;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
const MAX_READY_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Api {
    caps: GetDecoderCaps,
    create_decoder: CreateDecoder,
    destroy_decoder: DestroyDecoder,
    decode: DecodePicture,
    map: MapVideoFrame,
    unmap: UnmapVideoFrame,
    create_parser: CreateVideoParser,
    parse: ParseVideoData,
    destroy_parser: DestroyVideoParser,
}
impl Api {
    fn load() -> Result<(Self, libloading::Library), String> {
        // SAFETY: symbols have the signatures in NVIDIA's public headers. The caller
        // retains the returned library until all handles using these pointers are destroyed.
        unsafe {
            let library = libloading::Library::new("libnvcuvid.so.1").map_err(|e| format!("no NVIDIA NVDEC library: {e}"))?;
            let api = Self {
                caps: *library.get(b"cuvidGetDecoderCaps\0").map_err(|e| e.to_string())?,
                create_decoder: *library.get(b"cuvidCreateDecoder\0").map_err(|e| e.to_string())?,
                destroy_decoder: *library.get(b"cuvidDestroyDecoder\0").map_err(|e| e.to_string())?,
                decode: *library.get(b"cuvidDecodePicture\0").map_err(|e| e.to_string())?,
                map: *library.get(b"cuvidMapVideoFrame64\0").map_err(|e| e.to_string())?,
                unmap: *library.get(b"cuvidUnmapVideoFrame64\0").map_err(|e| e.to_string())?,
                create_parser: *library.get(b"cuvidCreateVideoParser\0").map_err(|e| e.to_string())?,
                parse: *library.get(b"cuvidParseVideoData\0").map_err(|e| e.to_string())?,
                destroy_parser: *library.get(b"cuvidDestroyVideoParser\0").map_err(|e| e.to_string())?,
            };
            Ok((api, library))
        }
    }
}
fn status(code: i32, operation: &str) -> Result<(), String> {
    if code == 0 { Ok(()) } else { Err(format!("{operation} failed ({code})")) }
}

// Only POD structs from ffi.rs are zeroed: no Rust references or invalid enum discriminants.
fn caps(codec: i32, bits: u32) -> CUVIDDECODECAPS {
    // SAFETY: this C struct contains only integers; zero is a valid value for every field.
    let mut caps: CUVIDDECODECAPS = unsafe { std::mem::zeroed() };
    caps.eCodecType = codec;
    caps.eChromaFormat = cudaVideoChromaFormat_420;
    caps.nBitDepthMinus8 = bits.saturating_sub(8);
    caps
}
fn output_format(bits: u32) -> i32 {
    if bits == 10 { cudaVideoSurfaceFormat_P016 } else { cudaVideoSurfaceFormat_NV12 }
}
fn validate(coded: (u32, u32), geometry: Geometry, reorder: usize) -> Result<u32, String> {
    let (w, h) = coded;
    let (x, y, cw, ch) = geometry.crop;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(format!("NVDEC coded size {w}x{h} is not supported"));
    }
    if !matches!(geometry.bits, 8 | 10) || cw == 0 || ch == 0 || x.checked_add(cw).is_none_or(|end| end > w) || y.checked_add(ch).is_none_or(|end| end > h) {
        return Err("NVDEC picture depth or crop is invalid".into());
    }
    let delay = u32::try_from(reorder).map_err(|_| "NVDEC reorder count overflows")?;
    if delay >= MAX_SURFACES {
        return Err("NVDEC reorder count is too large".into());
    }
    Ok(delay)
}
fn check_caps(caps: &CUVIDDECODECAPS, coded: (u32, u32), bits: u32) -> Result<(), String> {
    let (w, h) = coded;
    let blocks = w.div_ceil(16).checked_mul(h.div_ceil(16)).ok_or("NVDEC macroblock count overflows")?;
    let mask = 1u16.checked_shl(output_format(bits) as u32).ok_or("NVDEC output format overflows")?;
    if caps.bIsSupported == 0
        || caps.nOutputFormatMask & mask == 0
        || w < u32::from(caps.nMinWidth)
        || h < u32::from(caps.nMinHeight)
        || w > caps.nMaxWidth
        || h > caps.nMaxHeight
        || blocks > caps.nMaxMBCount
    {
        return Err(format!("GPU does not support NVDEC codec {} at {w}x{h}, {bits}-bit 4:2:0", caps.eCodecType));
    }
    Ok(())
}

/// Slices per picture the driver's offset list is read for.
const MAX_SLICES: u32 = 8192;

struct State {
    api: Api,
    // Points to Session's separately boxed, immutable Device (stable across moves).
    device: *const Device,
    decoder: *mut c_void,
    codec: i32,
    coded: (u32, u32),
    geometry: Geometry,
    delay: u32,
    surfaces: u32,
    mapped: Cell<Option<u64>>,
    pending: Vec<CUVIDPARSERDISPINFO>,
    ready: Vec<(i64, VideoFrame)>,
    error: Option<String>,
}
impl State {
    fn sequence(&mut self, format: &CUVIDEOFORMAT) -> Result<i32, String> {
        // The driver rounds the coded size up to its own block size (HEVC 640x360 comes back as
        // 640x368): take its size, which is the mapped surface's, when it holds the stream's.
        let driver = (format.coded_width, format.coded_height);
        let fits = |d: u32, c: u32| d >= c && d - c < 64;
        if !(fits(driver.0, self.coded.0) && fits(driver.1, self.coded.1)) || (!self.decoder.is_null() && driver != self.coded) {
            return Err(format!("NVDEC sequence is {}x{}, the stream {}x{}", driver.0, driver.1, self.coded.0, self.coded.1));
        }
        self.coded = driver;
        if format.codec != self.codec
            || format.chroma_format != cudaVideoChromaFormat_420
            || format.progressive_sequence != 1
            || u32::from(format.bit_depth_luma_minus8).checked_add(8) != Some(self.geometry.bits)
            || format.bit_depth_chroma_minus8 != format.bit_depth_luma_minus8
        {
            return Err("NVDEC sequence differs from the stream configuration".into());
        }
        let surfaces = u32::from(format.min_num_decode_surfaces).max(self.delay.checked_add(1).ok_or("NVDEC surface count overflows")?).max(2);
        if format.min_num_decode_surfaces == 0 || surfaces > MAX_SURFACES {
            return Err("NVDEC reported an invalid decode surface count".into());
        }
        if !self.decoder.is_null() {
            if surfaces > self.surfaces {
                return Err("NVDEC sequence needs more decode surfaces".into());
            }
            return i32::try_from(self.surfaces).map_err(|_| "NVDEC surface count overflows".into());
        }
        // SAFETY: plain C data (integers and pointers) permits an all-zero initialization.
        let mut info: CUVIDDECODECREATEINFO = unsafe { std::mem::zeroed() };
        info.ulWidth = u64::from(self.coded.0);
        info.ulHeight = u64::from(self.coded.1);
        info.ulNumDecodeSurfaces = u64::from(surfaces);
        info.CodecType = self.codec;
        info.ChromaFormat = cudaVideoChromaFormat_420;
        info.ulCreationFlags = cudaVideoCreate_PreferCUVID;
        info.bitDepthMinus8 = u64::from(self.geometry.bits.saturating_sub(8));
        info.ulMaxWidth = info.ulWidth;
        info.ulMaxHeight = info.ulHeight;
        let right = i16::try_from(self.coded.0).map_err(|_| "NVDEC width overflows")?;
        let bottom = i16::try_from(self.coded.1).map_err(|_| "NVDEC height overflows")?;
        // Explicit full coded rectangles prevent the parser's display crop being applied twice.
        info.display_area = ShortRect { left: 0, top: 0, right, bottom };
        info.target_rect = info.display_area;
        info.OutputFormat = output_format(self.geometry.bits);
        info.DeinterlaceMode = cudaVideoDeinterlaceMode_Weave;
        info.ulTargetWidth = info.ulWidth;
        info.ulTargetHeight = info.ulHeight;
        info.ulNumOutputSurfaces = 1;
        // SAFETY: the context is current throughout the parser call; info and the out-pointer
        // are live. The decoder is owned by Session until explicitly destroyed.
        status(unsafe { (self.api.create_decoder)(&mut self.decoder, &mut info) }, "cuvidCreateDecoder")?;
        if self.decoder.is_null() {
            return Err("NVDEC returned a null decoder".into());
        }
        self.surfaces = surfaces;
        i32::try_from(surfaces).map_err(|_| "NVDEC surface count overflows".into())
    }
    fn display(&mut self, info: &CUVIDPARSERDISPINFO) -> Result<i32, String> {
        if self.decoder.is_null()
            || info.picture_index < 0
            || u32::try_from(info.picture_index).is_ok_and(|i| i >= self.surfaces)
            || info.progressive_frame != 1
            || info.repeat_first_field != 0
        {
            return Err("NVDEC reported an invalid or non-progressive display picture".into());
        }
        if self.pending.len() >= MAX_SURFACES as usize {
            return Err("NVDEC display queue exceeds its surface limit".into());
        }
        self.pending.try_reserve(1).map_err(|e| format!("NVDEC display allocation failed: {e}"))?;
        self.pending.push(*info);
        Ok(1)
    }
    fn drain(&mut self) -> Result<(), String> {
        // Copy queued displays before another decode can recycle their picture indices.
        for info in std::mem::take(&mut self.pending) {
            self.map_picture(&info)?;
        }
        Ok(())
    }
    fn map_picture(&mut self, info: &CUVIDPARSERDISPINFO) -> Result<(), String> {
        // SAFETY: this C struct contains integers and nullable pointers only.
        let mut proc: CUVIDPROCPARAMS = unsafe { std::mem::zeroed() };
        proc.progressive_frame = 1;
        proc.top_field_first = info.top_field_first;
        let (mut address, mut pitch) = (0u64, 0u32);
        // SAFETY: the validated picture index belongs to the live decoder. Both output
        // pointers and proc are writable, and Session has pushed the context on this thread.
        status(unsafe { (self.api.map)(self.decoder, info.picture_index, &mut address, &mut pitch, &mut proc) }, "cuvidMapVideoFrame64")?;
        self.mapped.set(Some(address));
        let mapped = Mapped { api: self.api, decoder: self.decoder, address, outstanding: &self.mapped, active: true };
        let frame = self.readback(address, pitch);
        mapped.unmap()?;
        let frame = frame?;
        self.ready.try_reserve(1).map_err(|e| format!("NVDEC output allocation failed: {e}"))?;
        self.ready.push((info.timestamp, frame));
        Ok(())
    }
    fn readback(&self, address: u64, pitch: u32) -> Result<VideoFrame, String> {
        if address == 0 {
            return Err("NVDEC returned a null mapped address".into());
        }
        let (luma_size, total) = mapped_layout(self.coded, self.geometry.bits, pitch)?;
        let picture_bytes = (self.coded.0 as usize)
            .checked_mul(self.coded.1 as usize)
            .and_then(|n| n.checked_mul(if self.geometry.bits == 10 { 3 } else { 2 }))
            .ok_or("NVDEC output size overflows")?;
        if self.ready.len() >= MAX_SURFACES as usize
            || self.ready.len().checked_add(1).and_then(|n| n.checked_mul(picture_bytes)).is_none_or(|n| n > MAX_READY_BYTES)
        {
            return Err("NVDEC output queue exceeds its memory limit".into());
        }
        address.checked_add(u64::try_from(total).map_err(|_| "NVDEC mapped size overflows")?).ok_or("NVDEC mapped address overflows")?;
        let mut host = Vec::new();
        host.try_reserve_exact(total).map_err(|e| format!("NVDEC readback allocation failed: {e}"))?;
        host.resize(total, 0);
        // SAFETY: Session's separately boxed device outlives State and every synchronous callback;
        // it is immutable, and the CUDA context is current. No mutable Device references exist.
        let device = unsafe { self.device.as_ref() }.ok_or("NVDEC CUDA device is missing")?;
        device.copy_to_host(address, &mut host)?;
        let luma = host.get(..luma_size).ok_or("NVDEC luma range is invalid")?;
        let chroma = host.get(luma_size..).ok_or("NVDEC chroma range is invalid")?;
        to_frame(&Biplanar { luma, chroma, stride: pitch as usize, rows: self.coded.1 as usize }, &self.geometry)
    }
}
fn mapped_layout(coded: (u32, u32), bits: u32, pitch: u32) -> Result<(usize, usize), String> {
    let row_bytes = coded.0.checked_mul(if bits == 10 { 2 } else { 1 }).ok_or("NVDEC row size overflows")?;
    if pitch < row_bytes || pitch == 0 || coded.1 == 0 || !coded.1.is_multiple_of(2) || !matches!(bits, 8 | 10) {
        return Err("NVDEC reported an invalid mapped pitch or size".into());
    }
    let luma = (pitch as usize).checked_mul(coded.1 as usize).ok_or("NVDEC luma size overflows")?;
    let total = luma.checked_add(luma / 2).ok_or("NVDEC mapped size overflows")?;
    if total > MAX_FRAME_BYTES {
        return Err("NVDEC mapped frame exceeds its memory limit".into());
    }
    Ok((luma, total))
}
struct Mapped<'a> {
    api: Api,
    decoder: *mut c_void,
    address: u64,
    outstanding: &'a Cell<Option<u64>>,
    active: bool,
}
impl Mapped<'_> {
    fn unmap(mut self) -> Result<(), String> {
        self.active = false;
        // SAFETY: this guard owns one successful mapping on this decoder, and the context is
        // still current. No GPU-memory references escape the synchronous host copy.
        status(unsafe { (self.api.unmap)(self.decoder, self.address) }, "cuvidUnmapVideoFrame64")?;
        self.outstanding.set(None);
        Ok(())
    }
}
impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: balances a successful map on every early return or caught panic;
            // the enclosing parser operation retains the current context and driver library.
            let st = unsafe { (self.api.unmap)(self.decoder, self.address) };
            match status(st, "cuvidUnmapVideoFrame64") {
                Ok(()) => self.outstanding.set(None),
                Err(e) => log::warn!("{e}"),
            }
        }
    }
}

/// Callback state is exclusively accessed by synchronous callbacks while parse is running.
unsafe fn callback(data: *mut c_void, f: impl FnOnce(&mut State) -> Result<i32, String>) -> i32 {
    // SAFETY: cuvid receives the stable, leaked State pointer at parser creation; callbacks run
    // synchronously, without a live Rust reference to State in the enclosing parse call.
    let Some(state) = (unsafe { data.cast::<State>().as_mut() }) else { return 0 };
    if state.error.is_some() {
        return 0;
    }
    match catch_unwind(AssertUnwindSafe(|| f(state))) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            state.error = Some(error);
            0
        }
        Err(_) => {
            state.error = Some("NVDEC callback panicked".into());
            0
        }
    }
}
unsafe extern "C" fn sequence(data: *mut c_void, format: *mut CUVIDEOFORMAT) -> i32 {
    // SAFETY: cuvid invokes this callback with its synchronous format pointer and our boxed state.
    unsafe { callback(data, |state| state.sequence(format.as_ref().ok_or("NVDEC sequence pointer is null")?)) }
}
unsafe extern "C" fn decode(data: *mut c_void, picture: *mut CUVIDPICPARAMS) -> i32 {
    // SAFETY: the picture parameters belong to cuvid for the callback's duration: their shared
    // head is read (the slice offset list for at most `MAX_SLICES` entries, as the parser sized
    // it), and they are passed through to the live decoder with the context current.
    unsafe {
        callback(data, |state| {
            if state.decoder.is_null() || picture.is_null() {
                return Err("NVDEC decode pointer or decoder is null".into());
            }
            // A picture without bitstream data, or with slices past the end of it, is an error
            // here and never reaches the driver.
            let p = &*picture;
            let offsets = if p.pSliceDataOffsets.is_null() || p.nNumSlices > MAX_SLICES {
                &[][..]
            } else {
                std::slice::from_raw_parts(p.pSliceDataOffsets, p.nNumSlices as usize)
            };
            if p.pBitstreamData.is_null() || p.nBitstreamDataLen == 0 || offsets.is_empty() || offsets.iter().any(|&o| o > p.nBitstreamDataLen) {
                return Err("NVDEC parser returned a picture without valid bitstream data".into());
            }
            state.drain()?;
            status((state.api.decode)(state.decoder, picture), "cuvidDecodePicture")?;
            Ok(1)
        })
    }
}
unsafe extern "C" fn display(data: *mut c_void, picture: *mut CUVIDPARSERDISPINFO) -> i32 {
    // SAFETY: cuvid supplies display metadata valid during this synchronous callback;
    // null is only an EOS notification, which we did not request.
    unsafe { callback(data, |state| state.display(picture.as_ref().ok_or("NVDEC display pointer is null")?)) }
}

/// A movable session. Each operation pushes its CUDA context on the calling thread.
pub struct Session {
    // From `Box::into_raw` and freed in `Drop`: the parser's callbacks get this same pointer as
    // their user data, so Rust code reaches the state only through it (never through a `Box`
    // whose later use would invalidate the pointer the driver holds).
    state: ptr::NonNull<State>,
    parser: *mut c_void,
    device: Box<Device>,
    // Dropped after device/context release, and after all cuvid handles have been destroyed.
    _library: libloading::Library,
}
// SAFETY: no callbacks or GPU-memory references outlive an operation. The state allocation and
// boxed device retain stable addresses across moves; every driver call pushes the context on its
// calling thread.
unsafe impl Send for Session {}
impl Session {
    pub fn new(codec: NalCodec, coded: (u32, u32), geometry: Geometry, reorder: usize) -> Result<Self, String> {
        let delay = validate(coded, geometry, reorder)?;
        if matches!(codec, NalCodec::H264) && geometry.bits != 8 {
            return Err("NVDEC only takes 8-bit H.264".into());
        }
        let codec = match codec {
            NalCodec::H264 => cudaVideoCodec_H264,
            NalCodec::Hevc => cudaVideoCodec_HEVC,
        };
        let (api, library) = Api::load()?;
        let device = Box::new(Device::new()?);
        {
            let _current = device.push_current()?;
            let mut capabilities = caps(codec, geometry.bits);
            // SAFETY: capabilities is a live writable C struct, and the CUDA context is current.
            status(unsafe { (api.caps)(&mut capabilities) }, "cuvidGetDecoderCaps")?;
            check_caps(&capabilities, coded, geometry.bits)?;
        }
        let state = ptr::NonNull::new(Box::into_raw(Box::new(State {
            api,
            device: ptr::from_ref(device.as_ref()),
            decoder: ptr::null_mut(),
            codec,
            coded,
            geometry,
            delay,
            surfaces: 0,
            mapped: Cell::new(None),
            pending: Vec::new(),
            ready: Vec::new(),
            error: None,
        })))
        .ok_or("NVDEC state allocation is null")?;
        // From here on `Drop` frees the state, also when creating the parser fails.
        let mut session = Self { state, parser: ptr::null_mut(), device, _library: library };
        session.create_parser()?;
        Ok(session)
    }
    /// The callback state. Never hold the reference across a driver call that can run the
    /// parser's callbacks (`cuvidParseVideoData`): they reach the state through the same pointer.
    fn state(&mut self) -> &mut State {
        // SAFETY: the pointer comes from `Box::into_raw` and only `Drop` frees it, so it is valid and
        // aligned. `&mut self` makes this the only Rust access; callbacks run only inside
        // `parse`'s driver call, during which no reference from here is live.
        unsafe { self.state.as_mut() }
    }
    fn create_parser(&mut self) -> Result<(), String> {
        let (api, codec, delay) = {
            let state = self.state();
            (state.api, state.codec, state.delay)
        };
        let _current = self.device.push_current()?;
        // SAFETY: plain C data, nullable callback pointers and opaque pointers allow zeros.
        let mut params: CUVIDPARSERPARAMS = unsafe { std::mem::zeroed() };
        params.CodecType = codec;
        params.ulMaxNumDecodeSurfaces = 1; // sequence callback supplies the real DPB size
        params.ulClockRate = 0;
        params.ulMaxDisplayDelay = delay;
        params.pUserData = self.state.as_ptr().cast();
        params.pfnSequenceCallback = Some(sequence);
        params.pfnDecodePicture = Some(decode);
        params.pfnDisplayPicture = Some(display);
        // SAFETY: params and output pointer are valid; the state allocation remains alive until
        // after parser destruction (Drop), and the library is retained by Session.
        status(unsafe { (api.create_parser)(&mut self.parser, &mut params) }, "cuvidCreateVideoParser")?;
        if self.parser.is_null() {
            return Err("NVDEC returned a null parser".into());
        }
        Ok(())
    }
    fn parse(&mut self, payload: &[u8], pts: i64, flags: u64) -> Result<Vec<(i64, VideoFrame)>, String> {
        if let Some(error) = &self.state().error {
            return Err(error.clone());
        }
        if self.parser.is_null() {
            return Err("NVDEC parser is not available".into());
        }
        let api = self.state().api;
        let _current = self.device.push_current()?;
        let mut packet = CUVIDSOURCEDATAPACKET {
            flags,
            payload_size: u64::try_from(payload.len()).map_err(|_| "NVDEC packet length overflows")?,
            payload: if payload.is_empty() { ptr::null() } else { payload.as_ptr() },
            timestamp: pts,
        };
        // SAFETY: parser is live, payload and packet outlive this synchronous call. No reference
        // to State is live across it, so callbacks have exclusive access through their pointer.
        let st = unsafe { (api.parse)(self.parser, &mut packet) };
        // SAFETY: as in `state()`; the parse call has returned, so no callback runs while this
        // reference lives (the readback below calls no parser function).
        let state = unsafe { self.state.as_mut() };
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        status(st, "cuvidParseVideoData")?;
        match catch_unwind(AssertUnwindSafe(|| state.drain())) {
            Ok(result) => result?,
            Err(_) => return Err("NVDEC readback panicked".into()),
        }
        Ok(std::mem::take(&mut state.ready))
    }
    pub fn feed(&mut self, annex_b: &[u8], pts: i64) -> Result<Vec<(i64, VideoFrame)>, String> {
        if annex_b.is_empty() || annex_b.len() > MAX_PACKET_BYTES {
            return Err("NVDEC access unit is empty or too large".into());
        }
        let result = self.parse(annex_b, pts, CUVID_PKT_TIMESTAMP | CUVID_PKT_ENDOFPICTURE);
        if let Err(error) = &result {
            self.state().error.get_or_insert_with(|| error.clone());
        }
        result
    }
    pub fn flush(&mut self) -> Result<Vec<(i64, VideoFrame)>, String> {
        let result = self.parse(&[], 0, CUVID_PKT_ENDOFSTREAM);
        self.restart()?;
        result
    }
    /// Destroy the parser and, unless `keep_decoder`, the decoder (a seek keeps it: creating one
    /// costs tens of milliseconds, and the next parser's sequence callback takes it as it is).
    fn destroy(&mut self, keep_decoder: bool) -> Result<(), String> {
        let _current = self.device.push_current()?;
        let parser = self.parser;
        // SAFETY: as in `state()`; while this reference lives only the unmap and destroy calls
        // run, and they invoke no parser callbacks.
        let state = unsafe { self.state.as_mut() };
        let mut error = None;
        if let Some(address) = state.mapped.get() {
            // SAFETY: retry an outstanding map before releasing its decoder; context is current.
            match status(unsafe { (state.api.unmap)(state.decoder, address) }, "cuvidUnmapVideoFrame64") {
                Ok(()) => state.mapped.set(None),
                Err(e) => {
                    error = Some(e);
                }
            }
        }
        if !keep_decoder && !state.decoder.is_null() {
            // SAFETY: owned decoder, no host views or callbacks remain; unmapping was attempted
            // first, and context and library remain live while the driver releases its resources.
            match status(unsafe { (state.api.destroy_decoder)(state.decoder) }, "cuvidDestroyDecoder") {
                Ok(()) => {
                    state.decoder = ptr::null_mut();
                    state.surfaces = 0;
                    state.mapped.set(None);
                }
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        if !parser.is_null() {
            // SAFETY: owned parser, no parse call running; user data and library are still live.
            match status(unsafe { (state.api.destroy_parser)(parser) }, "cuvidDestroyVideoParser") {
                Ok(()) => self.parser = ptr::null_mut(),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn restart(&mut self) -> Result<(), String> {
        let state = self.state();
        state.ready.clear();
        state.pending.clear();
        state.error = None;
        let result = self.destroy(true).and_then(|()| self.create_parser());
        if let Err(error) = &result {
            self.state().error = Some(error.clone());
        }
        result
    }
    pub fn reset(&mut self) {
        if let Err(e) = self.restart() {
            log::warn!("NVDEC reset failed: {e}");
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(e) = self.destroy(false) {
            log::warn!("NVDEC cleanup failed: {e}");
        }
        // A parser that failed to be destroyed is leaked rather than left pointing at freed state.
        if self.parser.is_null() {
            // SAFETY: the pointer came from `Box::into_raw` in `new`, is freed only here, and no parser
            // holds it any more (destroyed above), so nothing can reach it afterwards.
            drop(unsafe { Box::from_raw(self.state.as_ptr()) });
        } else {
            log::warn!("NVDEC parser could not be destroyed: leaking its state");
        }
    }
}
/// Load both drivers and retain a primary context; does not create a decoder or parse video.
pub fn probe() -> Result<String, String> {
    let (_api, _library) = Api::load()?;
    let device = Device::new()?;
    let _current = device.push_current()?;
    Ok("NVDEC (libnvcuvid, first CUDA device)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn geometry() -> Geometry {
        Geometry { crop: (0, 0, 640, 360), bits: 8, color: filmcraft_color::ColorInfo::REC709, par: (1, 1) }
    }
    #[test]
    fn hostile_geometry_and_driver_sizes_are_errors() {
        for size in [(0, 0), (u32::MAX, 368), (640, u32::MAX), (641, 368)] {
            assert!(catch_unwind(|| validate(size, geometry(), 2)).unwrap().is_err());
        }
        let mut g = geometry();
        g.crop = (u32::MAX, 0, 640, 360);
        assert!(validate((640, 368), g, 2).is_err());
        assert!(validate((640, 368), geometry(), usize::MAX).is_err());
        assert_eq!(validate((640, 368), geometry(), 2).unwrap(), 2);
        for (size, bits, pitch) in [((640, 368), 8, 639), ((640, 368), 10, 1279), ((640, 368), 8, u32::MAX), ((640, u32::MAX), 10, 1280)] {
            assert!(catch_unwind(|| mapped_layout(size, bits, pitch)).unwrap().is_err());
        }
        assert_eq!(mapped_layout((640, 368), 8, 640).unwrap(), (235520, 353280));
        assert_eq!(mapped_layout((640, 368), 10, 1280).unwrap(), (471040, 706560));
    }
    #[test]
    fn capabilities_decline_unsupported_streams() {
        let mut c = caps(cudaVideoCodec_HEVC, 10);
        assert!(check_caps(&c, (640, 368), 10).is_err());
        c.bIsSupported = 1;
        c.nOutputFormatMask = 2;
        c.nMaxWidth = 8192;
        c.nMaxHeight = 8192;
        c.nMaxMBCount = 65536;
        assert!(check_caps(&c, (640, 368), 10).is_ok());
        assert!(check_caps(&c, (640, 368), 8).is_err());
        c.nMaxMBCount = 1;
        assert!(check_caps(&c, (640, 368), 10).is_err());
        c.nMinWidth = 1000;
        assert!(check_caps(&c, (640, 368), 10).is_err());
    }
    #[test]
    fn session_is_send() {
        fn is_send<T: Send>() {}
        is_send::<Session>();
    }

    // These stubs must never be called: the callback tests stop before driver access.
    unsafe extern "C" fn test_caps(_: *mut CUVIDDECODECAPS) -> i32 {
        999
    }
    unsafe extern "C" fn test_create_decoder(_: *mut *mut c_void, _: *mut CUVIDDECODECREATEINFO) -> i32 {
        999
    }
    unsafe extern "C" fn test_destroy(_: *mut c_void) -> i32 {
        999
    }
    unsafe extern "C" fn test_decode(_: *mut c_void, _: *mut CUVIDPICPARAMS) -> i32 {
        999
    }
    unsafe extern "C" fn test_map(_: *mut c_void, _: i32, _: *mut u64, _: *mut u32, _: *mut CUVIDPROCPARAMS) -> i32 {
        999
    }
    unsafe extern "C" fn test_unmap(_: *mut c_void, _: u64) -> i32 {
        999
    }
    unsafe extern "C" fn test_create_parser(_: *mut *mut c_void, _: *mut CUVIDPARSERPARAMS) -> i32 {
        999
    }
    unsafe extern "C" fn test_parse(_: *mut c_void, _: *mut CUVIDSOURCEDATAPACKET) -> i32 {
        999
    }
    fn test_state() -> State {
        State {
            api: Api {
                caps: test_caps,
                create_decoder: test_create_decoder,
                destroy_decoder: test_destroy,
                decode: test_decode,
                map: test_map,
                unmap: test_unmap,
                create_parser: test_create_parser,
                parse: test_parse,
                destroy_parser: test_destroy,
            },
            device: ptr::null(),
            decoder: ptr::null_mut(),
            codec: cudaVideoCodec_H264,
            coded: (640, 368),
            geometry: geometry(),
            delay: 2,
            surfaces: 0,
            mapped: Cell::new(None),
            pending: Vec::new(),
            ready: Vec::new(),
            error: None,
        }
    }
    #[test]
    fn callback_panics_and_hostile_metadata_do_not_escape() {
        let mut state = test_state();
        let data = ptr::from_mut(&mut state).cast();
        // SAFETY: data points to exclusive live test state; the closure touches no driver.
        assert_eq!(unsafe { callback(data, |_| panic!("synthetic callback panic")) }, 0);
        assert_eq!(state.error.as_deref(), Some("NVDEC callback panicked"));
        // SAFETY: same live state. A recorded failure prevents the second closure running.
        assert_eq!(unsafe { callback(data, |_| Err("later error".into())) }, 0);
        assert_eq!(state.error.as_deref(), Some("NVDEC callback panicked"));
        state.error = None;
        let data = ptr::from_mut(&mut state).cast();
        // SAFETY: valid test user data; null metadata is deliberately tested and never dereferenced.
        assert_eq!(unsafe { sequence(data, ptr::null_mut()) }, 0);
        assert_eq!(state.error.as_deref(), Some("NVDEC sequence pointer is null"));
        // SAFETY: C may report null user data; our callback rejects it before dereferencing.
        assert_eq!(unsafe { display(ptr::null_mut(), ptr::null_mut()) }, 0);
        // SAFETY: POD C metadata can be initialized to zeros for these hostile-data tests.
        let mut format: CUVIDEOFORMAT = unsafe { std::mem::zeroed() };
        format.codec = state.codec;
        format.coded_width = 640;
        format.coded_height = 368;
        format.chroma_format = cudaVideoChromaFormat_420;
        format.progressive_sequence = 1;
        for count in [0, 65, u8::MAX] {
            format.min_num_decode_surfaces = count;
            assert!(state.sequence(&format).is_err());
        }
        // SAFETY: POD display metadata; the null decoder and hostile index fail before mapping.
        let mut picture: CUVIDPARSERDISPINFO = unsafe { std::mem::zeroed() };
        state.decoder = ptr::dangling_mut::<c_void>();
        state.surfaces = 4;
        picture.progressive_frame = 1;
        for index in [-1, 4, i32::MAX] {
            picture.picture_index = index;
            assert!(state.display(&picture).is_err());
        }
        picture.picture_index = 0;
        picture.timestamp = 17;
        assert_eq!(state.display(&picture).unwrap(), 1);
        picture.picture_index = 1;
        picture.timestamp = -9;
        assert_eq!(state.display(&picture).unwrap(), 1);
        assert_eq!(state.pending.iter().map(|p| (p.picture_index, p.timestamp)).collect::<Vec<_>>(), vec![(0, 17), (1, -9)]);
    }
    unsafe extern "C" fn test_unmap_ok(_: *mut c_void, _: u64) -> i32 {
        0
    }
    #[test]
    fn mapping_guard_tracks_cleanup_on_success_failure_and_unwind() {
        let outstanding = Cell::new(Some(123));
        let mut api = test_state().api;
        api.unmap = test_unmap_ok;
        drop(Mapped { api, decoder: ptr::null_mut(), address: 123, outstanding: &outstanding, active: true });
        assert_eq!(outstanding.get(), None);
        outstanding.set(Some(123));
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _mapped = Mapped { api, decoder: ptr::null_mut(), address: 123, outstanding: &outstanding, active: true };
                panic!("synthetic readback panic");
            }))
            .is_err()
        );
        assert_eq!(outstanding.get(), None);
        outstanding.set(Some(123));
        api.unmap = test_unmap;
        assert!(Mapped { api, decoder: ptr::null_mut(), address: 123, outstanding: &outstanding, active: true }.unmap().is_err());
        assert_eq!(outstanding.get(), Some(123));
    }
}
