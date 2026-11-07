//! The post-render pass against the CPU export's arithmetic at odd and tiny sizes: a 4:2:2
//! chroma sample of one pixel at the right edge, planes that start inside a word, placement with
//! a border and with a crop, 4:4:4 with alpha, and the 8-bit picture with the video limiter.
//! (Scaling, and every step at ordinary sizes, are covered end to end in `tests/export_pipeline.rs`.)

use crate::export_post::{PostConfig, PostPass};
use crate::intra_enc::ExportDevice;
use crate::{f16_to_f32, f32_to_f16};
use filmcraft_export::{Matrix, Planes444, limit_rgba8, rgba_to_yuv422_10, rgbf_to_yuv422_10, to_yuv444_10};
use filmcraft_render::Image;

/// A premultiplied picture with gradients, a transparent patch and out-of-range values, rounded
/// through half floats as the accumulator holds it.
fn picture(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    for (i, p) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        let a = if (i % 7) == 3 { 0.0 } else { (0.35 + 0.65 * ((x * 3.0 + y) % 11.0) / 10.0).min(1.0) };
        let rgb = [(x * 0.13 + 0.05) % 1.1, (y * 0.21 + 0.1) % 0.9, ((x + y) * 0.07) % 1.2];
        for (c, v) in rgb.into_iter().enumerate() {
            p[c] = f16_to_f32(f32_to_f16(v * a));
        }
        p[3] = f16_to_f32(f32_to_f16(a));
    }
    img
}

/// `pipeline::place` without resampling: `img` at `offset` in a `w` × `h` transparent frame.
fn place(img: &Image, w: usize, h: usize, (ox, oy): (i64, i64)) -> Image {
    let mut out = Image::new(w, h);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (sx, sy) = (x - ox, y - oy);
            if sx >= 0 && sy >= 0 && (sx as usize) < img.w && (sy as usize) < img.h {
                let s = (sy as usize * img.w + sx as usize) * 4;
                let d = (y as usize * w + x as usize) * 4;
                out.px[d..d + 4].copy_from_slice(&img.px[s..s + 4]);
            }
        }
    }
    out
}

/// The planes the CPU export hands its encoder for `cfg` (no scaling): Y, Cb, Cr, alpha.
fn cpu_planes(img: &Image, cfg: &PostConfig) -> Vec<u16> {
    let (w, h) = (cfg.width as usize, cfg.height as usize);
    let img = place(img, w, h, cfg.offset);
    let cw = if cfg.chroma_422 { w.div_ceil(2) } else { w };
    let (mut y, mut cb, mut cr, mut a) = (vec![0u16; w * h], vec![0u16; cw * h], vec![0u16; cw * h], vec![0u16; w * h]);
    let mut rgba = if cfg.alpha { img.to_rgba8() } else { img.over_black_rgba8() };
    let m = Matrix::new(cfg.kr, cfg.kb);
    if cfg.floats {
        // `srgb_floats`
        let encode = |v: f32| if v > 0.0 { filmcraft_color::linear_to_srgb(v.min(1.0)) } else { 0.0 };
        let rgb: Vec<f32> = img
            .px
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let k = match cfg.alpha {
                    false => 1.0,
                    true if p[3] > 0.0 => 1.0 / p[3].min(1.0),
                    true => 0.0,
                };
                [encode(p[0] * k), encode(p[1] * k), encode(p[2] * k)]
            })
            .collect();
        if cfg.chroma_422 {
            rgbf_to_yuv422_10(&rgb, w, h, m, &mut y, &mut cb, &mut cr);
        } else {
            to_yuv444_10(&rgba, Some(&rgb), w, h, m, Planes444 { y: &mut y, cb: &mut cb, cr: &mut cr, alpha: None });
        }
    } else {
        if let Some((lo, hi)) = cfg.limiter {
            limit_rgba8(&mut rgba, lo, hi);
        }
        assert!(cfg.chroma_422, "the 8-bit picture goes to 4:2:2 encoders");
        rgba_to_yuv422_10(&rgba, w, h, &mut y, &mut cb, &mut cr);
    }
    for (d, p) in a.iter_mut().zip(rgba.as_chunks::<4>().0) {
        *d = ((u32::from(p[3]) * 1023 + 127) / 255) as u16;
    }
    let mut out = [y, cb, cr].concat();
    if cfg.alpha {
        out.extend(a);
    }
    out
}

