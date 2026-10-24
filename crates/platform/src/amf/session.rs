//! AMD AMF encoder session management.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use super::ffi::*;

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
}

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

// SAFETY: Session owns raw AMF COM pointers that are manipulated sequentially on worker threads.
unsafe impl Send for Session {}

impl Session {
    pub fn open(cfg: &Config) -> Result<Self, String> {
        let rt = runtime()?;
        let factory = rt.factory();

        let mut context: *mut AmfContext = std::ptr::null_mut();
        // SAFETY: calling AMFFactory::CreateContext with live out-pointer.
        let res = unsafe { ((*(*factory).vtbl).create_context)(factory, &mut context) };
        if res != AMF_OK || context.is_null() {
            return Err(format!("AMF CreateContext failed ({res})"));
        }

        #[cfg(target_os = "windows")]
        {
            // SAFETY: calling AMFContext::InitDX11 with NULL pointer; AMF internally selects an AMD adapter,
            // verifies GPU support, and initializes the Direct3D 11 device.
            let res = unsafe { ((*(*context).vtbl).init_dx11)(context, std::ptr::null_mut(), AMF_DX11_1) };
            if res != AMF_OK {
                // SAFETY: releasing context on initialization failure.
                unsafe { ((*(*context).vtbl).release)(context) };
                return Err(format!("AMF InitDX11 failed ({res})"));
            }
        }

        let codec = cfg.profile.codec();
        let component_id = match codec {
            Codec::H264 => AMF_VIDEO_ENCODER_VCE_AVC,
            Codec::Hevc => AMF_VIDEO_ENCODER_HW_HEVC,
            Codec::Av1 => AMF_VIDEO_ENCODER_HW_AV1,
        };

        let mut component: *mut AmfComponent = std::ptr::null_mut();
        // SAFETY: calling CreateComponent with live context and component ID.
        let res = unsafe { ((*(*factory).vtbl).create_component)(factory, context, component_id.as_ptr(), &mut component) };
        if res != AMF_OK || component.is_null() {
            // SAFETY: terminating and releasing context on component creation failure.
            unsafe {
                ((*(*context).vtbl).terminate)(context);
                ((*(*context).vtbl).release)(context);
            }
            return Err(format!("AMF CreateComponent for {:?} failed ({res})", codec));
        }

        let ten_bit = cfg.profile.is_ten_bit();
        let (w, h) = (cfg.width as i32, cfg.height as i32);
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
                // SAFETY: setting property on live AMFComponent before Init.
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
                set(windows::core::w!("GOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
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

        if prop_res != AMF_OK {
            // SAFETY: terminating and releasing component and context on property configuration failure.
            unsafe {
                ((*(*component).vtbl).terminate)(component);
                ((*(*component).vtbl).release)(component);
                ((*(*context).vtbl).terminate)(context);
                ((*(*context).vtbl).release)(context);
            }
            return Err(format!("AMF SetProperty failed ({prop_res})"));
        }

        let surface_format = if ten_bit { AMF_SURFACE_P010 } else { AMF_SURFACE_NV12 };
        // SAFETY: calling AMFComponent::Init with configured format and dimensions.
        let res = unsafe { ((*(*component).vtbl).init)(component, surface_format, w, h) };
        if res != AMF_OK {
            // SAFETY: terminating and releasing component and context on failure.
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

    pub fn encode_8bit(&mut self, y: &[u8], u: &[u8], v: &[u8], pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        let (w, h) = (self.width as i32, self.height as i32);
        let mut surface: *mut AmfSurface = std::ptr::null_mut();
        // SAFETY: allocating host NV12 surface.
        let res = unsafe { ((*(*self.context).vtbl).alloc_surface)(self.context, AMF_MEMORY_HOST, AMF_SURFACE_NV12, w, h, &mut surface) };
        if res != AMF_OK || surface.is_null() {
            return Err(format!("AMF AllocSurface failed ({res})"));
        }

        // SAFETY: filling NV12 planes.
        unsafe {
            let y_plane = ((*(*surface).vtbl).get_plane_at)(surface, 0);
            let y_ptr = ((*(*y_plane).vtbl).get_native)(y_plane);
            let y_pitch = ((*(*y_plane).vtbl).get_hpitch)(y_plane) as usize;
            let wu = self.width as usize;
            let hu = self.height as usize;

            for (row, dst_row) in y.chunks_exact(wu).zip((0..hu).map(|r| y_ptr.add(r * y_pitch))) {
                std::ptr::copy_nonoverlapping(row.as_ptr(), dst_row, wu);
            }

            let uv_plane = ((*(*surface).vtbl).get_plane_at)(surface, 1);
            let uv_ptr = ((*(*uv_plane).vtbl).get_native)(uv_plane);
            let uv_pitch = ((*(*uv_plane).vtbl).get_hpitch)(uv_plane) as usize;
            let cwu = wu / 2;

            for (r, (ur, vr)) in (0..hu / 2).zip(u.chunks_exact(cwu).zip(v.chunks_exact(cwu))) {
                let dst = uv_ptr.add(r * uv_pitch);
                for (i, (&u_val, &v_val)) in (0..cwu).zip(ur.iter().zip(vr.iter())) {
                    *dst.add(i * 2) = u_val;
                    *dst.add(i * 2 + 1) = v_val;
                }
            }
        }

        self.submit_surface(surface, pts, duration)
    }

    pub fn encode_10bit(&mut self, y: &[u16], u: &[u16], v: &[u16], pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        let (w, h) = (self.width as i32, self.height as i32);
        let mut surface: *mut AmfSurface = std::ptr::null_mut();
        // SAFETY: allocating host P010 surface.
        let res = unsafe { ((*(*self.context).vtbl).alloc_surface)(self.context, AMF_MEMORY_HOST, AMF_SURFACE_P010, w, h, &mut surface) };
        if res != AMF_OK || surface.is_null() {
            return Err(format!("AMF AllocSurface P010 failed ({res})"));
        }

        // SAFETY: filling P010 planes.
        unsafe {
            let y_plane = ((*(*surface).vtbl).get_plane_at)(surface, 0);
            let y_ptr = ((*(*y_plane).vtbl).get_native)(y_plane) as *mut u16;
            let y_pitch = ((*(*y_plane).vtbl).get_hpitch)(y_plane) as usize / 2;
            let wu = self.width as usize;
            let hu = self.height as usize;

            for (r, row) in (0..hu).zip(y.chunks_exact(wu)) {
                let dst = y_ptr.add(r * y_pitch);
                for (c, &val) in (0..wu).zip(row.iter()) {
                    *dst.add(c) = (val & 0x3ff) << 6;
                }
            }

            let uv_plane = ((*(*surface).vtbl).get_plane_at)(surface, 1);
            let uv_ptr = ((*(*uv_plane).vtbl).get_native)(uv_plane) as *mut u16;
            let uv_pitch = ((*(*uv_plane).vtbl).get_hpitch)(uv_plane) as usize / 2;
            let cwu = wu / 2;

            for (r, (ur, vr)) in (0..hu / 2).zip(u.chunks_exact(cwu).zip(v.chunks_exact(cwu))) {
                let dst = uv_ptr.add(r * uv_pitch);
                for (c, (&u_val, &v_val)) in (0..cwu).zip(ur.iter().zip(vr.iter())) {
                    *dst.add(c * 2) = (u_val & 0x3ff) << 6;
                    *dst.add(c * 2 + 1) = (v_val & 0x3ff) << 6;
                }
            }
        }

        self.submit_surface(surface, pts, duration)
    }

    fn submit_surface(&mut self, surface: *mut AmfSurface, pts: i64, duration: i64) -> Result<Vec<Packet>, String> {
        // SAFETY: surface is a valid live AmfSurface.
        unsafe {
            ((*(*surface).vtbl).set_pts)(surface, pts);
            ((*(*surface).vtbl).set_duration)(surface, duration);

            let mut res = ((*(*self.component).vtbl).submit_input)(self.component, surface as *mut c_void);
            let mut backlog = Vec::new();
            if res == AMF_INPUT_FULL {
                let start = Instant::now();
                while res == AMF_INPUT_FULL && start.elapsed() < Duration::from_millis(500) {
                    let mut more = self.query_packets()?;
                    backlog.append(&mut more);
                    std::thread::sleep(Duration::from_millis(1));
                    res = ((*(*self.component).vtbl).submit_input)(self.component, surface as *mut c_void);
                }
            }
            ((*(*surface).vtbl).release)(surface);
            if res != AMF_OK {
                return Err(format!("AMF SubmitInput failed ({res})"));
            }
            let mut packets = backlog;
            packets.extend(self.query_packets()?);
            Ok(packets)
        }
    }

    fn check_output_buffer_keyframe(&self, buf: *mut AmfBuffer, slice: &[u8]) -> bool {
        let (prop_name, target_enum) = match self.codec {
            Codec::H264 => (windows::core::w!("OutputDataType"), AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_IDR),
            Codec::Hevc => (windows::core::w!("HevcOutputDataType"), AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_IDR),
            Codec::Av1 => (windows::core::w!("Av1OutputFrameType"), AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_KEY),
        };
        let mut var = AmfVariant { var_type: 0, _pad: 0, val: [0; 16] };
        // SAFETY: calling get_property on valid AmfBuffer pointer.
        let hr = unsafe { ((*(*buf).vtbl).get_property)(buf, prop_name.as_ptr(), &mut var) };
        if hr == AMF_OK && let Some(val) = var.as_int64() {
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

        // SAFETY: polling QueryOutput for ready buffers.
        unsafe {
            loop {
                let res = ((*(*self.component).vtbl).query_output)(self.component, &mut out_data);
                if res == AMF_OK && !out_data.is_null() {
                    repeats = 0;
                    let buf = out_data as *mut AmfBuffer;
                    let size = ((*(*buf).vtbl).get_size)(buf);
                    let ptr = ((*(*buf).vtbl).get_native)(buf);
                    let pts = ((*(*buf).vtbl).get_pts)(buf);
                    let slice = std::slice::from_raw_parts(ptr, size);
                    let is_key = self.check_output_buffer_keyframe(buf, slice);
                    packets.push(Packet { data: slice.to_vec(), key: is_key, pts, dts: pts });
                    ((*(*buf).vtbl).release)(buf);
                    out_data = std::ptr::null_mut();
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
        // SAFETY: draining the encoder component and polling until AMF_EOF.
        unsafe {
            let res = ((*(*self.component).vtbl).drain)(self.component);
            if res != AMF_OK {
                return Err(format!("AMF Drain failed ({res})"));
            }
        }

        let mut packets = Vec::new();
        let start = Instant::now();
        loop {
            let (chunk, eof) = self.poll_output(true)?;
            packets.extend(chunk);
            if eof || start.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(packets)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: clean up AMF component and context.
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

fn is_keyframe(codec: Codec, data: &[u8]) -> bool {
    let nals = crate::nvenc::annex_b_nals(data);
    for n in nals {
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
            Codec::Av1 => {
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
                        if pos >= data.len() { break; }
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
                        if !valid { break; }
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
                return false;
            }
        }
    }
    false
}
