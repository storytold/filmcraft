//! Browser-owned decoded frames; shared compositor import +on-demand original CPU pixels.

use super::{Config, call, get, obj};
use eframe::wgpu;
use filmcraft_frame::{CpuTransfer, GpuFormat, GpuPixels, GpuSurface, PixelData, VideoFrame};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};
use wasm_bindgen::{JsCast, JsValue};

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static OWNERS: RefCell<HashMap<u64, Rc<Owner>>> = RefCell::new(HashMap::new());
    static RETAINED: Cell<u64> = const { Cell::new(0) };
    static REJECTED: Cell<u64> = const { Cell::new(0) };
    static CLOSED: Cell<u64> = const { Cell::new(0) };
    static MATERIALIZED: Cell<u64> = const { Cell::new(0) };
    static CANVAS: Cell<u64> = const { Cell::new(0) };
    static FAILED: Cell<u64> = const { Cell::new(0) };
    static IMPORTED: Cell<u64> = const { Cell::new(0) };
    static LIVE: Cell<usize> = const { Cell::new(0) };
}

struct Owner {
    frame: web_sys::VideoFrame,
    config: Config,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.frame.close();
        CLOSED.with(|n| n.set(n.get().saturating_add(1)));
        LIVE.with(|n| n.set(n.get().saturating_sub(1)));
    }
}

#[derive(Debug)]
struct Surface {
    id: u64,
    width: u32,
    height: u32,
}
impl Drop for Surface {
    fn drop(&mut self) {
        OWNERS.with(|owners| {
            owners.borrow_mut().remove(&self.id);
        });
    }
}

fn owner(id: u64) -> Result<Rc<Owner>, String> {
    OWNERS.with(|owners| owners.borrow().get(&id).cloned().ok_or_else(|| "Browser frame owner unavailable".into()))
}

impl GpuSurface for Surface {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn byte_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn prepare_cpu(&self, transfer: CpuTransfer) {
        let owner = match owner(self.id) {
            Ok(owner) => owner,
            Err(error) => {
                transfer.complete(Err(error));
                return;
            }
        };
        MATERIALIZED.with(|n| n.set(n.get().saturating_add(1)));
        wasm_bindgen_futures::spawn_local(async move {
            let result = match super::pixels::read(owner.frame.as_ref(), &owner.config).await {
                Ok((frame, _)) => Ok(frame.data),
                Err(_) => {
                    CANVAS.with(|n| n.set(n.get().saturating_add(1)));
                    super::to_rgba(owner.frame.as_ref()).map(|frame| frame.data).ok_or_else(|| "Browser CPU frame extraction failed".into())
                }
            };
            if result.is_err() {
                FAILED.with(|n| n.set(n.get().saturating_add(1)));
            }
            transfer.complete(result);
            crate::repaint();
        });
    }
}

pub(super) fn enable() {
    filmcraft_gpu::set_surface_importer(import);
    ENABLED.with(|enabled| enabled.set(true));
}

fn dimension(value: JsValue) -> Option<u32> {
    let value = value.as_f64()?;
    (value.is_finite() && value > 0.0 && value <= u32::MAX as f64 && value.fract() == 0.0).then_some(value as u32)
}

pub(super) fn retain(frame: &JsValue, config: &Config) -> Option<VideoFrame> {
    let result = retain_inner(frame, config);
    if result.is_some() {
        RETAINED.with(|n| n.set(n.get().saturating_add(1)));
    } else {
        REJECTED.with(|n| n.set(n.get().saturating_add(1)));
    }
    result
}

pub(super) fn stats() -> serde_json::Value {
    serde_json::json!({"enabled":ENABLED.with(Cell::get), "live":LIVE.with(Cell::get),
        "retained":RETAINED.with(Cell::get), "rejected":REJECTED.with(Cell::get), "closed":CLOSED.with(Cell::get),
        "cpuMaterializations":MATERIALIZED.with(Cell::get), "canvas":CANVAS.with(Cell::get), "failed":FAILED.with(Cell::get), "imports":IMPORTED.with(Cell::get)})
}