/// What the pass writes for `cfg` from `img` (the rendered picture).
fn gpu_planes(dev: &ExportDevice, img: &Image, cfg: &PostConfig) -> Vec<u16> {
    let (device, queue) = (&dev.device, &dev.queue);
    let size = wgpu::Extent3d { width: img.w as u32, height: img.h as u32, depth_or_array_layers: 1 };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-accum"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let halves: Vec<u8> = img.px.iter().flat_map(|&v| f32_to_f16(v).to_le_bytes()).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &halves,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.w as u32 * 8), rows_per_image: Some(img.h as u32) },
        size,
    );
    let view = tex.create_view(&Default::default());
    let pass = PostPass::new(device, queue);
    let planes = pass.run(device, queue, &view, cfg).expect("the pass runs this configuration");
    let bytes = planes.buffer.size();
    let rb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(&planes.buffer, 0, &rb, 0, bytes);
    queue.submit([enc.finish()]);
    rb.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("mapped"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let data = rb.slice(..).get_mapped_range().expect("range").to_vec();
    data.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()
}

#[test]
fn the_post_pass_matches_the_cpu_export_at_odd_sizes() {
    let Some(dev) = ExportDevice::shared() else {
        eprintln!("SKIPPED (export post pass): no adapter");
        return;
    };
    let base = PostConfig {
        rendered: (17, 13),
        width: 17,
        height: 13,
        content: None,
        offset: (0, 0),
        limiter: None,
        hdr: false,
        floats: true,
        alpha: false,
        kr: 0.2126,
        kb: 0.0722,
        chroma_422: true,
        bit_depth: 10,
        overlays: false,
    };
    let cases = [
        ("17x13 4:2:2", base.clone()),
        ("17x13 4:4:4 alpha", PostConfig { alpha: true, chroma_422: false, ..base.clone() }),
        ("17x13 8-bit alpha limiter", PostConfig { floats: false, alpha: true, limiter: Some((10.0, 80.0)), ..base.clone() }),
        ("17x13 8-bit over black", PostConfig { floats: false, ..base.clone() }),
        ("1x1 4:2:2", PostConfig { rendered: (1, 1), width: 1, height: 1, ..base.clone() }),
        ("1x1 4:4:4 alpha", PostConfig { rendered: (1, 1), width: 1, height: 1, alpha: true, chroma_422: false, ..base.clone() }),
        ("3x1 8-bit", PostConfig { rendered: (3, 1), width: 3, height: 1, floats: false, ..base.clone() }),
        ("19x15 border", PostConfig { width: 19, height: 15, offset: (1, 1), ..base.clone() }),
        ("15x13 crop BT.2020", PostConfig { width: 15, offset: (-2, 0), kr: 0.2627, kb: 0.0593, alpha: true, ..base.clone() }),
    ];
    for (what, cfg) in cases {
        let img = picture(cfg.rendered.0 as usize, cfg.rendered.1 as usize);
        let want = cpu_planes(&img, &cfg);
        let got = gpu_planes(&dev, &img, &cfg);
        assert!(got.len() >= want.len(), "{what}: {} samples, want {}", got.len(), want.len());
        let off: Vec<(usize, u16, u16)> = want.iter().zip(&got).enumerate().filter(|(_, (a, b))| a != b).map(|(i, (a, b))| (i, *a, *b)).collect();
        // (the GPU's `pow` may round the sRGB curve a code apart where the CPU's sits on a half)
        assert!(off.iter().all(|&(_, a, b)| a.abs_diff(b) <= 1), "{what}: samples more than a code off: {off:?}");
        assert!(off.len() * 50 <= want.len().max(50), "{what}: {} of {} samples a code off: {off:?}", off.len(), want.len());
        eprintln!("{what}: {} of {} samples a code off", off.len(), want.len());
        assert!(got[want.len()..].iter().all(|&v| v == 0), "{what}: the padding stays zero");
    }
}
