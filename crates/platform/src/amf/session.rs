//! AMD AMF encoder session management.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use super::ffi::{
    AMF_COLOR_PRIMARIES_BT709, AMF_COLOR_PRIMARIES_BT2020, AMF_COLOR_TRANSFER_CHARACTERISTIC_ARIB_STD_B67, AMF_COLOR_TRANSFER_CHARACTERISTIC_BT709,
    AMF_COLOR_TRANSFER_CHARACTERISTIC_SMPTE2084, AMF_VIDEO_CONVERTER_COLOR_PROFILE_709, AMF_VIDEO_CONVERTER_COLOR_PROFILE_2020,
};
use super::ffi::{
    AMF_DX11_1, AMF_EOF, AMF_INPUT_FULL, AMF_MEMORY_HOST, AMF_OK, AMF_REPEAT, AMF_SURFACE_NV12, AMF_SURFACE_P010, AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_KEY,
    AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_IDR, AMF_VIDEO_ENCODER_HW_AV1, AMF_VIDEO_ENCODER_HW_HEVC, AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_IDR,
    AMF_VIDEO_ENCODER_VCE_AVC, AmfBuffer, AmfComponent, AmfContext, AmfHdrMetadata, AmfSurface, AmfVariant, runtime,
};
use filmcraft_export::ColorSignal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    H264Baseline,
    H264Main,
    H264High,
    HevcMain,
    HevcMain10,
    Av1Main,
    Av1Main10,
}

impl Profile {
    pub fn codec(self) -> Codec {
        match self {
            Profile::H264Baseline | Profile::H264Main | Profile::H264High => Codec::H264,
            Profile::HevcMain | Profile::HevcMain10 => Codec::Hevc,
            Profile::Av1Main | Profile::Av1Main10 => Codec::Av1,
        }
    }

    pub fn is_ten_bit(self) -> bool {
        matches!(self, Profile::HevcMain10 | Profile::Av1Main10)
    }
}

pub struct Config {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub bitrate_kbps: u32,
    pub max_bitrate_kbps: u32,
    pub cbr: bool,
    pub keyint: u32,
    pub profile: Profile,
    /// The colour the stream says it is (BT.709 SDR, or BT.2020 with PQ / HLG for Main 10).
    pub signal: ColorSignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub key: bool,
    pub pts: i64,
    pub dts: i64,
}

pub struct Session {
    context: *mut AmfContext,
    component: *mut AmfComponent,
    codec: Codec,
    ten_bit: bool,
    width: u32,
    height: u32,
}

// SAFETY: the context and component are owned by the session alone (nothing else holds them), and
// every call on them takes `&mut self`, so moving the session to another thread never makes two
// threads use them at once; AMF objects are not tied to the thread that created them.
unsafe impl Send for Session {}

