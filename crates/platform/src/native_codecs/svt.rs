use std::ffi::{CStr, c_char};

use filmcraft_export::{BitrateMode, ColorSignal, EncodedPacket, EncoderFrame, ExportSettings, FieldOrder, Format, Result, VideoEncoder};
use filmcraft_isobmff::{Av1Config, SampleEntry};
use filmcraft_time::FrameRate;

use super::svt_ffi::{EbBufferHeaderType, EbComponentType, EbSvtAv1EncConfiguration, EbSvtIOFormat};
use super::video_error as err;

type Status = i32;
type InitHandle = unsafe extern "C" fn(*mut *mut EbComponentType, *mut EbSvtAv1EncConfiguration) -> Status;
type Parse = unsafe extern "C" fn(*mut EbSvtAv1EncConfiguration, *const c_char, *const c_char) -> Status;
type SetParameter = unsafe extern "C" fn(*mut EbComponentType, *mut EbSvtAv1EncConfiguration) -> Status;
type HandleCall = unsafe extern "C" fn(*mut EbComponentType) -> Status;
type SendPicture = unsafe extern "C" fn(*mut EbComponentType, *mut EbBufferHeaderType) -> Status;
type GetPacket = unsafe extern "C" fn(*mut EbComponentType, *mut *mut EbBufferHeaderType, u8) -> Status;
type Release = unsafe extern "C" fn(*mut *mut EbBufferHeaderType);
type Header = unsafe extern "C" fn(*mut EbComponentType, *mut *mut EbBufferHeaderType) -> Status;
type ReleaseHeader = unsafe extern "C" fn(*mut EbBufferHeaderType) -> Status;

const EOS: u32 = 1;
const EMPTY: i32 = 0x80002033u32 as i32;

struct Api {
    init_handle: InitHandle,
    set: SetParameter,
    parse: Parse,
    init: HandleCall,
    deinit: HandleCall,
    destroy: HandleCall,
    send: SendPicture,
    get: GetPacket,
    release: Release,
    header: Header,
    release_header: ReleaseHeader,
    _library: libloading::Library,
}

impl Api {
    fn load() -> std::result::Result<Self, String> {
        let library = super::load("FILMCRAFT_SVT_AV1_LIBRARY", "SvtAv1Enc.dll")?;
        // SAFETY: all signatures come from the public 4.3.0 ABI-0 header. Check the version before
        // handing a structure to the library; older releases have a different init_handle signature.
        unsafe {
            let version = library.get::<unsafe extern "C" fn() -> *const c_char>(b"svt_av1_get_version\0").map_err(|e| e.to_string())?;
            let ptr = version();
            if ptr.is_null() {
                return Err("no SVT version".into());
            }
            let version = CStr::from_ptr(ptr).to_string_lossy();
            if !version.trim_start_matches('v').starts_with("4.3.0") {
                return Err(format!("need SVT-AV1 4.3.0 mainline + 444/12-bit/SCD, got {version}"));
            }
            macro_rules! sym {
                ($name:literal) => {
                    *library.get(concat!($name, "\0").as_bytes()).map_err(|e| e.to_string())?
                };
            }
            Ok(Self {
                init_handle: sym!("svt_av1_enc_init_handle"),
                set: sym!("svt_av1_enc_set_parameter"),
                parse: sym!("svt_av1_enc_parse_parameter"),
                init: sym!("svt_av1_enc_init"),
                deinit: sym!("svt_av1_enc_deinit"),
                destroy: sym!("svt_av1_enc_deinit_handle"),
                send: sym!("svt_av1_enc_send_picture"),
                get: sym!("svt_av1_enc_get_packet"),
                release: sym!("svt_av1_enc_release_out_buffer"),
                header: sym!("svt_av1_enc_stream_header"),
                release_header: sym!("svt_av1_enc_stream_header_release"),
                _library: library,
            })
        }
    }
}

pub fn available() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| Api::load().is_ok())
}

struct Encoder {
    api: Api,
    handle: *mut EbComponentType,
    initialized: bool,
    config: Av1Config,
    width: u32,
    height: u32,
    rate: FrameRate,
    signal: ColorSignal,
    first_dts: Option<i64>,
    ended: bool,
    planes: [Vec<u16>; 3],
}