fn retain_inner(frame: &JsValue, config: &Config) -> Option<VideoFrame> {
    if !ENABLED.with(Cell::get) || config.rotation != 0 {
        return None;
    }
    if !matches!(get(frame, "format").as_string().as_deref(), Some("NV12" | "I420")) {
        return None;
    }
    let rect = get(frame, "visibleRect");
    let (width, height) = (dimension(get(&rect, "width"))?, dimension(get(&rect, "height"))?);
    // Coded/visible chroma clamps differ at cropped edges; keep original CPU reconstruction there.
    if get(&rect, "x").as_f64() != Some(0.0)
        || get(&rect, "y").as_f64() != Some(0.0)
        || dimension(get(frame, "codedWidth")) != Some(width)
        || dimension(get(frame, "codedHeight")) != Some(height)
        || dimension(get(frame, "displayWidth")) != Some(width)
        || dimension(get(frame, "displayHeight")) != Some(height)
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || get(frame, "rotation").as_f64().is_some_and(|r| r != 0.0)
        || get(frame, "flip").as_bool() == Some(true)
    {
        return None;
    }
    let color = get(frame, "colorSpace");
    if get(&color, "matrix").as_string().as_deref() != Some("bt709")
        || get(&color, "primaries").as_string().as_deref() != Some("bt709")
        || !matches!(get(&color, "transfer").as_string().as_deref(), Some("bt709" | "iec61966-2-1"))
        || get(&color, "fullRange").as_bool().is_none()
    {
        return None;
    }
    let byte_len = width.checked_mul(height)?.checked_mul(4)?;
    usize::try_from(byte_len).ok()?;
    // One cache's frame allowance across all sources; overflow uses the existing CPU path.
    if LIVE.with(Cell::get) >= super::CACHE_FRAMES {
        return None;
    }
    let browser = frame.clone().dyn_into::<web_sys::VideoFrame>().ok()?;
    let color = super::pixels::color(frame, config.color)?;
    let id = NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.checked_add(1)?);
        Some(id)
    })?;
    OWNERS.with(|owners| owners.borrow_mut().insert(id, Rc::new(Owner { frame: browser, config: config.clone() })));
    LIVE.with(|n| n.set(n.get().saturating_add(1)));
    Some(VideoFrame {
        width,
        height,
        data: PixelData::Gpu(GpuPixels::new(Arc::new(Surface { id, width, height }), GpuFormat::Rgba8)),
        color,
        par: config.par,
        pts: filmcraft_time::Tick::ZERO,
    })
}

fn import(surface: &dyn GpuSurface, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<filmcraft_gpu::ImportedSurface, String> {
    let surface = surface.as_any().downcast_ref::<Surface>().ok_or("Wrong browser surface")?;
    if device.as_webgpu().is_none() || queue.as_webgpu().is_none() {
        return Err("Browser surface requires WebGPU".into());
    }
    let owner = owner(surface.id)?;
    if surface.width > device.limits().max_texture_dimension_2d || surface.height > device.limits().max_texture_dimension_2d {
        return Err("Browser surface exceeds texture limits".into());
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Browser decoded frame"),
        size: wgpu::Extent3d { width: surface.width, height: surface.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    // wgpu's external-copy wrapper unwraps browser exceptions; catch them at this platform boundary.
    let source = obj(&[("source", JsValue::from(Clone::clone(&owner.frame)))]);
    let destination = obj(&[
        ("texture", JsValue::from(texture.as_webgpu().ok_or("Missing browser texture")?.clone())),
        ("colorSpace", "srgb".into()),
        ("premultipliedAlpha", false.into()),
    ]);
    let extent = obj(&[("width", surface.width.into()), ("height", surface.height.into()), ("depthOrArrayLayers", 1.into())]);
    call(queue.as_webgpu().ok_or("Missing browser queue")?.as_ref(), "copyExternalImageToTexture", &[&source, &destination, &extent]).map_err(|error| {
        FAILED.with(|n| n.set(n.get().saturating_add(1)));
        format!("Browser frame import failed: {error:?}")
    })?;
    IMPORTED.with(|n| n.set(n.get().saturating_add(1)));
    // Import cache keeps the owner; queued work also holds it through completion.
    let inflight = owner.clone();
    let promise = queue.as_webgpu().ok_or("Missing browser queue")?.on_submitted_work_done();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        drop(inflight);
    });
    Ok(filmcraft_gpu::ImportedSurface::Rgba { texture, keepalive: Box::new(owner) })
}
