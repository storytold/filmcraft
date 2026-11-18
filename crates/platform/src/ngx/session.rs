//! Windows Direct3D 11 + NVIDIA NGX SDK (DLAA with a Preset K hint)
//! hardware neural reconstruction session.
//!
//! Experimental SDK build: every `unsafe` block has a `// SAFETY:` comment,
//! the public API is safe and returns `Result<_, String>`, and no raw driver/COM pointers escape.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use filmcraft_render::Image;
use filmcraft_render::flow::FlowField;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS, D3D11_CPU_ACCESS_FLAG, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R16G16_FLOAT, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R32_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::core::Interface;

use super::ffi::{
    NVSDK_NGX_D3D11_CreateFeature, NVSDK_NGX_D3D11_DestroyParameters, NVSDK_NGX_D3D11_EvaluateFeature, NVSDK_NGX_D3D11_GetCapabilityParameters,
    NVSDK_NGX_D3D11_Init_with_ProjectID, NVSDK_NGX_D3D11_ReleaseFeature, NVSDK_NGX_D3D11_Shutdown1, NVSDK_NGX_DLSS_Feature_Flags_AutoExposure,
    NVSDK_NGX_DLSS_Feature_Flags_IsHDR, NVSDK_NGX_DLSS_Feature_Flags_MVLowRes, NVSDK_NGX_DLSS_Hint_Render_Preset_K, NVSDK_NGX_ENGINE_TYPE_CUSTOM,
    NVSDK_NGX_Feature_SuperSampling, NVSDK_NGX_FeatureCommonInfo, NVSDK_NGX_Handle, NVSDK_NGX_LOGGING_LEVEL_OFF, NVSDK_NGX_LoggingInfo, NVSDK_NGX_Parameter,
    NVSDK_NGX_Parameter_SetD3d11Resource, NVSDK_NGX_Parameter_SetF, NVSDK_NGX_Parameter_SetI, NVSDK_NGX_Parameter_SetUI, NVSDK_NGX_PathListInfo,
    NVSDK_NGX_PerfQuality_Value_DLAA, NVSDK_NGX_Result, NVSDK_NGX_Result_Success, NVSDK_NGX_VERSION_API,
};

/// IEEE 754 `f32` → half-precision `u16` (round to nearest even).
#[inline]
fn f32_to_f16(v: f32) -> u16 {
    if !v.is_finite() {
        return 0;
    }
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let sig = mant | 0x80_0000;
        let shift = 14 - e;
        let kept = sig >> shift;
        let midpoint = 1 << (shift - 1);
        let remainder = sig & ((1 << shift) - 1);
        let up = remainder > midpoint || (remainder == midpoint && kept & 1 != 0);
        return sign | (kept + u32::from(up)) as u16;
    }
    let m = mant >> 13;
    let round_bits = mant & 0x1fff;
    let mut h = u32::from(sign) | ((e as u32) << 10) | m;
    if round_bits > 0x1000 || (round_bits == 0x1000 && (m & 1) == 1) {
        h += 1;
    }
    h as u16
}

/// IEEE 754 half-precision `u16` → `f32`.
#[inline]
fn f16_to_f32(h: u16) -> f32 {
    let u = u32::from(h);
    let sign = (u & 0x8000) << 16;
    let exp = (u >> 10) & 0x1f;
    let mant = u & 0x03ff;
    let bits = match exp {
        0 => {
            if mant == 0 {
                sign
            } else {
                let b = 31 - mant.leading_zeros();
                let frac = (mant ^ (1 << b)) << (23 - b);
                sign | ((103 + b) << 23) | frac
            }
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 112) << 23) | (mant << 13),
    };
    let v = f32::from_bits(bits);
    if v.is_finite() { v } else { 0.0 }
}

/// Bytes a mapped `RGBA16F` readback of `w` × `h` pixels with `pitch` bytes per row spans: `h - 1`
/// full rows and one last row of `w * 8` bytes. Fails when a row does not fit in `pitch`.
fn readback_len(pitch: usize, w: usize, h: usize) -> Result<usize, String> {
    let row_bytes = w.checked_mul(8).ok_or("NGX readback row size overflows")?;
    if pitch < row_bytes {
        return Err(format!("D3D11 Map(staging) row pitch {pitch} is shorter than a {w}-pixel RGBA16F row ({row_bytes} bytes)"));
    }
    if h == 0 {
        return Ok(0);
    }
    pitch.checked_mul(h - 1).and_then(|v| v.checked_add(row_bytes)).ok_or_else(|| "NGX readback size overflows".to_string())
}

