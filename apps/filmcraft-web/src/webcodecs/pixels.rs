//! Browser planes -> core frames; no color conversion or chroma reconstruction here.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use filmcraft_time::Tick;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{Config, call, get, obj};

// One 8K 4:4:4 16-bit picture + alpha fits within 256 MiB; cap browser-driven allocations.
const MAX_COPY_BYTES: u32 = 256 * 1024 * 1024;

fn integer(value: JsValue) -> Option<u32> {
    let n = value.as_f64()?;
    if n.is_finite() && n >= 0.0 && n <= u32::MAX as f64 && n.fract() == 0.0 { Some(n as u32) } else { None }
}

fn color(frame: &JsValue, fallback: ColorInfo) -> Option<ColorInfo> {
    let c = get(frame, "colorSpace");
    let matrix = match get(&c, "matrix").as_string().as_deref() {
        Some("bt709") => Matrix::Bt709,
        Some("bt470bg" | "smpte170m") => Matrix::Bt601,
        Some("bt2020-ncl") => Matrix::Bt2020Ncl,
        None => fallback.matrix,
        _ => return None,
    };
    let transfer = match get(&c, "transfer").as_string().as_deref() {
        Some("bt709") => Transfer::Bt709,
        Some("iec61966-2-1") => Transfer::Srgb,
        Some("smpte2084") => Transfer::Pq,
        Some("arib-std-b67") => Transfer::Hlg,
        None => fallback.transfer,
        _ => return None,
    };
    let primaries = match get(&c, "primaries").as_string().as_deref() {
        Some("bt709") => Primaries::Bt709,
        Some("bt470bg") => Primaries::Bt601_625,
        Some("smpte170m") => Primaries::Bt601_525,
        Some("bt2020") => Primaries::Bt2020,
        None => fallback.primaries,
        _ => return None,
    };
    let range = match get(&c, "fullRange").as_bool() {
        Some(true) => Range::Full,
        Some(false) => Range::Limited,
        None => fallback.range,
    };
    Some(ColorInfo { matrix, transfer, primaries, range })
}

fn plane(raw: &js_sys::Uint8Array, layout: &JsValue, width: u32, height: u32) -> Result<Vec<u8>, &'static str> {
    let offset = integer(get(layout, "offset")).ok_or("layout")?;
    let stride = integer(get(layout, "stride")).filter(|s| *s >= width).ok_or("layout")?;
    let end =
        height.checked_sub(1).and_then(|h| h.checked_mul(stride)).and_then(|n| offset.checked_add(n)).and_then(|n| n.checked_add(width)).ok_or("layout")?;
    if end > raw.length() {
        return Err("layout");
    }
    let len = width.checked_mul(height).filter(|n| *n <= MAX_COPY_BYTES).ok_or("allocation")? as usize;
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| "memory")?;
    out.resize(len, 0);
    if stride == width {
        raw.subarray(offset, end).copy_to(&mut out);
    } else {
        for (y, row) in out.chunks_exact_mut(width as usize).enumerate() {
            // Checked for the last row above; earlier rows are smaller, but stay checked anyway.
            let start = u32::try_from(y).ok().and_then(|y| y.checked_mul(stride)).and_then(|n| offset.checked_add(n)).ok_or("layout")?;
            let row_end = start.checked_add(width).filter(|e| *e <= end).ok_or("layout")?;
            raw.subarray(start, row_end).copy_to(row);
        }
    }
    Ok(out)
}

/// Copy native 8-bit YUV planes; unsupported formats/geometry return a counted Canvas fallback reason.
pub(super) async fn read(frame: &JsValue, cfg: &Config) -> Result<(VideoFrame, u32), &'static str> {
    let format = get(frame, "format").as_string().ok_or("format")?;
    let (chroma, alpha, interleaved) = match format.as_str() {
        "NV12" => (Chroma::C420, false, true),
        "I420" => (Chroma::C420, false, false),
        "I420A" => (Chroma::C420, true, false),
        "I422" => (Chroma::C422, false, false),
        "I422A" => (Chroma::C422, true, false),
        "I444" => (Chroma::C444, false, false),
        "I444A" => (Chroma::C444, true, false),
        _ => return Err("format"),
    };
    let rect = get(frame, "visibleRect");
    let w = integer(get(&rect, "width")).filter(|n| *n > 0).ok_or("geometry")?;
    let h = integer(get(&rect, "height")).filter(|n| *n > 0).ok_or("geometry")?;
    if integer(get(frame, "displayWidth")) != Some(w)
        || integer(get(frame, "displayHeight")) != Some(h)
        || get(frame, "rotation").as_f64().is_some_and(|r| r != 0.0)
        || get(frame, "flip").as_bool() == Some(true)
    {
        return Err("geometry");
    }
    let color = color(frame, cfg.color).ok_or("color")?;
    let options = obj(&[("rect", rect)]);
    let bytes =
        integer(call(frame, "allocationSize", &[&options]).map_err(|_| "allocation")?).filter(|n| *n > 0 && *n <= MAX_COPY_BYTES).ok_or("allocation")?;
    let array: js_sys::Function = get(&js_sys::global(), "Uint8Array").dyn_into().map_err(|_| "allocation")?;
    let raw: js_sys::Uint8Array =
        js_sys::Reflect::construct(&array, &js_sys::Array::of1(&bytes.into())).and_then(|v| v.dyn_into()).map_err(|_| "allocation")?;
    let promise = call(frame, "copyTo", &[raw.as_ref(), &options]).map_err(|_| "copy")?.dyn_into::<js_sys::Promise>().map_err(|_| "copy")?;
    let layout = js_sys::Array::from(&JsFuture::from(promise).await.map_err(|_| "copy")?);
    let count = if interleaved {
        2
    } else if alpha {
        4
    } else {
        3
    };
    if layout.length() != count {
        return Err("layout");
    }
    let (sx, sy) = chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let y = plane(&raw, &layout.get(0), w, h)?;
    let (u, v) = if interleaved {
        let uv = plane(&raw, &layout.get(1), cw.checked_mul(2).ok_or("allocation")?, ch)?;
        let pairs = uv.as_chunks::<2>().0;
        let (mut u, mut v) = (Vec::new(), Vec::new());
        u.try_reserve_exact(pairs.len()).map_err(|_| "memory")?;
        v.try_reserve_exact(pairs.len()).map_err(|_| "memory")?;
        u.extend(pairs.iter().map(|p| p[0]));
        v.extend(pairs.iter().map(|p| p[1]));
        (u, v)
    } else {
        (plane(&raw, &layout.get(1), cw, ch)?, plane(&raw, &layout.get(2), cw, ch)?)
    };
    let alpha = if alpha { Some(Arc::new(plane(&raw, &layout.get(3), w, h)?)) } else { None };
    Ok((
        VideoFrame {
            width: w,
            height: h,
            data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, alpha },
            color,
            par: cfg.par,
            pts: Tick::ZERO,
        },
        bytes,
    ))
}
