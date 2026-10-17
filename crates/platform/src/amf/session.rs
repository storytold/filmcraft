//! AMD AMF encoder session management.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use super::device::Device;
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
    _device: Device,
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
        let device = Device::new()?;

        let mut context: *mut AmfContext = std::ptr::null_mut();
        // SAFETY: calling AMFFactory::CreateContext with live out-pointer.
        let res = unsafe { ((*(*factory).vtbl).create_context)(factory, &mut context) };
        if res != AMF_OK || context.is_null() {
            return Err(format!("AMF CreateContext failed ({res})"));
        }

        #[cfg(target_os = "windows")]
        {
            // SAFETY: calling AMFContext::InitDX11 with D3D11 device.
            let res = unsafe { ((*(*context).vtbl).init_dx11)(context, device.as_raw(), AMF_DX11_1) };
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
        let rate_control = if cfg.cbr { 1 } else { 2 };

        // SAFETY: setting properties on live AMFComponent before Init.
        unsafe {
            let set = (*(*component).vtbl).set_property;
            match codec {
                Codec::H264 => {
                    let prof_val = match cfg.profile {
                        Profile::H264Baseline => 66,
                        Profile::H264Main => 77,
                        _ => 100,
                    };
                    set(component, windows::core::w!("Usage").as_ptr(), AmfVariant::int64(0));
                    set(component, windows::core::w!("Profile").as_ptr(), AmfVariant::int64(prof_val));
                    set(component, windows::core::w!("FrameSize").as_ptr(), AmfVariant::size(w, h));
                    set(component, windows::core::w!("FrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                    set(component, windows::core::w!("TargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                    set(component, windows::core::w!("PeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                    set(component, windows::core::w!("RateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                    set(component, windows::core::w!("GOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
                }
                Codec::Hevc => {
                    let prof_val = if ten_bit { 2 } else { 1 };
                    set(component, windows::core::w!("HevcUsage").as_ptr(), AmfVariant::int64(0));
                    set(component, windows::core::w!("HevcProfile").as_ptr(), AmfVariant::int64(prof_val));
                    set(component, windows::core::w!("HevcFrameSize").as_ptr(), AmfVariant::size(w, h));
                    set(component, windows::core::w!("HevcFrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                    set(component, windows::core::w!("HevcTargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                    set(component, windows::core::w!("HevcPeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                    set(component, windows::core::w!("HevcRateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                    set(component, windows::core::w!("HevcGOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
                    if ten_bit {
                        set(component, windows::core::w!("HevcNominalRange").as_ptr(), AmfVariant::int64(1));
                    }
                }
                Codec::Av1 => {
                    set(component, windows::core::w!("Av1Usage").as_ptr(), AmfVariant::int64(0));
                    set(component, windows::core::w!("Av1Profile").as_ptr(), AmfVariant::int64(1));
                    set(component, windows::core::w!("Av1FrameSize").as_ptr(), AmfVariant::size(w, h));
                    set(component, windows::core::w!("Av1FrameRate").as_ptr(), AmfVariant::rate(cfg.fps.0, cfg.fps.1));
                    set(component, windows::core::w!("Av1TargetBitrate").as_ptr(), AmfVariant::int64(target_bps));
                    set(component, windows::core::w!("Av1PeakBitrate").as_ptr(), AmfVariant::int64(peak_bps));
                    set(component, windows::core::w!("Av1RateControlMethod").as_ptr(), AmfVariant::int64(rate_control));
                    set(component, windows::core::w!("Av1GOPSize").as_ptr(), AmfVariant::int64(i64::from(cfg.keyint.max(1))));
                    if ten_bit {
                        set(component, windows::core::w!("Av1ColorBitDepth").as_ptr(), AmfVariant::int64(10));
                    }
                }
            }
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

        Ok(Session { _device: device, context, component, codec, ten_bit, width: cfg.width, height: cfg.height })
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

            ((*(*surface).vtbl).set_pts)(surface, pts);
            ((*(*surface).vtbl).set_duration)(surface, duration);

            let res = ((*(*self.component).vtbl).submit_input)(self.component, surface as *mut c_void);
            ((*(*surface).vtbl).release)(surface);
            if res != AMF_OK {
                return Err(format!("AMF SubmitInput failed ({res})"));
            }
        }

        self.query_packets()
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

            ((*(*surface).vtbl).set_pts)(surface, pts);
            ((*(*surface).vtbl).set_duration)(surface, duration);

            let res = ((*(*self.component).vtbl).submit_input)(self.component, surface as *mut c_void);
            ((*(*surface).vtbl).release)(surface);
            if res != AMF_OK {
                return Err(format!("AMF SubmitInput P010 failed ({res})"));
            }
        }

        self.query_packets()
    }

    fn query_packets(&mut self) -> Result<Vec<Packet>, String> {
        let mut packets = Vec::new();
        let mut out_data: *mut c_void = std::ptr::null_mut();
        // SAFETY: polling QueryOutput for ready buffers.
        unsafe {
            loop {
                let res = ((*(*self.component).vtbl).query_output)(self.component, &mut out_data);
                if res == AMF_OK && !out_data.is_null() {
                    let buf = out_data as *mut AmfBuffer;
                    let size = ((*(*buf).vtbl).get_size)(buf);
                    let ptr = ((*(*buf).vtbl).get_native)(buf);
                    let pts = ((*(*buf).vtbl).get_pts)(buf);
                    let slice = std::slice::from_raw_parts(ptr, size);
                    let is_key = is_keyframe(self.codec, slice);
                    packets.push(Packet { data: slice.to_vec(), key: is_key, pts, dts: pts });
                    ((*(*buf).vtbl).release)(buf);
                    out_data = std::ptr::null_mut();
                } else {
                    break;
                }
            }
        }
        Ok(packets)
    }

    pub fn drain(&mut self) -> Result<Vec<Packet>, String> {
        let mut packets = Vec::new();
        // SAFETY: draining the encoder component and polling until AMF_EOF.
        unsafe {
            let res = ((*(*self.component).vtbl).drain)(self.component);
            if res != AMF_OK {
                return Err(format!("AMF Drain failed ({res})"));
            }

            let mut out_data: *mut c_void = std::ptr::null_mut();
            let start = Instant::now();
            loop {
                let res = ((*(*self.component).vtbl).query_output)(self.component, &mut out_data);
                if res == AMF_OK && !out_data.is_null() {
                    let buf = out_data as *mut AmfBuffer;
                    let size = ((*(*buf).vtbl).get_size)(buf);
                    let ptr = ((*(*buf).vtbl).get_native)(buf);
                    let pts = ((*(*buf).vtbl).get_pts)(buf);
                    let slice = std::slice::from_raw_parts(ptr, size);
                    let is_key = is_keyframe(self.codec, slice);
                    packets.push(Packet { data: slice.to_vec(), key: is_key, pts, dts: pts });
                    ((*(*buf).vtbl).release)(buf);
                    out_data = std::ptr::null_mut();
                } else if res == AMF_REPEAT {
                    std::thread::sleep(Duration::from_millis(2));
                    if start.elapsed() > Duration::from_secs(5) {
                        break;
                    }
                } else {
                    break;
                }
            }
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
                    if matches!(nal_type, 19 | 20) {
                        return true;
                    }
                }
            }
            Codec::Av1 => {
                return true;
            }
        }
    }
    false
}