/// Unpack a mapped `RGBA16F` readback (`pitch` bytes per row) into an image, keeping `curr`'s
/// alpha. Missing or short data is an error, never a black frame: the caller then falls back to
/// the CPU path instead of baking an empty picture into the overlay clip.
fn unpack_rgba16f(raw: Option<&[u8]>, pitch: usize, w: usize, h: usize, curr: &Image) -> Result<Image, String> {
    let raw = raw.ok_or("D3D11 Map(staging) returned no data")?;
    let len = readback_len(pitch, w, h)?;
    if raw.len() < len {
        return Err(format!("D3D11 Map(staging) returned {} bytes for a {w}x{h} readback that needs {len}", raw.len()));
    }
    let row_bytes = w.saturating_mul(8);
    let mut out = Image::new(w, h);
    for y in 0..h {
        let start = y.saturating_mul(pitch);
        let row = raw.get(start..start.saturating_add(row_bytes)).ok_or("NGX readback row out of range")?;
        for (x, px_bytes) in row.as_chunks::<8>().0.iter().enumerate() {
            let i = (y * w + x) * 4;
            let r = f16_to_f32(u16::from_le_bytes([px_bytes[0], px_bytes[1]]));
            let g = f16_to_f32(u16::from_le_bytes([px_bytes[2], px_bytes[3]]));
            let b = f16_to_f32(u16::from_le_bytes([px_bytes[4], px_bytes[5]]));
            let a = curr.px.get(i + 3).copied().unwrap_or(1.0);
            if let Some(dst) = out.px.get_mut(i..i + 4) {
                dst.copy_from_slice(&[r, g, b, a]);
            }
        }
    }
    Ok(out)
}

fn check_ngx(res: NVSDK_NGX_Result, op: &str) -> Result<(), String> {
    if res == NVSDK_NGX_Result_Success { Ok(()) } else { Err(format!("{op} failed (NVSDK_NGX_Result = {res:#x})")) }
}

fn wide_null(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// Explicit SDK installs must contain the named release library, not an OTA cache entry.
fn dlss_search_dir(dir: PathBuf) -> Result<PathBuf, String> {
    if !dir.is_absolute() || !dir.join("nvngx_dlss.dll").is_file() {
        return Err("FILMCRAFT_DLSS_RUNTIME_DIR must name an absolute directory containing the official nvngx_dlss.dll".into());
    }
    Ok(dir)
}

/// Process-wide initialized NGX + Direct3D 11 runtime, guarded by [`runtime_lock`].
struct NgxRuntime {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    sessions: usize,
}

impl Drop for NgxRuntime {
    fn drop(&mut self) {
        // SAFETY: the last feature has been released under the runtime lock. Both the device
        // and the library owning this entry point are live through this call.
        let result = unsafe { NVSDK_NGX_D3D11_Shutdown1(self.device.as_raw()) };
        if let Err(e) = check_ngx(result, "NVSDK_NGX_D3D11_Shutdown1") {
            log::warn!("{e}");
        }
    }
}

fn runtime_lock() -> &'static Mutex<Option<NgxRuntime>> {
    static RT: OnceLock<Mutex<Option<NgxRuntime>>> = OnceLock::new();
    RT.get_or_init(|| Mutex::new(None))
}

/// Cache only a successful initialization so a transient driver failure can be retried.
fn get_or_try_init<T>(slot: &mut Option<T>, init: impl FnOnce() -> Result<T, String>) -> Result<&mut T, String> {
    match slot {
        Some(rt) => Ok(rt),
        None => Ok(slot.insert(init()?)),
    }
}