impl Session {
    pub fn open(cfg: &Config) -> Result<Self, String> {
        // the plane copies below rely on a nonzero even size; AMF takes it as `amf_int32`
        if cfg.width == 0 || cfg.height == 0 || !cfg.width.is_multiple_of(2) || !cfg.height.is_multiple_of(2) {
            return Err(format!("AMF needs a nonzero even picture size, not {}x{}", cfg.width, cfg.height));
        }
        let (Ok(w), Ok(h)) = (i32::try_from(cfg.width), i32::try_from(cfg.height)) else {
            return Err(format!("{}x{} is too large for AMF", cfg.width, cfg.height));
        };
        if cfg.fps.0 == 0 || cfg.fps.1 == 0 {
            return Err("AMF needs a nonzero frame rate".into());
        }
        let rt = runtime()?;
        let factory = rt.factory();

        let mut context: *mut AmfContext = std::ptr::null_mut();
        // SAFETY: `factory` is the non-null factory AMFInit returned, alive for the process (the DLL is
        // never unloaded); the out-pointer is a local, checked for null below.
        let res = unsafe { ((*(*factory).vtbl).create_context)(factory, &mut context) };
        if res != AMF_OK || context.is_null() {
            return Err(format!("AMF CreateContext failed ({res})"));
        }

        // SAFETY: `context` was just created and checked non-null. A null device asks AMF to create its
        // own D3D11 device on the default adapter; no pointer of ours is retained.
        let res = unsafe { ((*(*context).vtbl).init_dx11)(context, std::ptr::null_mut(), AMF_DX11_1) };
        if res != AMF_OK {
            // SAFETY: our one reference to the context, released once; it is not used afterwards.
            unsafe { ((*(*context).vtbl).release)(context) };
            return Err(format!("AMF InitDX11 failed ({res})"));
        }

        let codec = cfg.profile.codec();
        let component_id = match codec {
            Codec::H264 => AMF_VIDEO_ENCODER_VCE_AVC,
            Codec::Hevc => AMF_VIDEO_ENCODER_HW_HEVC,
            Codec::Av1 => AMF_VIDEO_ENCODER_HW_AV1,
        };

        let mut component: *mut AmfComponent = std::ptr::null_mut();
        // SAFETY: `factory` and `context` are live (above); the component id is a static NUL-terminated
        // wide string, and the out-pointer is a local, checked for null below.
        let res = unsafe { ((*(*factory).vtbl).create_component)(factory, context, component_id.as_ptr(), &mut component) };
        if res != AMF_OK || component.is_null() {
            // SAFETY: the initialised context, terminated and its one reference released; not used after.
            unsafe {
                ((*(*context).vtbl).terminate)(context);
                ((*(*context).vtbl).release)(context);
            }
            return Err(format!("AMF CreateComponent for {:?} failed ({res})", codec));
        }

        let ten_bit = cfg.profile.is_ten_bit();
        let target_bps = i64::from(cfg.bitrate_kbps.max(100)).saturating_mul(1000);
        let peak_bps = i64::from(cfg.max_bitrate_kbps.max(cfg.bitrate_kbps)).saturating_mul(1000);
        let rate_control = match (codec, cfg.cbr) {
            (Codec::H264, true) => 1,
            (Codec::H264, false) => 2,
            (Codec::Hevc | Codec::Av1, true) => 3,
            (Codec::Hevc | Codec::Av1, false) => 2,
        };

        let mut prop_res = AMF_OK;
        let mut set = |name: *const u16, val: AmfVariant| {
            if prop_res == AMF_OK {
                // SAFETY: `component` is live and not yet initialised; `name` is a static NUL-terminated
                // wide string and the variant is passed by value (AMF copies it).
                let r = unsafe { ((*(*component).vtbl).set_property)(component, name, val) };
                if r != AMF_OK {
                    prop_res = r;
                }
            }
        };

        match codec {
            Codec::H264 => {
                let prof_val = match cfg.profile {
                    Profile::H264Baseline => 66,
                    Profile::H264Main => 77,
                    _ => 100,
                };
                set(windows::core::w!("Usage").as_ptr(), AmfVariant::int64(0));
                set(windows::core::w!("Profile").as_ptr(), AmfVariant::int64(prof_val));
                set(windows::core::w!("FrameSize").as_ptr(), AmfVariant::size(w, h));
                set(windows::core::w!("FrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                set(windows::core::w!("TargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                set(windows::core::w!("PeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                set(windows::core::w!("RateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                // the AVC encoder places IDR frames by IDRPeriod (240 by default); it has no GOP-size
                // property (`VideoEncoderVCE.h`)
                set(windows::core::w!("IDRPeriod").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
            }
            Codec::Hevc => {
                let prof_val = if ten_bit { 2 } else { 1 };
                set(windows::core::w!("HevcUsage").as_ptr(), AmfVariant::int64(0));
                set(windows::core::w!("HevcProfile").as_ptr(), AmfVariant::int64(prof_val));
                set(windows::core::w!("HevcFrameSize").as_ptr(), AmfVariant::size(w, h));
                set(windows::core::w!("HevcFrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                set(windows::core::w!("HevcTargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                set(windows::core::w!("HevcPeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                set(windows::core::w!("HevcRateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                set(windows::core::w!("HevcGOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
                if ten_bit {
                    set(windows::core::w!("HevcColorBitDepth").as_ptr(), AmfVariant::int64(10));
                    set(windows::core::w!("HevcNominalRange").as_ptr(), AmfVariant::int64(0));
                }
            }
            Codec::Av1 => {
                set(windows::core::w!("Av1Usage").as_ptr(), AmfVariant::int64(0));
                set(windows::core::w!("Av1Profile").as_ptr(), AmfVariant::int64(1));
                set(windows::core::w!("Av1FrameSize").as_ptr(), AmfVariant::size(w, h));
                set(windows::core::w!("Av1FrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                set(windows::core::w!("Av1TargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                set(windows::core::w!("Av1PeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                set(windows::core::w!("Av1RateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                set(windows::core::w!("Av1GOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
                if ten_bit {
                    set(windows::core::w!("Av1ColorBitDepth").as_ptr(), AmfVariant::int64(10));
                }
            }
        }

        // The colour description the stream carries (VUI / sequence header). Left unset, AMF writes
        // BT.709 even into a PQ stream, so for HDR a runtime that refuses it fails the session; for
        // SDR the request only restates AMF's default.
        if let Some(props) = color_properties(codec) {
            let hdr = cfg.signal.is_hdr();
            let profile = if hdr { AMF_VIDEO_CONVERTER_COLOR_PROFILE_2020 } else { AMF_VIDEO_CONVERTER_COLOR_PROFILE_709 };
            let primaries = if hdr { AMF_COLOR_PRIMARIES_BT2020 } else { AMF_COLOR_PRIMARIES_BT709 };
            let transfer = match cfg.signal.transfer {
                16 => AMF_COLOR_TRANSFER_CHARACTERISTIC_SMPTE2084,
                18 => AMF_COLOR_TRANSFER_CHARACTERISTIC_ARIB_STD_B67,
                _ => AMF_COLOR_TRANSFER_CHARACTERISTIC_BT709,
            };
            // the input planes are already in the output's colour: the same description on both
            // sides, so AMF converts nothing
            for (name, value) in [
                (props.in_profile, profile),
                (props.in_transfer, transfer),
                (props.in_primaries, primaries),
                (props.out_profile, profile),
                (props.out_transfer, transfer),
                (props.out_primaries, primaries),
            ] {
                if hdr {
                    set(name.as_ptr(), AmfVariant::int64(value));
                } else {
                    // SAFETY: setting a property on the live component before Init; the name is a
                    // static NUL-terminated wide string.
                    let _ = unsafe { ((*(*component).vtbl).set_property)(component, name.as_ptr(), AmfVariant::int64(value)) };
                }
            }
            if let Some((md, (max_cll, max_fall))) = cfg.signal.static_metadata()
                && prop_res == AMF_OK
            {
                let meta = AmfHdrMetadata {
                    // ST 2086 lists the primaries green, blue, red
                    green_primary: [md.primaries[0].0, md.primaries[0].1],
                    blue_primary: [md.primaries[1].0, md.primaries[1].1],
                    red_primary: [md.primaries[2].0, md.primaries[2].1],
                    white_point: [md.white_point.0, md.white_point.1],
                    max_mastering_luminance: md.max_luminance,
                    min_mastering_luminance: md.min_luminance,
                    max_content_light_level: max_cll,
                    max_frame_average_light_level: max_fall,
                };
                if let Err(e) = set_hdr_metadata(context, component, props.hdr_metadata, &meta) {
                    prop_res = e;
                }
            }
        }

        // CBR with the HRD model enforced, as AMD's rate-control developers recommend (#880). A
        // runtime without the property keeps the encoder rather than failing the export.
        if cfg.cbr {
            let hrd = match codec {
                Codec::H264 => windows::core::w!("EnforceHRD"),
                Codec::Hevc => windows::core::w!("HevcEnforceHRD"),
                Codec::Av1 => windows::core::w!("Av1EnforceHRD"),
            };
            // SAFETY: as for `set` above.
            let _ = unsafe { ((*(*component).vtbl).set_property)(component, hrd.as_ptr(), AmfVariant::bool_val(true)) };
        }

        if prop_res != AMF_OK {
            // SAFETY: the live component and context, each terminated and its one reference released
            // once, component first; neither is used afterwards.
            unsafe {
                ((*(*component).vtbl).terminate)(component);
                ((*(*component).vtbl).release)(component);
                ((*(*context).vtbl).terminate)(context);
                ((*(*context).vtbl).release)(context);
            }
            return Err(format!("AMF SetProperty failed ({prop_res})"));
        }

        let surface_format = if ten_bit { AMF_SURFACE_P010 } else { AMF_SURFACE_NV12 };
        // SAFETY: the live, configured component; format and size are plain values (the size was
        // checked to fit `amf_int32` at the top of `open`).
        let res = unsafe { ((*(*component).vtbl).init)(component, surface_format, w, h) };
        if res != AMF_OK {
            // SAFETY: as above, on the Init failure.
            unsafe {
                ((*(*component).vtbl).terminate)(component);
                ((*(*component).vtbl).release)(component);
                ((*(*context).vtbl).terminate)(context);
                ((*(*context).vtbl).release)(context);
            }
            return Err(format!("AMF Component Init failed ({res})"));
        }

        Ok(Session { context, component, codec, ten_bit, width: cfg.width, height: cfg.height })
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    pub fn is_ten_bit(&self) -> bool {
        self.ten_bit
    }

    /// Encode one 8-bit 4:2:0 picture: `y` is `width × height`, `u` and `v` are a quarter of that.
    pub fn encode_8bit(&mut self, y: &[u8], u: &[u8], v: &[u8], pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        let (wu, hu) = (self.width as usize, self.height as usize);
        check_planes(y.len(), u.len(), v.len(), wu, hu)?;
        let surface = self.alloc_surface(AMF_SURFACE_NV12)?;
        // NV12: a luma plane of `width` bytes per row, then interleaved U/V, `width` bytes per row at
        // half the height
        let filled = (|| -> Result<(), String> {
            let (y_ptr, y_pitch) = plane(surface, 0, wu, hu)?;
            let (uv_ptr, uv_pitch) = plane(surface, 1, wu, hu / 2)?;
            let cwu = wu / 2;
            for (r, row) in y.chunks_exact(wu).take(hu).enumerate() {
                // SAFETY: `plane` checked that the plane holds `hu` rows of `y_pitch >= wu` bytes and
                // `r < hu`; the source row is `wu` bytes of a Rust slice, which cannot overlap AMF's memory.
                unsafe { std::ptr::copy_nonoverlapping(row.as_ptr(), y_ptr.add(r * y_pitch), wu) };
            }
            for (r, (ur, vr)) in u.chunks_exact(cwu).zip(v.chunks_exact(cwu)).take(hu / 2).enumerate() {
                for (i, (&u_val, &v_val)) in ur.iter().zip(vr).enumerate() {
                    // SAFETY: `plane` checked `hu / 2` rows of `uv_pitch >= wu` bytes, `r < hu / 2`, and
                    // `2 * i + 1 < 2 * cwu = wu`.
                    unsafe {
                        let dst = uv_ptr.add(r * uv_pitch);
                        *dst.add(i * 2) = u_val;
                        *dst.add(i * 2 + 1) = v_val;
                    }
                }
            }
            Ok(())
        })();
        self.submit_filled(surface, filled, pts, duration)
    }

    /// Encode one 10-bit 4:2:0 picture (values in the low 10 bits), laid out as for
    /// [`Session::encode_8bit`].
    pub fn encode_10bit(&mut self, y: &[u16], u: &[u16], v: &[u16], pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        let (wu, hu) = (self.width as usize, self.height as usize);
        check_planes(y.len(), u.len(), v.len(), wu, hu)?;
        let surface = self.alloc_surface(AMF_SURFACE_P010)?;
        // P010: 16-bit samples with the value in the high 10 bits, `width` samples per row
        let filled = (|| -> Result<(), String> {
            let (y_ptr, y_pitch) = plane(surface, 0, wu * 2, hu)?;
            let (uv_ptr, uv_pitch) = plane(surface, 1, wu * 2, hu / 2)?;
            let cwu = wu / 2;
            for (r, row) in y.chunks_exact(wu).take(hu).enumerate() {
                for (c, &val) in row.iter().enumerate() {
                    // SAFETY: `plane` checked `hu` rows of `y_pitch >= 2 * wu` bytes, `r < hu` and `c < wu`.
                    // The write is unaligned because AMF does not promise an even pitch.
                    unsafe { y_ptr.add(r * y_pitch + c * 2).cast::<u16>().write_unaligned((val & 0x3ff) << 6) };
                }
            }
            for (r, (ur, vr)) in u.chunks_exact(cwu).zip(v.chunks_exact(cwu)).take(hu / 2).enumerate() {
                for (c, (&u_val, &v_val)) in ur.iter().zip(vr).enumerate() {
                    // SAFETY: `plane` checked `hu / 2` rows of `uv_pitch >= 2 * wu` bytes, `r < hu / 2`, and
                    // `4 * c + 3 < 4 * cwu = 2 * wu`.
                    unsafe {
                        let dst = uv_ptr.add(r * uv_pitch + c * 4);
                        dst.cast::<u16>().write_unaligned((u_val & 0x3ff) << 6);
                        dst.add(2).cast::<u16>().write_unaligned((v_val & 0x3ff) << 6);
                    }
                }
            }
            Ok(())
        })();
        self.submit_filled(surface, filled, pts, duration)
    }

    /// A host-memory surface of the session's size, holding one reference, which
    /// [`Session::submit_filled`] releases.
    fn alloc_surface(&mut self, format: u32) -> Result<*mut AmfSurface, String> {
        let (w, h) = (self.width as i32, self.height as i32);
        let mut surface: *mut AmfSurface = std::ptr::null_mut();
        // SAFETY: `self.context` is the live context `open` initialised; the out-pointer is a local.
        let res = unsafe { ((*(*self.context).vtbl).alloc_surface)(self.context, AMF_MEMORY_HOST, format, w, h, &mut surface) };
        if res != AMF_OK || surface.is_null() {
            return Err(format!("AMF AllocSurface failed ({res})"));
        }
        Ok(surface)
    }

    /// Submit a surface whose planes were filled (`filled` is `Ok`), releasing it on every path.
    fn submit_filled(&mut self, surface: *mut AmfSurface, filled: Result<(), String>, pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        let result = filled.and_then(|()| self.submit_surface(surface, pts, duration));
        // SAFETY: `surface` came from `alloc_surface` and our reference is still held; AMF takes its own
        // reference to a surface it accepts, so ours is released here whether or not it was submitted.
        unsafe { ((*(*surface).vtbl).release)(surface) };
        result
    }

    fn submit_surface(&mut self, surface: *mut AmfSurface, pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        // SAFETY: `surface` is live until `submit_filled` releases it after this returns, and
        // `self.component` is the initialised encoder.
        let mut res = unsafe {
            ((*(*surface).vtbl).set_pts)(surface, pts);
            ((*(*surface).vtbl).set_duration)(surface, duration);
            ((*(*self.component).vtbl).submit_input)(self.component, surface.cast::<c_void>())
        };
        let mut packets = Vec::new();
        let start = Instant::now();
        while res == AMF_INPUT_FULL && start.elapsed() < Duration::from_millis(500) {
            packets.append(&mut self.query_packets()?);
            std::thread::sleep(Duration::from_millis(1));
            // SAFETY: as above.
            res = unsafe { ((*(*self.component).vtbl).submit_input)(self.component, surface.cast::<c_void>()) };
        }
        if res != AMF_OK {
            return Err(format!("AMF SubmitInput failed ({res})"));
        }
        packets.extend(self.query_packets()?);
        Ok(packets)
    }

    fn check_output_buffer_keyframe(&self, buf: *mut AmfBuffer, slice: &[u8]) -> bool {
        let (prop_name, target_enum) = match self.codec {
            Codec::H264 => (windows::core::w!("OutputDataType"), AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_IDR),
            Codec::Hevc => (windows::core::w!("HevcOutputDataType"), AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_IDR),
            Codec::Av1 => (windows::core::w!("Av1OutputFrameType"), AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_KEY),
        };
        let mut var = AmfVariant { var_type: 0, _pad: 0, val: [0; 16] };
        // SAFETY: `buf` is the live output buffer `poll_output` holds a reference to; the name is a
        // static NUL-terminated wide string and `var` a local the call writes.
        let hr = unsafe { ((*(*buf).vtbl).get_property)(buf, prop_name.as_ptr(), &mut var) };
        if hr == AMF_OK
            && let Some(val) = var.as_int64()
        {
            val == target_enum
        } else {
            is_keyframe(self.codec, slice)
        }
    }

    fn poll_output(&mut self, wait: bool) -> Result<(Vec<Packet>, bool), String> {
        let mut packets = Vec::new();
        let mut out_data: *mut c_void = std::ptr::null_mut();
        let mut repeats = 0;
        let mut eof = false;

        // SAFETY: `self.component` is the initialised encoder. A buffer from QueryOutput holds one
        // reference, ours, released after its bytes are copied (or on the error path); its pointer and
        // size come from that buffer, and the slice is used only before the release.
        unsafe {
            loop {
                let res = ((*(*self.component).vtbl).query_output)(self.component, &mut out_data);
                if res == AMF_OK && !out_data.is_null() {
                    repeats = 0;
                    let buf = out_data.cast::<AmfBuffer>();
                    out_data = std::ptr::null_mut();
                    let size = ((*(*buf).vtbl).get_size)(buf);
                    let ptr = ((*(*buf).vtbl).get_native)(buf);
                    let pts = ((*(*buf).vtbl).get_pts)(buf);
                    // `from_raw_parts` needs a non-null pointer even for an empty slice
                    let data: &[u8] = if size == 0 {
                        &[]
                    } else if ptr.is_null() {
                        ((*(*buf).vtbl).release)(buf);
                        return Err("AMF QueryOutput gave a buffer with no memory".into());
                    } else {
                        std::slice::from_raw_parts(ptr, size)
                    };
                    let is_key = self.check_output_buffer_keyframe(buf, data);
                    packets.push(Packet { data: data.to_vec(), key: is_key, pts, dts: pts });
                    ((*(*buf).vtbl).release)(buf);
                } else if res == AMF_REPEAT {
                    if !wait {
                        break;
                    }
                    repeats += 1;
                    if repeats > 20 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                } else if res == AMF_EOF {
                    eof = true;
                    break;
                } else {
                    return Err(format!("AMF QueryOutput failed ({res})"));
                }
            }
        }
        Ok((packets, eof))
    }

    fn query_packets(&mut self) -> Result<Vec<Packet>, String> {
        self.poll_output(false).map(|(pkts, _)| pkts)
    }

    pub fn drain(&mut self) -> Result<Vec<Packet>, String> {
        // SAFETY: `self.component` is the initialised encoder, used only through `&mut self`.
        unsafe {
            let res = ((*(*self.component).vtbl).drain)(self.component);
            if res != AMF_OK {
                return Err(format!("AMF Drain failed ({res})"));
            }
        }

        Self::drain_loop(Duration::from_secs(5), || self.poll_output(true))
    }

    pub(crate) fn drain_loop<F>(timeout: Duration, mut poller: F) -> Result<Vec<Packet>, String>
    where
        F: FnMut() -> Result<(Vec<Packet>, bool), String>,
    {
        let mut packets = Vec::new();
        let start = Instant::now();
        let mut eof_reached = false;
        loop {
            let (chunk, eof) = poller()?;
            packets.extend(chunk);
            if eof {
                eof_reached = true;
                break;
            }
            if start.elapsed() > timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if !eof_reached {
            return Err("AMF drain timed out before receiving EOF".to_string());
        }
        Ok(packets)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session holds the only reference to each; they are terminated and released once,
        // component before context, and the session is not used again.
        unsafe {
            if !self.component.is_null() {
                ((*(*self.component).vtbl).terminate)(self.component);
                ((*(*self.component).vtbl).release)(self.component);
            }
            if !self.context.is_null() {
                ((*(*self.context).vtbl).terminate)(self.context);
                ((*(*self.context).vtbl).release)(self.context);
            }
        }
    }
}

/// The names of an encoder's colour-description properties.
struct ColorProperties {
    in_profile: windows::core::PCWSTR,
    in_transfer: windows::core::PCWSTR,
    in_primaries: windows::core::PCWSTR,
    out_profile: windows::core::PCWSTR,
    out_transfer: windows::core::PCWSTR,
    out_primaries: windows::core::PCWSTR,
    hdr_metadata: windows::core::PCWSTR,
}

/// The colour-description properties of the HEVC and AV1 encoders (`VideoEncoderHEVC.h`,
/// `VideoEncoderAV1.h`). H.264 here is SDR only and keeps AMF's BT.709 default.
fn color_properties(codec: Codec) -> Option<ColorProperties> {
    use windows::core::w;
    match codec {
        Codec::H264 => None,
        Codec::Hevc => Some(ColorProperties {
            in_profile: w!("HevcInColorProfile"),
            in_transfer: w!("HevcInColorTransferChar"),
            in_primaries: w!("HevcInColorPrimaries"),
            out_profile: w!("HevcOutColorProfile"),
            out_transfer: w!("HevcOutColorTransferChar"),
            out_primaries: w!("HevcOutColorPrimaries"),
            hdr_metadata: w!("HevcInHDRMetadata"),
        }),
        Codec::Av1 => Some(ColorProperties {
            in_profile: w!("Av1InputColorProfile"),
            in_transfer: w!("Av1InputColorTransferChar"),
            in_primaries: w!("Av1InputColorPrimaries"),
            out_profile: w!("Av1OutputColorProfile"),
            out_transfer: w!("Av1OutputColorTransferChar"),
            out_primaries: w!("Av1OutputColorPrimaries"),
            hdr_metadata: w!("Av1InHDRMetadata"),
        }),
    }
}

/// Hand the encoder its HDR10 static metadata: an `AMFBuffer` holding an `AMFHDRMetadata`, which
/// the encoder writes as SEI (HEVC) or metadata OBUs (AV1). Returns the failing AMF result.
fn set_hdr_metadata(context: *mut AmfContext, component: *mut AmfComponent, name: windows::core::PCWSTR, meta: &AmfHdrMetadata) -> Result<(), i32> {
    let mut buf: *mut AmfBuffer = std::ptr::null_mut();
    let size = std::mem::size_of::<AmfHdrMetadata>();
    // SAFETY: `context` is the live, initialised context; the out-pointer is a local.
    let res = unsafe { ((*(*context).vtbl).alloc_buffer)(context, AMF_MEMORY_HOST, size, &mut buf) };
    if res != AMF_OK || buf.is_null() {
        return Err(if res == AMF_OK { super::ffi::AMF_FAIL } else { res });
    }
    // SAFETY: `buf` is the buffer just allocated, holding our one reference. Its memory is checked
    // to be non-null and at least `size` bytes before the unaligned write of `meta`; the component
    // takes its own reference to the buffer in SetProperty, so ours is released on every path.
    unsafe {
        let mem = ((*(*buf).vtbl).get_native)(buf);
        let res = if mem.is_null() || ((*(*buf).vtbl).get_size)(buf) < size {
            super::ffi::AMF_FAIL
        } else {
            mem.cast::<AmfHdrMetadata>().write_unaligned(*meta);
            ((*(*component).vtbl).set_property)(component, name.as_ptr(), AmfVariant::interface(buf.cast::<c_void>()))
        };
        ((*(*buf).vtbl).release)(buf);
        if res == AMF_OK { Ok(()) } else { Err(res) }
    }
}

/// The input planes hold a `w × h` picture and its two quarter-size chroma planes.
fn check_planes(y: usize, u: usize, v: usize, w: usize, h: usize) -> Result<(), String> {
    let (luma, chroma) = (w * h, (w / 2) * (h / 2));
    if y < luma || u < chroma || v < chroma {
        return Err(format!("AMF: the picture's planes ({y}, {u}, {v} samples) are smaller than {w}x{h} 4:2:0 needs ({luma}, {chroma}, {chroma})"));
    }
    Ok(())
}

/// Plane `index` of a host surface: its memory and row pitch in bytes, checked to hold `rows` rows
/// of `row_bytes` bytes, so the copies into it stay inside it.
fn plane(surface: *mut AmfSurface, index: usize, row_bytes: usize, rows: usize) -> Result<(*mut u8, usize), String> {
    // SAFETY: `surface` is a live surface from `alloc_surface`. The plane it returns belongs to the
    // surface (we hold no reference to release) and is called only after its null check.
    let (ptr, hpitch, vpitch) = unsafe {
        let p = ((*(*surface).vtbl).get_plane_at)(surface, index);
        if p.is_null() {
            return Err(format!("AMF surface has no plane {index}"));
        }
        (((*(*p).vtbl).get_native)(p), ((*(*p).vtbl).get_hpitch)(p), ((*(*p).vtbl).get_vpitch)(p))
    };
    if ptr.is_null() {
        return Err(format!("AMF plane {index} has no host memory"));
    }
    let pitch = usize::try_from(hpitch)
        .ok()
        .filter(|&p| p >= row_bytes)
        .ok_or_else(|| format!("AMF plane {index}: pitch {hpitch} is narrower than a row ({row_bytes} bytes)"))?;
    if usize::try_from(vpitch).ok().is_none_or(|v| v < rows) {
        return Err(format!("AMF plane {index}: {vpitch} rows, {rows} needed"));
    }
    Ok((ptr, pitch))
}

fn is_keyframe(codec: Codec, data: &[u8]) -> bool {
    // AV1 output is a sequence of OBUs, with no Annex B start codes to split on
    if codec == Codec::Av1 {
        return av1_is_keyframe(data);
    }
    for n in crate::nvenc::annex_b_nals(data) {
        match codec {
            Codec::H264 => {
                if let Some(&b) = n.first() {
                    let nal_type = b & 0x1f;
                    if nal_type == 5 {
                        return true;
                    }
                }
            }
            Codec::Hevc => {
                if n.len() >= 2 {
                    let nal_type = (n[0] >> 1) & 0x3f;
                    if matches!(nal_type, 19..=21) {
                        return true;
                    }
                }
            }
            Codec::Av1 => {}
        }
    }
    false
}

/// Whether a temporal unit's OBUs hold a sequence header or the frame header of a shown key frame.
fn av1_is_keyframe(data: &[u8]) -> bool {
    // Parse OBUs: check for sequence header (1) or key frame in frame header (3, 6)
    let mut pos = 0usize;
    while pos < data.len() {
        let b0 = data[pos];
        if (b0 >> 7) & 1 != 0 {
            break;
        }
        let obu_type = (b0 >> 3) & 0x0F;
        let extension_flag = (b0 >> 2) & 1;
        let has_size = (b0 >> 1) & 1;
        pos += 1;
        if extension_flag != 0 {
            if pos >= data.len() {
                break;
            }
            pos += 1;
        }
        let payload_start = if has_size != 0 {
            let mut payload_len = 0usize;
            let mut shift = 0;
            let mut valid = false;
            while pos < data.len() {
                let b = data[pos];
                pos += 1;
                payload_len |= ((b & 0x7F) as usize) << shift;
                shift += 7;
                if (b & 0x80) == 0 {
                    valid = true;
                    break;
                }
            }
            if !valid {
                break;
            }
            let start = pos;
            pos = pos.saturating_add(payload_len);
            start
        } else {
            let start = pos;
            pos = data.len();
            start
        };

        if obu_type == 1 {
            return true;
        }
        if (obu_type == 3 || obu_type == 6)
            && let Some(&p0) = data.get(payload_start)
        {
            let show_existing_frame = (p0 >> 7) & 1;
            let frame_type = (p0 >> 5) & 3;
            if show_existing_frame == 0 && frame_type == 0 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_loop_fails_on_timeout_without_eof() {
        let res = Session::drain_loop(Duration::from_millis(20), || Ok((vec![], false)));
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("timed out before receiving EOF"));
    }

    #[test]
    fn drain_loop_succeeds_on_eof() {
        let mut count = 0;
        let res = Session::drain_loop(Duration::from_millis(100), || {
            count += 1;
            if count == 3 { Ok((vec![Packet { data: vec![1, 2, 3], key: true, pts: 0, dts: 0 }], true)) } else { Ok((vec![], false)) }
        });
        assert!(res.is_ok());
        let packets = res.unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].data, vec![1, 2, 3]);
    }

    #[test]
    fn av1_keyframes_are_found_without_start_codes() {
        // a temporal delimiter, then a sequence header OBU
        assert!(is_keyframe(Codec::Av1, &[0x12, 0x00, 0x0a, 0x01, 0x00]));
        // a temporal delimiter, then a frame OBU for a shown key frame
        assert!(is_keyframe(Codec::Av1, &[0x12, 0x00, 0x32, 0x01, 0x00]));
        // a temporal delimiter, then a frame OBU for an inter frame
        assert!(!is_keyframe(Codec::Av1, &[0x12, 0x00, 0x32, 0x01, 0x20]));
    }
}