// SAFETY: one encoder exclusively owns its SVT handle. Calls are serialized through &mut self;
// SVT owns and synchronizes its worker threads and has no caller-thread affinity.
unsafe impl Send for Encoder {}

fn check(status: Status) -> Result<()> {
    if status == 0 { Ok(()) } else { Err(err(format!("library status {status:#x}"))) }
}

impl Encoder {
    fn new(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<Self> {
        s.validate()?;
        if w < 64
            || h < 64
            || w > 8192
            || h > 8192
            || !w.is_multiple_of(2)
            || !h.is_multiple_of(2)
            || s.field_order != FieldOrder::Progressive
            || s.pixel_aspect.is_some_and(|(n, d)| n != d)
            || rate.num <= 0
            || rate.den <= 0
        {
            return Err(err("needs progressive even dimensions 64..8192, square pixels and a positive frame rate"));
        }
        let mut enc = Self {
            api: Api::load().map_err(err)?,
            handle: std::ptr::null_mut(),
            initialized: false,
            config: Av1Config::default(),
            width: w,
            height: h,
            rate,
            signal: s.signal,
            first_dts: None,
            ended: false,
            planes: Default::default(),
        };
        let mut cfg = EbSvtAv1EncConfiguration::default();
        // SAFETY: cfg matches the pinned public ABI and handle is an owned out-pointer. SVT copies
        // configuration at set_parameter. No pointers to local configuration survive this call.
        unsafe {
            check((enc.api.init_handle)(&mut enc.handle, &mut cfg))?;
            cfg.source_width = w;
            cfg.source_height = h;
            cfg.frame_rate_numerator = u32::try_from(rate.num).map_err(err)?;
            cfg.frame_rate_denominator = u32::try_from(rate.den).map_err(err)?;
            cfg.encoder_bit_depth = 10;
            cfg.encoder_color_format = 1;
            cfg.profile = 0;
            cfg.enc_mode = s.av1_preset as i8;
            cfg.rate_control_mode = 0;
            cfg.aq_mode = 2;
            let crf = std::ffi::CString::new(s.crf.to_string()).map_err(err)?;
            check((enc.api.parse)(&mut cfg, c"crf".as_ptr(), crf.as_ptr()))?;
            cfg.intra_refresh_type = 2;
            cfg.scene_change_detection = u32::from(s.keyframe_distance.filter(|k| *k > 0).is_none());
            cfg.intra_period_length = s.keyframe_distance.filter(|k| *k > 0).map(|k| k.saturating_sub(1).min(i32::MAX as u32) as i32).unwrap_or(-2);
            cfg.color_primaries = i32::from(s.signal.primaries);
            cfg.transfer_characteristics = i32::from(s.signal.transfer);
            cfg.matrix_coefficients = i32::from(s.signal.matrix);
            cfg.color_range = 0;
            cfg.level_of_parallelism = s.max_threads.min(32);
            check((enc.api.set)(enc.handle, &mut cfg))?;
            check((enc.api.init)(enc.handle))?;
            enc.initialized = true;
            let mut header = std::ptr::null_mut();
            check((enc.api.header)(enc.handle, &mut header))?;
            if header.is_null() {
                return Err(err("missing sequence header"));
            }
            let bytes = std::slice::from_raw_parts((*header).p_buffer, (*header).n_filled_len as usize);
            let config = crate::nvenc::av1::av1_config(bytes, true).map_err(err);
            (enc.api.release_header)(header);
            enc.config = config?;
        }
        Ok(enc)
    }

    fn receive(&mut self, blocking: bool) -> Result<Vec<EncodedPacket>> {
        let mut packets = Vec::new();
        loop {
            let mut packet = std::ptr::null_mut();
            // SAFETY: an owned live handle and an out-pointer; get_packet owns returned storage
            // until release. Copy bytes and timestamps before releasing exactly once.
            unsafe {
                let status = (self.api.get)(self.handle, &mut packet, u8::from(blocking));
                if status == EMPTY {
                    break;
                }
                check(status)?;
                if packet.is_null() {
                    return Err(err("missing output packet"));
                }
                let p = &*packet;
                let eos = p.flags & EOS != 0;
                if p.n_filled_len > 0 {
                    self.first_dts.get_or_insert(p.dts);
                    let offset = p.pts.saturating_sub(p.dts).saturating_mul(self.rate.den);
                    packets.push(EncodedPacket {
                        data: std::slice::from_raw_parts(p.p_buffer, p.n_filled_len as usize).to_vec(),
                        key: p.pic_type == 3,
                        duration: self.rate.den as u32,
                        composition_offset: i32::try_from(offset).map_err(err)?,
                    });
                }
                (self.api.release)(&mut packet);
                if eos {
                    self.ended = true;
                    break;
                }
            }
        }
        Ok(packets)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: exclusively owned handle; destroy after deinit joins SVT's workers, before
        // unloading the library. A setup failure may own a handle that was never initialized.
        unsafe {
            if !self.handle.is_null() {
                if self.initialized {
                    (self.api.deinit)(self.handle);
                }
                (self.api.destroy)(self.handle);
            }
        }
    }
}

impl VideoEncoder for Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let mut e = SampleEntry::av1(self.config.clone(), self.width as u16, self.height as u16);
        self.signal.apply_to(&mut e, false);
        e
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn media_start(&self) -> Option<i64> {
        self.first_dts.filter(|d| *d < 0).map(|d| d.saturating_neg().saturating_mul(self.rate.den))
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if self.ended || (f.width, f.height) != (self.width, self.height) {
            return Err(err("picture size changed or encoder ended"));
        }
        let [y, u, v] = &mut self.planes;
        let (w, h) = (self.width as usize, self.height as usize);
        if let Some(rgb) = f.hdr {
            let (kr, kb) = self.signal.kr_kb();
            filmcraft_export::rgbf_to_yuv420_10(rgb, w, h, filmcraft_export::Matrix::new(kr, kb), y, u, v)?;
        } else {
            filmcraft_export::rgba_to_yuv420_10(f.rgba, w, h, y, u, v)?;
        }
        let mut input = EbSvtIOFormat {
            luma: y.as_mut_ptr().cast(),
            cb: u.as_mut_ptr().cast(),
            cr: v.as_mut_ptr().cast(),
            y_stride: self.width,
            cb_stride: self.width / 2,
            cr_stride: self.width / 2,
        };
        let mut header = EbBufferHeaderType {
            size: std::mem::size_of::<EbBufferHeaderType>() as u32,
            p_buffer: (&mut input as *mut EbSvtIOFormat).cast(),
            n_filled_len: (y.len() + u.len() + v.len()) as u32 * 2,
            pts: i64::try_from(f.index).map_err(err)?,
            pic_type: 255,
            ..Default::default()
        };
        // SAFETY: dimensions and plane lengths were checked by the conversion. SVT copies input
        // planes during send_picture; buffers remain alive and exclusive until the call returns.
        check(unsafe { (self.api.send)(self.handle, &mut header) })?;
        self.receive(false)
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        if self.ended {
            return Ok(Vec::new());
        }
        let mut eos = EbBufferHeaderType { size: std::mem::size_of::<EbBufferHeaderType>() as u32, flags: EOS, ..Default::default() };
        // SAFETY: a live handle and EOS header with no input planes, per the public API.
        check(unsafe { (self.api.send)(self.handle, &mut eos) })?;
        self.receive(true)
    }
}

pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Av1 && s.bitrate_mode == BitrateMode::Crf).then(|| Encoder::new(w, h, rate, s).map(|e| Box::new(e) as Box<dyn VideoEncoder>))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_keyframes_follow_a_scene_cut() {
        if !available() {
            eprintln!("SKIPPED: no SVT-AV1 mainline library");
            return;
        }
        let settings = ExportSettings { format: Format::Av1, bitrate_mode: BitrateMode::Crf, crf: 35.0, av1_preset: 6, ..Default::default() };
        let mut encoder = Encoder::new(160, 96, FrameRate::FPS_24, &settings).unwrap();
        let mut packets = Vec::new();
        for index in 0..96 {
            let rgba = vec![if index < 48 { 0 } else { 255 }; 160 * 96 * 4];
            packets.extend(encoder.encode(&EncoderFrame { width: 160, height: 96, rgba: &rgba, hdr: None, index }).unwrap());
        }
        packets.extend(encoder.flush().unwrap());
        assert_eq!(packets.len(), 96);
        let keys: Vec<usize> = packets.iter().enumerate().filter_map(|(i, p)| p.key.then_some(i)).collect();
        assert_eq!(keys, vec![0, 48], "SCD must insert a keyframe at the cut, before the automatic maximum GOP");
    }
}