fn init_runtime() -> Result<NgxRuntime, String> {
    let search_dir = std::env::var_os("FILMCRAFT_DLSS_RUNTIME_DIR").map(PathBuf::from).map(dlss_search_dir).transpose()?;
    let data_dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("no local application data directory")?
        .join("FilmCraft")
        .join("NGX");
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("creating NGX application data directory: {e}"))?;

    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let (mut device, mut context) = (None, None);
    // SAFETY: creates a hardware Direct3D 11 device on the default adapter.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("D3D11CreateDevice for NGX failed: {e}"))?;
    let (device, context) = device.zip(context).ok_or("D3D11CreateDevice returned no device")?;
    let mt = device.cast::<ID3D11Multithread>().map_err(|e| format!("NGX D3D11 multithread protection: {e}"))?;
    // SAFETY: turns on multithread protection for this device before it is shared.
    let _ = unsafe { mt.SetMultithreadProtected(true) };

    let dir_wide = search_dir.as_deref().map(wide_null);
    let path_ptrs = [dir_wide.as_ref().map_or(std::ptr::null(), |dir| dir.as_ptr())];
    let info = NVSDK_NGX_FeatureCommonInfo {
        PathListInfo: NVSDK_NGX_PathListInfo {
            Path: if dir_wide.is_some() { path_ptrs.as_ptr() } else { std::ptr::null() },
            Length: u32::from(dir_wide.is_some()),
        },
        InternalData: std::ptr::null_mut(),
        LoggingInfo: NVSDK_NGX_LoggingInfo {
            LoggingCallback: std::ptr::null(),
            MinimumLoggingLevel: NVSDK_NGX_LOGGING_LEVEL_OFF,
            DisableOtherLoggingSinks: false,
        },
    };
    let app_data = wide_null(&data_dir);
    // A UUID generated for FilmCraft's custom engine, stable across sessions.
    // SAFETY: the strings and optional path list live through init, and the device is live until shutdown.
    let st = unsafe {
        NVSDK_NGX_D3D11_Init_with_ProjectID(
            c"de2361fe-977f-4408-8fdf-28ce19825ef6".as_ptr(),
            NVSDK_NGX_ENGINE_TYPE_CUSTOM,
            concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast(),
            app_data.as_ptr(),
            device.as_raw(),
            &info,
            NVSDK_NGX_VERSION_API,
        )
    };
    check_ngx(st, "NVSDK_NGX_D3D11_Init_with_ProjectID")?;

    Ok(NgxRuntime { device, context, sessions: 0 })
}

fn with_runtime<T>(f: impl FnOnce(&mut NgxRuntime) -> Result<T, String>) -> Result<T, String> {
    let mut guard = runtime_lock().lock().unwrap_or_else(PoisonError::into_inner);
    let rt = get_or_try_init(&mut guard, init_runtime)?;
    let result = f(rt);
    if rt.sessions == 0 {
        guard.take();
    }
    result
}

fn set_ui(params: *mut NVSDK_NGX_Parameter, name: &CStr, val: u32) {
    // SAFETY: the live session owns this NGX parameter block; the C string lives across the call.
    unsafe { NVSDK_NGX_Parameter_SetUI(params, name.as_ptr(), val) }
}

fn set_i(params: *mut NVSDK_NGX_Parameter, name: &CStr, val: i32) {
    // SAFETY: the live session owns this NGX parameter block; the C string lives across the call.
    unsafe { NVSDK_NGX_Parameter_SetI(params, name.as_ptr(), val) }
}

fn set_f(params: *mut NVSDK_NGX_Parameter, name: &CStr, val: f32) {
    // SAFETY: the live session owns this NGX parameter block; the C string lives across the call.
    unsafe { NVSDK_NGX_Parameter_SetF(params, name.as_ptr(), val) }
}

fn set_d3d11_resource(params: *mut NVSDK_NGX_Parameter, name: &CStr, tex: &ID3D11Texture2D) -> Result<(), String> {
    let res: ID3D11Resource = tex.cast().map_err(|e| format!("ID3D11Texture2D -> ID3D11Resource failed: {e}"))?;
    // SAFETY: `params` is valid and `res.as_raw()` is a live `ID3D11Resource*` outliving evaluation.
    unsafe {
        NVSDK_NGX_Parameter_SetD3d11Resource(params, name.as_ptr(), res.as_raw());
    }
    Ok(())
}

fn create_tex2d(
    device: &ID3D11Device,
    (width, height): (u32, u32),
    format: DXGI_FORMAT,
    usage: D3D11_USAGE,
    bind: D3D11_BIND_FLAG,
    cpu_access: D3D11_CPU_ACCESS_FLAG,
    zero_bytes: Option<(&[u8], u32)>,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: cpu_access.0 as u32,
        MiscFlags: 0,
    };
    let sub = zero_bytes.map(|(buf, bpp)| D3D11_SUBRESOURCE_DATA {
        pSysMem: buf.as_ptr().cast(),
        SysMemPitch: width.saturating_mul(bpp),
        SysMemSlicePitch: width.saturating_mul(height).saturating_mul(bpp),
    });
    let mut tex = None;
    // SAFETY: `desc` and optional `sub` point to valid stack/slice memory for the call duration.
    unsafe { device.CreateTexture2D(&desc, sub.as_ref().map(std::ptr::from_ref), Some(&mut tex)) }
        .map_err(|e| format!("CreateTexture2D({width}x{height}, fmt={}): {e}", format.0))?;
    tex.ok_or_else(|| "CreateTexture2D returned null".into())
}

/// One hardware NGX DLSS (DLAA, Preset K Transformer) session bound to a fixed `(width, height)`.
pub struct Session {
    width: usize,
    height: usize,
    params: *mut NVSDK_NGX_Parameter,
    handle: *mut NVSDK_NGX_Handle,
    color_in: ID3D11Texture2D,
    color_out: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    mvec_in: ID3D11Texture2D,
    depth_in: ID3D11Texture2D,
    color_upload: Vec<u8>,
    mvec_upload: Vec<u8>,
}

// SAFETY: `Session` accesses the NGX D3D11 runtime exclusively under the runtime mutex. The D3D11 device was created
// without SINGLETHREADED. Its texture COM references may be released on the dropping thread
// after the feature and parameter block have been released under that lock.
unsafe impl Send for Session {}

impl Session {
    /// Probe whether the system has a working NVIDIA NGX DLSS runtime.
    pub fn probe() -> Result<(), String> {
        let _ = Self::new(64, 64)?;
        Ok(())
    }

    /// Open a hardware NGX DLSS (Preset K Transformer) session for frames of `width` × `height`.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if !(32..=8192).contains(&width) || !(32..=8192).contains(&height) {
            return Err(format!("frame size {width}x{height} is outside NGX DLSS bounds (32..=8192)"));
        }
        with_runtime(|rt| {
            let (w, h) = (width as usize, height as usize);
            let pixels = w.checked_mul(h).ok_or("NGX frame pixel count overflows")?;
            let upload = |stride: usize| -> Result<Vec<u8>, String> {
                let len = pixels.checked_mul(stride).ok_or("NGX upload size overflows")?;
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(len).map_err(|e| format!("allocating {width}x{height} NGX upload: {e}"))?;
                bytes.resize(len, 0);
                Ok(bytes)
            };
            let color_upload = upload(8)?;
            let mvec_upload = upload(4)?;
            let mut params: *mut NVSDK_NGX_Parameter = std::ptr::null_mut();
            // SAFETY: `params` is a valid out-pointer.
            let st = unsafe { NVSDK_NGX_D3D11_GetCapabilityParameters(&mut params) };
            check_ngx(st, "NVSDK_NGX_D3D11_GetCapabilityParameters")?;
            if params.is_null() {
                return Err("NVSDK_NGX_D3D11_GetCapabilityParameters returned null".into());
            }

            let setup = (|| -> Result<*mut NVSDK_NGX_Handle, String> {
                set_ui(params, c"CreationNodeMask", 1);
                set_ui(params, c"VisibilityNodeMask", 1);
                set_ui(params, c"Width", width);
                set_ui(params, c"Height", height);
                set_ui(params, c"OutWidth", width);
                set_ui(params, c"OutHeight", height);
                set_i(params, c"PerfQualityValue", NVSDK_NGX_PerfQuality_Value_DLAA);
                let flags = NVSDK_NGX_DLSS_Feature_Flags_IsHDR | NVSDK_NGX_DLSS_Feature_Flags_MVLowRes | NVSDK_NGX_DLSS_Feature_Flags_AutoExposure;
                set_i(params, c"DLSS.Feature.Create.Flags", flags);
                set_ui(params, c"DLSS.Hint.Render.Preset.DLAA", NVSDK_NGX_DLSS_Hint_Render_Preset_K);

                let mut handle: *mut NVSDK_NGX_Handle = std::ptr::null_mut();
                // SAFETY: `rt.context` is the initialized D3D11 immediate context and `params` is populated.
                let st = unsafe { NVSDK_NGX_D3D11_CreateFeature(rt.context.as_raw(), NVSDK_NGX_Feature_SuperSampling, params, &mut handle) };
                check_ngx(st, "NVSDK_NGX_D3D11_CreateFeature(SuperSampling, DLAA, Preset K)")?;
                if handle.is_null() {
                    return Err("NVSDK_NGX_D3D11_CreateFeature returned null handle".into());
                }
                Ok(handle)
            })();

            let handle = match setup {
                Ok(h) => h,
                Err(e) => {
                    // SAFETY: destroys the parameter block allocated above on setup error.
                    let _ = unsafe { NVSDK_NGX_D3D11_DestroyParameters(params) };
                    return Err(e);
                }
            };

            let textures = (|| -> Result<(ID3D11Texture2D, ID3D11Texture2D, ID3D11Texture2D, ID3D11Texture2D, ID3D11Texture2D), String> {
                let srv = D3D11_BIND_SHADER_RESOURCE;
                let uav_rtv_srv = D3D11_BIND_FLAG(D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_UNORDERED_ACCESS.0);
                let no_cpu = D3D11_CPU_ACCESS_FLAG(0);
                let sz = (width, height);
                let color_in = create_tex2d(&rt.device, sz, DXGI_FORMAT_R16G16B16A16_FLOAT, D3D11_USAGE_DEFAULT, srv, no_cpu, None)?;
                let color_out = create_tex2d(&rt.device, sz, DXGI_FORMAT_R16G16B16A16_FLOAT, D3D11_USAGE_DEFAULT, uav_rtv_srv, no_cpu, None)?;
                let staging =
                    create_tex2d(&rt.device, sz, DXGI_FORMAT_R16G16B16A16_FLOAT, D3D11_USAGE_STAGING, D3D11_BIND_FLAG(0), D3D11_CPU_ACCESS_READ, None)?;
                let mvec_in = create_tex2d(&rt.device, sz, DXGI_FORMAT_R16G16_FLOAT, D3D11_USAGE_DEFAULT, srv, no_cpu, Some((&mvec_upload, 4)))?;
                let depth_in = create_tex2d(&rt.device, sz, DXGI_FORMAT_R32_FLOAT, D3D11_USAGE_DEFAULT, srv, no_cpu, Some((&mvec_upload, 4)))?;
                Ok((color_in, color_out, staging, mvec_in, depth_in))
            })();

            let (color_in, color_out, staging, mvec_in, depth_in) = match textures {
                Ok(t) => t,
                Err(e) => {
                    // SAFETY: releases the feature handle and parameter block when texture creation fails.
                    let _ = unsafe { NVSDK_NGX_D3D11_ReleaseFeature(handle) };
                    // SAFETY: see above.
                    let _ = unsafe { NVSDK_NGX_D3D11_DestroyParameters(params) };
                    return Err(e);
                }
            };

            rt.sessions += 1;
            Ok(Self { width: w, height: h, params, handle, color_in, color_out, staging, mvec_in, depth_in, color_upload, mvec_upload })
        })
    }

    /// Evaluate hardware NGX DLSS (Preset K Transformer) on `curr` and `field` (`prev -> curr`).
    pub fn enhance(&mut self, curr: &Image, field: &FlowField, sequential: bool) -> Result<Image, String> {
        let (w, h) = (self.width, self.height);
        let pixels = w.checked_mul(h).ok_or("frame size overflows")?;
        if curr.w != w || curr.h != h || curr.px.len() < pixels.saturating_mul(4) {
            return Err(format!("expected {w}x{h} frame, got {}x{}", curr.w, curr.h));
        }

        // Pack linear RGBA f32 into RGBA16F (`self.color_upload`: 8 bytes per pixel).
        for (dst, src) in self.color_upload.as_chunks_mut::<8>().0.iter_mut().zip(curr.px.as_chunks::<4>().0) {
            let r = f32_to_f16(src[0].clamp(0.0, 16.0)).to_le_bytes();
            let g = f32_to_f16(src[1].clamp(0.0, 16.0)).to_le_bytes();
            let b = f32_to_f16(src[2].clamp(0.0, 16.0)).to_le_bytes();
            let a = f32_to_f16(src[3].clamp(0.0, 1.0)).to_le_bytes();
            *dst = [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]];
        }

        // Pack `FlowField` into full-frame pixel-space RG16F motion vectors (`-dx, -dy`: pointing
        // from `curr` back to `prev`, matching `MV.Scale = 1.0`).
        for y in 0..h {
            let py = y as f32 + 0.5;
            let row_off = y * w;
            for x in 0..w {
                let px = x as f32 + 0.5;
                let ([dx, dy], _) = field.sample_px(px, py);
                let mvx = f32_to_f16(if dx.is_finite() { -dx } else { 0.0 }).to_le_bytes();
                let mvy = f32_to_f16(if dy.is_finite() { -dy } else { 0.0 }).to_le_bytes();
                let i = (row_off + x) * 4;
                if let Some(dst) = self.mvec_upload.get_mut(i..i + 4) {
                    dst.copy_from_slice(&[mvx[0], mvx[1], mvy[0], mvy[1]]);
                }
            }
        }

        with_runtime(|rt| {
            let (w32, h32) = (w as u32, h as u32);
            // SAFETY: `self.color_upload` holds `w * h * 8` bytes and `self.mvec_upload` holds `w * h * 4` bytes.
            unsafe {
                rt.context.UpdateSubresource(&self.color_in, 0, None, self.color_upload.as_ptr().cast(), w32 * 8, w32 * h32 * 8);
                rt.context.UpdateSubresource(&self.mvec_in, 0, None, self.mvec_upload.as_ptr().cast(), w32 * 4, w32 * h32 * 4);
            }

            set_d3d11_resource(self.params, c"Color", &self.color_in)?;
            set_d3d11_resource(self.params, c"Output", &self.color_out)?;
            set_d3d11_resource(self.params, c"MotionVectors", &self.mvec_in)?;
            set_d3d11_resource(self.params, c"Depth", &self.depth_in)?;
            set_i(self.params, c"Reset", i32::from(!sequential));
            set_f(self.params, c"MV.Scale.X", 1.0);
            set_f(self.params, c"MV.Scale.Y", 1.0);
            set_f(self.params, c"Jitter.Offset.X", 0.0);
            set_f(self.params, c"Jitter.Offset.Y", 0.0);
            set_f(self.params, c"DLSS.Pre.Exposure", 1.0);
            set_f(self.params, c"DLSS.Exposure.Scale", 1.0);
            set_ui(self.params, c"DLSS.Render.Subrect.Dimensions.Width", w32);
            set_ui(self.params, c"DLSS.Render.Subrect.Dimensions.Height", h32);

            // SAFETY: `self.handle` and `self.params` belong to this session and all bound D3D11
            // textures are live for the call.
            let st = unsafe { NVSDK_NGX_D3D11_EvaluateFeature(rt.context.as_raw(), self.handle, self.params, std::ptr::null()) };
            check_ngx(st, "NVSDK_NGX_D3D11_EvaluateFeature")?;

            // SAFETY: copies the GPU output texture into our CPU-readable staging texture of identical size and format.
            unsafe {
                rt.context.CopyResource(&self.staging, &self.color_out);
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: maps `self.staging` for CPU read access; unmapped before returning.
            unsafe { rt.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| format!("D3D11 Map(staging) failed: {e}"))?;
            let pitch = mapped.RowPitch as usize;
            let out = match (mapped.pData.is_null(), readback_len(pitch, w, h)) {
                (true, _) => unpack_rgba16f(None, pitch, w, h, curr),
                (false, Err(e)) => Err(e),
                (false, Ok(len)) => {
                    // SAFETY: while mapped, `mapped.pData` is readable for `h - 1` rows of `pitch`
                    // bytes plus one last row of `w * 8` bytes, which is exactly `len`.
                    let raw = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len) };
                    unpack_rgba16f(Some(raw), pitch, w, h, curr)
                }
            };
            // SAFETY: balances the successful `Map` call above.
            unsafe {
                rt.context.Unmap(&self.staging, 0);
            }
            out
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = with_runtime(|rt| {
            if !self.handle.is_null() {
                // SAFETY: releases this session's DLSS feature handle.
                let _ = unsafe { NVSDK_NGX_D3D11_ReleaseFeature(self.handle) };
                self.handle = std::ptr::null_mut();
            }
            if !self.params.is_null() {
                // SAFETY: destroys this session's NGX parameter block.
                let _ = unsafe { NVSDK_NGX_D3D11_DestroyParameters(self.params) };
                self.params = std::ptr::null_mut();
            }
            rt.sessions -= 1;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests;
