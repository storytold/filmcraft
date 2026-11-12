use super::*;
use filmcraft_frame::Chroma;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_render::Blend;
use filmcraft_render::plan::execute_cpu;

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = crate::adapter::native_instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&crate::device_descriptor(&adapter))).ok()
}

/// A 10-bit frame draws the same whether its planes go up as 16-bit codes (`R16Unorm`, on a
/// device with normalised 16-bit textures) or as half floats, uploaded by a worker's
/// [`Uploader`] beforehand, converted beforehand, or neither; and the uploader's textures come
/// back from the pool for the next frame of that size.
#[test]
fn yuv16_frames_draw_the_same_however_they_reach_the_gpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    eprintln!("normalised 16-bit textures: {}", up.norm16());
    let (w, h) = (64u32, 8u32);
    let y: Vec<u16> = (0..w * h).map(|i| (64 + (i % w) * 876 / (w - 1)) as u16).collect();
    let cb: Vec<u16> = (0..w * h).map(|i| (200 + (i / w) * 80) as u16).collect();
    let cr: Vec<u16> = (0..w * h).map(|i| (800 - (i % w) * 10) as u16).collect();
    // (a fresh frame each time: the compositor keeps the textures of a frame it has uploaded)
    let plan = || {
        let data = PixelData::Yuv16 { planes: [Arc::new(y.clone()), Arc::new(cb.clone()), Arc::new(cr.clone())], chroma: Chroma::C444, bits: 10, alpha: None };
        let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
        FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] }
    };
    let reference = execute_cpu(&plan()).over_black_rgba8();
    for (name, uploader, prepared) in [("unprepared", None, false), ("uploaded by a worker", Some(&up), true), ("converted beforehand", None, true)] {
        let plan = plan();
        let prep = prepared.then(|| prepare(&plan, uploader));
        if let Some(p) = &prep {
            let layer = p.layers[0].as_ref().expect("a 16-bit frame is prepared");
            assert_eq!(layer.uploaded(), uploader.is_some(), "{name}");
            assert_eq!(layer.raw16, uploader.is_some_and(Uploader::norm16), "{name}");
        }
        c.composite_prepared(&plan, prep.as_ref());
        let (_, _, gpu) = c.read_output().expect("readback");
        let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(error <= 2, "{name}: GPU differs from the CPU by up to {error} codes");
    }
    // the pool: a frame's three planes return when nothing holds them any more, and the next
    // frame of that size takes them
    let pooled0 = up.pooled();
    drop(prepare(&plan(), Some(&up)));
    assert_eq!(up.pooled(), pooled0 + 3);
    let kept = prepare(&plan(), Some(&up));
    assert_eq!(up.pooled(), pooled0);
    drop(kept);
    assert_eq!(up.pooled(), pooled0 + 3);
}

/// Planes go up through a staging buffer whose rows are padded to the copy alignment: a 10-bit
/// 4:2:2 frame whose rows are not a multiple of it draws as the CPU does, and a frame whose
/// planes are shorter than its size (a hostile file) uploads without panicking.
#[test]
fn padded_and_short_planes_upload() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    // 1000 * 2 bytes per row: 7.8 copy alignments; chroma rows 500 * 2
    let (w, h) = (1000u32, 6u32);
    let (cw, ch) = (w / 2, h);
    let y: Vec<u16> = (0..w * h).map(|i| (64 + (i % w) * 876 / (w - 1)) as u16).collect();
    let cb: Vec<u16> = (0..cw * ch).map(|i| (200 + (i / cw) * 80) as u16).collect();
    let cr: Vec<u16> = (0..cw * ch).map(|i| (800 - (i % cw)) as u16).collect();
    let frame = |y: Vec<u16>, cb: Vec<u16>, cr: Vec<u16>| {
        let data = PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C422, bits: 10, alpha: None };
        let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
        FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] }
    };
    let plan = frame(y.clone(), cb.clone(), cr.clone());
    let reference = execute_cpu(&plan).over_black_rgba8();
    let prep = prepare(&plan, Some(&up));
    c.composite_prepared(&plan, Some(&prep));
    let (_, _, gpu) = c.read_output().expect("readback");
    let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(error <= 2, "GPU differs from the CPU by up to {error} codes");
    // short planes: the rows that exist go up, the rest stay zero, also in staging buffers
    // that held the full frame before (mapped again by now: the readback waited for the GPU)
    let short = frame(y[..y.len() / 2].to_vec(), cb[..7].to_vec(), Vec::new());
    let staging = up.created().1;
    let prep = prepare(&short, Some(&up));
    assert_eq!(up.created().1, staging, "the short frame reuses the full frame's staging buffers");
    assert!(prep.layers[0].as_ref().is_some_and(Prepared::uploaded));
    c.composite_prepared(&short, Some(&prep));
    let (_, _, gpu) = c.read_output().expect("readback");
    let zero_padded =
        frame([&y[..y.len() / 2], &vec![0; y.len() - y.len() / 2][..]].concat(), [&cb[..7], &vec![0; cb.len() - 7][..]].concat(), vec![0; cr.len()]);
    let reference = execute_cpu(&zero_padded).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    // (zero is below the 10-bit video range, which the CPU and the GPU clip a few codes apart;
    // rows left over from the full frame differ by the whole range)
    assert!(error <= 4, "a short plane differs from its zero-padded frame by up to {error} codes");
}

/// A preroll that Stop interrupts leaves no "GPU is awake" behind for the next preroll, and its
/// loop length is not taken as the reference.
#[test]
fn a_cancelled_preroll_leaves_the_gpu_to_be_woken_again() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    c.wake_warm = true;
    let reference = c.wake_ref;
    c.wake_cancelled();
    assert!(!c.wake_warm());
    assert_eq!(c.wake_ref, reference);
}

/// A frame worker records its uploads and submits nothing; the compositor submits them before
/// its own commands, so the frame is complete when drawn. (wgpu fails a surface reconfigure
/// when another thread submits during it: the GPU compositor went off on a window resize.)
#[test]
fn uploads_from_a_worker_wait_for_the_compositors_submit() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    let worker = {
        let up = up.clone();
        std::thread::spawn(move || {
            let (w, h) = (64u32, 8u32);
            let y: Vec<u16> = (0..w * h).map(|i| (64 + (i % w) * 876 / (w - 1)) as u16).collect();
            let cb: Vec<u16> = (0..w * h).map(|i| (300 + (i / w) * 40) as u16).collect();
            let cr: Vec<u16> = (0..w * h).map(|i| (700 - (i % w)) as u16).collect();
            let data = PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 10, alpha: None };
            let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
            let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
            let prep = prepare(&plan, Some(&up));
            (plan, prep)
        })
    };
    let (plan, prep) = worker.join().expect("worker");
    assert!(prep.layers[0].as_ref().is_some_and(Prepared::uploaded));
    assert_eq!(up.pending_copies(), 1, "the worker recorded its copies and submitted nothing");
    c.composite_prepared(&plan, Some(&prep));
    assert_eq!(up.pending_copies(), 0, "the composite submitted the copies first");
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&plan).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(error <= 2, "GPU differs from the CPU by up to {error} codes");
    // a flush with nothing pending submits nothing
    c.flush_uploads();
    assert_eq!(up.pending_copies(), 0);
}

/// A pass submits the recorded uploads within its budget, and a composite submits the ones it
/// draws whatever the budget, in recording order; later uploads wait for a pass with room.
#[test]
fn uploads_are_submitted_as_needed_or_within_the_budget() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    let (w, h) = (64u32, 8u32);
    let plan_for = |level: u16| {
        let plane = |n: usize| Arc::new(vec![level; n]);
        let n = (w * h) as usize;
        let data = PixelData::Yuv16 { planes: [plane(n), plane(n), plane(n)], chroma: Chroma::C444, bits: 10, alpha: None };
        let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
        FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] }
    };
    let (a, b, d) = (plan_for(300), plan_for(600), plan_for(900));
    let (pa, pb, pd) = (prepare(&a, Some(&up)), prepare(&b, Some(&up)), prepare(&d, Some(&up)));
    assert_eq!(up.pending_copies(), 3);
    // no budget: nothing goes
    c.flush_uploads_within(0);
    assert_eq!(up.pending_copies(), 3);
    // a budget for one frame's planes (three 64 x 8 planes of two bytes, rows padded to 256)
    c.flush_uploads_within(3 * 256 * 8);
    assert_eq!(up.pending_copies(), 2, "one upload fit the budget");
    // the budget is the pass's: after a flush that spent it, a composite submits only its needs;
    // drawing the third frame submits the third, whatever the budget, and the second waits
    c.set_upload_budget(3 * 256 * 8);
    c.composite_prepared(&d, Some(&pd));
    assert_eq!(up.pending_copies(), 1, "the composite submitted the upload it needed and no other");
    let (e, f) = (plan_for(200), plan_for(400));
    let (_pe, pf) = (prepare(&e, Some(&up)), prepare(&f, Some(&up)));
    c.flush_uploads_within(3 * 256 * 8);
    assert_eq!(up.pending_copies(), 2, "the pass's budget took the oldest of the three");
    let g = plan_for(100);
    let pg = prepare(&g, Some(&up));
    c.composite_prepared(&f, Some(&pf));
    assert_eq!(up.pending_copies(), 2, "the composite took what it drew and left the others for the next pass");
    c.flush_uploads();
    assert_eq!(up.pending_copies(), 1, "the next pass flushed one more, its budget's worth");
    c.composite_prepared(&g, Some(&pg));
    assert_eq!(up.pending_copies(), 0, "the composite took the one it drew");
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&g).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(error <= 2, "the frame drawn right after its upload differs from the CPU by up to {error} codes");
    // the earlier frames draw right as well
    for (plan, prep) in [(&a, &pa), (&b, &pb)] {
        c.composite_prepared(plan, Some(prep));
        let (_, _, gpu) = c.read_output().expect("readback");
        let reference = execute_cpu(plan).over_black_rgba8();
        let error = reference.iter().zip(&gpu).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        assert!(error <= 2, "an earlier frame differs from the CPU by up to {error} codes");
    }
}

/// Measures how fast the GPU copies a 4K 16-bit plane from a mapped staging buffer into a
/// texture (run by hand: `cargo test -p filmcraft-gpu copy_throughput_probe -- --ignored --no-capture`).
#[test]
#[ignore = "a measurement, not a check"]
fn copy_throughput_probe() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (3840u32, 2160u32);
    let size = u64::from(w) * 2 * u64::from(h);
    let tex = dev.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (label, n_bufs) in [("one buffer, 8 copies", 1usize), ("8 buffers, 8 copies", 8usize)] {
        let bufs: Vec<wgpu::Buffer> = (0..n_bufs)
            .map(|_| {
                let b = dev.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size,
                    usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: true,
                });
                b.slice(..).get_mapped_range_mut().expect("map").slice(..).copy_from_slice(&vec![0x40u8; size as usize]);
                b.unmap();
                b
            })
            .collect();
        for round in 0..3 {
            let mut enc = dev.create_command_encoder(&Default::default());
            for i in 0..8 {
                enc.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &bufs[i % n_bufs],
                        layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 2), rows_per_image: Some(h) },
                    },
                    wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            }
            dev.poll(wgpu::PollType::wait_indefinitely()).expect("idle");
            let t0 = std::time::Instant::now();
            q.submit([enc.finish()]);
            dev.poll(wgpu::PollType::wait_indefinitely()).expect("done");
            let dt = t0.elapsed().as_secs_f64();
            let mb = 8.0 * size as f64 / 1e6;
            eprintln!("{label}, round {round}: {mb:.0} MB in {:.1} ms = {:.1} GB/s", dt * 1e3, mb / 1e3 / dt);
        }
    }
}

/// A pass's budget goes to the frames wanted soonest, whatever their order of recording, and a
/// frame dropped before its upload was submitted takes the upload with it (its textures go back
/// to the pool for the next frame, which then draws right).
#[test]
fn uploads_go_soonest_first_and_a_dropped_frame_cancels_its_upload() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    let (w, h) = (64u32, 8u32);
    let plan_for = |level: u16| {
        let plane = |n: usize| Arc::new(vec![level; n]);
        let n = (w * h) as usize;
        let data = PixelData::Yuv16 { planes: [plane(n), plane(n), plane(n)], chroma: Chroma::C444, bits: 10, alpha: None };
        let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
        FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] }
    };
    let one_frame = 3 * 256 * 8;
    // recorded as the frames two, one and zero ahead of the playhead
    let (far, near, now) = (plan_for(300), plan_for(600), plan_for(900));
    let p_far = prepare_ahead(&far, Some(&up), 2);
    let p_near = prepare_ahead(&near, Some(&up), 1);
    let p_now = prepare_ahead(&now, Some(&up), 0);
    assert_eq!(up.pending_copies(), 3);
    c.flush_uploads_within(one_frame);
    assert_eq!(up.pending_copies(), 2, "the budget took one upload");
    c.set_upload_budget(one_frame);
    c.composite_prepared(&now, Some(&p_now));
    assert_eq!(up.pending_copies(), 2, "the flush took the frame wanted now, so its composite needed no upload");
    // the far frame is dropped before its upload went: the upload goes with it
    let before = up.created();
    drop(p_far);
    assert_eq!(up.pending_copies(), 1, "the dropped frame's upload was cancelled");
    // (the readback submits whatever is still pending)
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&now).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(error <= 2, "the frame wanted now differs from the CPU by up to {error} codes");
    // ... and its textures serve the next frame of that size, which draws right
    let next = plan_for(150);
    let p_next = prepare_ahead(&next, Some(&up), 0);
    assert_eq!(up.created(), before, "the next frame reused the dropped frame's textures and staging");
    c.composite_prepared(&next, Some(&p_next));
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&next).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(error <= 2, "the frame in the reused textures differs from the CPU by up to {error} codes");
    c.composite_prepared(&near, Some(&p_near));
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&near).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    assert!(error <= 2, "the near frame differs from the CPU by up to {error} codes");
}

/// Staging buffers come back after their copies ran: the next frame of the same size reuses
/// them, and none is created for it. (A buffer per plane per frame had the device's allocator
/// create and destroy memory blocks through every play start, with the UI thread waiting.)
#[test]
fn staging_buffers_are_reused_after_the_submit() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let up = c.uploader();
    let (w, h) = (64u32, 8u32);
    let plan_for = |level: u16| {
        let plane = |n: usize| Arc::new(vec![level; n]);
        let n = (w * h) as usize;
        let data = PixelData::Yuv16 { planes: [plane(n), plane(n), plane(n)], chroma: Chroma::C444, bits: 10, alpha: None };
        let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
        FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] }
    };
    let settle = |dev: &wgpu::Device| {
        // the maps complete once the device has run the copies
        dev.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) }).expect("poll");
    };
    let first = plan_for(512);
    let prep = prepare(&first, Some(&up));
    assert_eq!(up.created().1, 3, "one staging buffer per plane");
    assert_eq!(up.free_staging(), 0);
    c.composite_prepared(&first, Some(&prep));
    settle(&dev);
    assert_eq!(up.free_staging(), 3, "the submitted buffers are mapped again");

    let second = plan_for(800);
    let prep = prepare(&second, Some(&up));
    assert_eq!(up.created().1, 3, "the second frame created no staging buffer");
    assert_eq!(up.free_staging(), 0, "it took the three mapped ones");
    c.composite_prepared(&second, Some(&prep));
    let (_, _, gpu) = c.read_output().expect("readback");
    let reference = execute_cpu(&second).over_black_rgba8();
    let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(error <= 2, "a frame through reused staging differs from the CPU by up to {error} codes");
    settle(&dev);
    assert_eq!(up.free_staging(), 3, "and they came back again");
}

/// Warming up makes the render targets of the sequence's size and draws each kind of frame
/// through the pipelines; a degenerate size is ignored, and frames still draw right afterwards.
#[test]
fn warm_up_makes_the_targets_and_draws_nothing_wrong() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    c.warm_up(0, 7);
    assert!(c.read_output().is_none(), "no targets without a size");
    c.warm_up(64, 8);
    let (w, h, _) = c.read_output().expect("targets of the warmed-up size");
    assert_eq!((w, h), (64, 8));
    // (4:4:4, where the CPU and the GPU agree to the code; 4:2:0 chroma is interpolated differently)
    let (w, h) = (64u32, 8u32);
    let y: Vec<u16> = (0..w * h).map(|i| (64 + (i % w) * 876 / (w - 1)) as u16).collect();
    let cb: Vec<u16> = (0..w * h).map(|i| (200 + (i / w) * 80) as u16).collect();
    let cr: Vec<u16> = (0..w * h).map(|i| (800 - (i % w) * 10) as u16).collect();
    let data = PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 10, alpha: None };
    let frame = Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() });
    let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
    let reference = execute_cpu(&plan).over_black_rgba8();
    c.composite(&plan);
    let (_, _, gpu) = c.read_output().expect("readback");
    let error = reference.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(error <= 2, "GPU differs from the CPU by up to {error} codes after warming up");
}

#[test]
fn yuv_code_range_uniforms() {
    let mut mismatches = Vec::new();
    for bits in [8, 10, 12, 16] {
        let levels = (1u32 << bits) as f32;
        for range in [Range::Full, Range::Limited] {
            let src = SrcInfo {
                kind: 2,
                code_scale: if bits == 8 { 255.0 } else { levels },
                code_levels: levels,
                alpha: 0.0,
                chroma: (1, 1),
                size: (1, 1),
                color: filmcraft_color::ColorInfo { range, ..filmcraft_color::ColorInfo::REC709 },
                input: None,
            };
            let u = GpuCompositor::uniforms(&src, &Affine::IDENTITY, 1.0, Blend::Normal, (1, 1));
            // Full: 0..2^bits-1, neutral 2^(bits-1). Limited: BT.709 8-bit codes * 2^(bits-8).
            let expected = match range {
                Range::Full => [0.0, levels - 1.0, levels / 2.0, levels - 1.0],
                Range::Limited => [16.0, 219.0, 128.0, 224.0].map(|v| v * (1u32 << (bits - 8)) as f32),
            };
            if u[16..20] != expected {
                mismatches.push(format!("{bits}-bit {range:?}: {:?} != {expected:?}", &u[16..20]));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn yuv_code_range_gpu_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let mut prepared_compositor = GpuCompositor::new(&dev, &q);
    for bits in [8, 10, 12, 16] {
        let scale = 1u32 << (bits - 8);
        for range in [Range::Full, Range::Limited] {
            // Constant chroma isolates range normalization from CPU/GPU chroma reconstruction.
            for (u, v) in [(128, 128), (64, 192), (192, 64)] {
                let y = (0..=255).map(|code| (code * scale) as u16).collect::<Vec<_>>();
                let planes = [Arc::new(y), Arc::new(vec![(u * scale) as u16; 256]), Arc::new(vec![(v * scale) as u16; 256])];
                let data = if bits == 8 {
                    PixelData::Yuv8 { planes: planes.map(|p| Arc::new(p.iter().map(|v| *v as u8).collect())), chroma: Chroma::C444, alpha: None }
                } else {
                    PixelData::Yuv16 { planes, chroma: Chroma::C444, bits, alpha: None }
                };
                let frame = Arc::new(VideoFrame {
                    width: 256,
                    height: 1,
                    data,
                    color: filmcraft_color::ColorInfo { range, ..filmcraft_color::ColorInfo::REC709 },
                    par: (1, 1),
                    pts: Default::default(),
                });
                let cpu = frame.to_rgba8();
                let plan = FramePlan::Layers { width: 256, height: 1, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
                c.composite(&plan);
                let (w, h, gpu) = c.read_output().expect("readback");
                assert_eq!((w, h, gpu.len()), (256, 1, cpu.len()));
                let error = cpu.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().expect("pixels");
                // One output-code tolerance: CPU f32 vs GPU f16 accumulator quantization.
                assert!(error <= 1, "{bits}-bit {range:?} U={u} V={v}: max error {error}");
                if u == 128 && v == 128 {
                    assert!(gpu.as_chunks::<4>().0.iter().all(|p| p[0] == p[1] && p[1] == p[2]), "neutral tint: {bits}-bit {range:?}");
                }
                let prepared = prepare(&plan, None);
                prepared_compositor.composite_prepared(&plan, Some(&prepared));
                assert_eq!(prepared_compositor.read_output().expect("prepared readback").2, gpu, "prepared: {bits}-bit {range:?}");
                eprintln!("range gate: {bits}-bit {range:?} U={u} V={v}, max error={error}, prepared=inline");
            }
        }
    }
}

fn yuv_frame(w: u32, h: u32) -> Arc<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + ((i % w) * 219 / w)) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (64 + (i / cw) * 128 / ch) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (200 - (i % cw) * 100 / cw) as u8).collect();
    Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    })
}

#[test]
fn gpu_matches_cpu_plan() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::OceanSunset),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    )
    .unwrap();
    let rgba = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    let (w, h) = (320usize, 180usize);
    let plan = FramePlan::Layers {
        width: w,
        height: h,
        layers: vec![
            PlanLayer { frame: yuv_frame(640, 360), matrix: Affine::scale(0.5, 0.5), opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
            PlanLayer {
                frame: rgba,
                matrix: Affine::motion(Vec2::new(200.0, 100.0), Vec2::new(0.4, 0.4), 12.0, Vec2::new(160.0, 90.0)),
                opacity: 0.7,
                blend: Blend::Normal,
                fx: None,
                input: None,
            },
        ],
    };
    let cpu = execute_cpu(&plan).over_black_rgba8();
    c.composite(&plan);
    let (gw, gh, gpu) = c.read_output().expect("readback");
    assert_eq!((gw as usize, gh as usize), (w, h));
    // compare away from antialiased edges: mean abs error and 99th percentile
    let mut diffs: Vec<u32> =
        cpu.chunks(4).zip(gpu.chunks(4)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0)).collect();
    diffs.sort_unstable();
    let p99 = diffs[diffs.len() * 99 / 100];
    let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
    assert!(p99 <= 6 && mean < 1.5, "p99 {p99}, mean {mean}");
    // cache: compositing the same plan again uploads nothing
    let before = c.uploaded_bytes;
    c.composite(&plan);
    assert_eq!(c.uploaded_bytes, before);
}

#[test]
fn half_float_conversion() {
    for v in [0.0f32, 1.0, 0.5, 0.123, 65504.0, -2.0, 1e-5] {
        let h = f32_to_f16(v);
        // decode
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = ((h >> 10) & 0x1f) as i32;
        let m = (h & 0x3ff) as f32;
        let d = if e == 0 { s * m * 2f32.powi(-24) } else { s * (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        assert!((d - v).abs() <= v.abs() * 1e-3 + 1e-6, "{v} → {d}");
    }
}

#[test]
fn gpu_push_matches_cpu_with_alpha_and_surrounding_blends() {
    use filmcraft_project::ParamValue;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (128usize, 72usize);
    let layer = |side: usize| {
        let px = (0..w * h)
            .flat_map(|i| {
                let u = (i % w % 2) as f32;
                let v = (i / w % 2) as f32;
                let a = if side == 0 { (u + v) / 2.0 } else { 1.0 - (u + v) / 2.0 };
                [u * a, v * a, 0.5 * a, a]
            })
            .collect();
        PlanLayer::new(Arc::new(VideoFrame::rgba_f32(w as u32, h as u32, px)), Affine::IDENTITY, 0.7, Blend::Normal)
    };
    let mut worst = (0, 0.0f64);
    for direction in 0..4 {
        for blur in [0.0, 50.0, 100.0] {
            for progress in [0.0, 0.25, 0.37, 0.5, 0.75, 1.0] {
                let mut effect = filmcraft_project::find_effect("push").unwrap().instance();
                effect.param_mut("direction").unwrap().value = ParamValue::Choice(direction);
                effect.param_mut("motion_blur").unwrap().value = ParamValue::Float(blur);
                let mut above = layer(1);
                above.blend = Blend::Multiply;
                let plan = FramePlan::Composite {
                    width: w,
                    height: h,
                    steps: vec![
                        PlanStep::Layers(vec![layer(1)]),
                        PlanStep::Transition { inputs: [vec![layer(0)], vec![layer(1)]], effect, progress, scale: 1.0 },
                        PlanStep::Layers(vec![above]),
                    ],
                };
                let cpu = execute_cpu(&plan).over_black_rgba8();
                c.composite_prepared(&plan, Some(&prepare(&plan, None)));
                let (_, _, gpu) = c.read_output().expect("readback");
                let mut diffs: Vec<u32> =
                    cpu.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0).map(|(a, b)| (0..3).map(|k| a[k].abs_diff(b[k]) as u32).max().unwrap()).collect();
                diffs.sort_unstable();
                let p99 = diffs[diffs.len() * 99 / 100];
                let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
                worst = (worst.0.max(p99), worst.1.max(mean));
                // Existing compositor acceptance band; moving edges included.
                assert!(p99 <= 6 && mean < 1.5, "direction {direction}, blur {blur}, progress {progress}: p99 {p99}, mean {mean}");
            }
        }
    }
    assert_eq!(c.gpu_transitions, 4 * 3 * 6);
    assert_eq!(c.cpu_transitions, 0);
    eprintln!("Push 72 cases: worst p99 {}, mean {}", worst.0, worst.1);
    let fallback = FramePlan::Composite {
        width: w,
        height: h,
        steps: vec![PlanStep::Transition {
            inputs: [vec![layer(0)], vec![layer(1)]],
            effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
            progress: 0.37,
            scale: 1.0,
        }],
    };
    c.composite(&fallback);
    let (_, _, got) = c.read_output().expect("fallback readback");
    let expected = execute_cpu(&fallback).over_black_rgba8();
    assert!(got.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1));
    assert_eq!(c.cpu_transitions, 1);
    assert_eq!(c.gpu_transitions, 4 * 3 * 6);

    // Reuse both input targets across ordered transitions; preparation advances past intervening layers.
    let transition = |progress| PlanStep::Transition {
        inputs: [vec![layer(0)], vec![layer(1)]],
        effect: filmcraft_project::find_effect("push").unwrap().instance(),
        progress,
        scale: 1.0,
    };
    let mut middle = layer(0);
    middle.blend = Blend::Screen;
    let stacked = FramePlan::Composite {
        width: w,
        height: h,
        steps: vec![PlanStep::Layers(vec![layer(1)]), transition(0.25), PlanStep::Layers(vec![middle]), transition(0.75)],
    };
    c.composite_prepared(&stacked, Some(&prepare(&stacked, None)));
    let (_, _, gpu) = c.read_output().expect("stacked readback");
    let cpu = execute_cpu(&stacked).over_black_rgba8();
    // One byte level tolerates half-float resolve rounding.
    assert!(gpu.iter().zip(cpu).all(|(a, b)| a.abs_diff(b) <= 1), "stacked transitions exceed one byte level");
    assert_eq!(c.gpu_transitions, 4 * 3 * 6 + 2);
    assert_eq!(c.cpu_transitions, 1);
}

#[test]
fn prepared_upload_matches_inline_conversion() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (64u32, 36u32);
    let yuv16 = Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv16 {
            planes: [
                Arc::new((0..w * h).map(|i| (64 + (i * 7) % 876) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (64 + (i * 13) % 896) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (960 - (i * 5) % 896) as u16).collect()),
            ],
            chroma: Chroma::C422,
            bits: 10,
            alpha: None,
        },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    });
    let f32_layer: Vec<f32> = (0..w * h * 4).map(|i| ((i * 37) % 1000) as f32 / 1000.0 * if i % 4 == 3 { 1.0 } else { 0.6 }).collect();
    let rgbaf = Arc::new(VideoFrame::rgba_f32(w, h, f32_layer.clone()));
    let layers = FramePlan::Layers {
        width: w as usize,
        height: h as usize,
        layers: vec![
            PlanLayer { frame: yuv16, matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
            PlanLayer { frame: rgbaf, matrix: Affine::scale(0.5, 0.5), opacity: 0.8, blend: Blend::Normal, fx: None, input: None },
        ],
    };
    let image = FramePlan::Image(filmcraft_render::Image { w: w as usize, h: h as usize, px: f32_layer });
    for plan in [layers, image] {
        let mut a = GpuCompositor::new(&dev, &q);
        a.composite(&plan);
        let inline = a.read_output().expect("readback");
        let mut b = GpuCompositor::new(&dev, &q);
        let up = b.uploader();
        // Uploaded by a worker, the planes have the formats of the inline upload: the same
        // picture exactly. Converted without an uploader, 16-bit planes are half floats, while
        // the inline upload takes normalised 16-bit textures where the device has them: the
        // picture is then within one code (seen on Apple GPUs), and exact where it has not.
        for (name, uploader, codes) in [("uploaded by a worker", Some(&up), 0), ("converted beforehand", None, u8::from(up.norm16()))] {
            let prep = prepare(&plan, uploader);
            assert!(uploader.is_some() || prep.bytes() > 0, "{name}");
            b.composite_prepared(&plan, Some(&prep));
            let prepared = b.read_output().expect("readback");
            assert_eq!((inline.0, inline.1), (prepared.0, prepared.1), "{name}");
            let error = inline.2.iter().zip(&prepared.2).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(error <= codes, "{name}: prepared upload differs by up to {error} codes");
        }
    }
}

/// The upload cache is keyed by pixel-buffer address, so it must keep the buffer alive: otherwise
/// a new frame allocated at a freed frame's address is drawn with the stale texture (seen as
/// whole frames from a previous composite in `crates/golden` GPU parity).
#[test]
fn upload_cache_keeps_buffers_alive() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let px = Arc::new(vec![200u8; 16 * 8 * 4]);
    let frame = Arc::new(VideoFrame { width: 16, height: 8, data: PixelData::Rgba8(px.clone()), ..(*yuv_frame(16, 8)).clone() });
    let plan = FramePlan::Layers {
        width: 16,
        height: 8,
        layers: vec![PlanLayer { frame, matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }],
    };
    c.composite(&plan);
    drop(plan);
    assert!(Arc::strong_count(&px) > 1, "cached upload must own its pixel buffer");
}

/// The WGSL tetrahedral LUT matches `Lut3d::apply` on the CPU.
#[test]
fn gpu_lut_matches_cpu_tetrahedral() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = GpuLut::new(&dev, &q);
    for size in [2usize, 17, 33, 65] {
        let mut lut = filmcraft_color::Lut3d::from_fn(size, |c| {
            [(c[0] * c[1] + c[2] * c[2]).sin(), c[1].powf(0.45) * (1.0 - 0.3 * c[0]), (c[0] - c[2]).abs() + 0.1 * c[1]]
        });
        if size == 17 {
            lut.domain_min = [-0.1, 0.0, 0.0];
            lut.domain_max = [1.2, 1.0, 2.0];
        }
        let mut s = 99u64;
        let mut px = Vec::new();
        for _ in 0..70_000 {
            for k in 0..4 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                px.push(if k == 3 { 0.5 } else { (s % 10_001) as f32 / 10_000.0 * 1.2 - 0.1 });
            }
        }
        let gpu = g.apply(&lut, &px).expect("gpu lut");
        let mut worst = 0f32;
        for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
            let c = lut.apply([p[0], p[1], p[2]]);
            for k in 0..3 {
                worst = worst.max((c[k] - gpu[i * 4 + k]).abs());
            }
            assert_eq!(gpu[i * 4 + 3], 0.5);
        }
        eprintln!("LUT {size}³: max |cpu − gpu| = {worst:e}");
        assert!(worst < 1e-4, "size {size}: max |cpu - gpu| = {worst}");
    }
}

fn test_masks(scale: f32) -> Vec<filmcraft_render::mask::FlatMask> {
    use filmcraft_project::{Mask, MaskMode, MaskPath, ParamValue};
    let mut a = Mask::new("a", MaskPath::ellipse(Vec2::new(150.0, 90.0), Vec2::new(90.0, 55.0)));
    a.feather.value = ParamValue::Float(24.0);
    a.expansion.value = ParamValue::Float(6.0);
    let mut b = Mask::new("b", MaskPath::polygon(&[Vec2::new(40.0, 20.0), Vec2::new(260.0, 50.0), Vec2::new(120.0, 170.0)]));
    b.mode = MaskMode::Subtract;
    b.feather.value = ParamValue::Float(0.0);
    b.opacity.value = ParamValue::Float(70.0);
    let mut c = Mask::new("c", MaskPath::rect(200.0, 100.0, 300.0, 175.0));
    c.mode = MaskMode::Difference;
    c.inverted = true;
    c.feather.value = ParamValue::Float(3.5);
    filmcraft_render::mask::prepare(&[a, b, c], filmcraft_time::Tick(0), scale)
}

#[test]
fn gpu_mask_coverage_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = crate::GpuMask::new(&dev, &q);
    for scale in [1.0f32, 0.5] {
        let masks = test_masks(scale);
        let (w, h) = ((320.0 * scale) as usize, (180.0 * scale) as usize);
        let cpu = filmcraft_render::mask::coverage(&masks, w, h).unwrap();
        let gpu = g.coverage(&masks, w, h).expect("gpu coverage");
        let worst = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        eprintln!("mask coverage @{scale}: max |cpu − gpu| = {worst:e}");
        assert!(worst < 2e-4, "scale {scale}: {worst}");
        assert!(cpu.iter().any(|v| *v > 0.99) && cpu.iter().any(|v| *v < 0.01) && cpu.iter().any(|v| *v > 0.2 && *v < 0.8));
    }
}

#[test]
fn gpu_masked_mix_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = crate::GpuMask::new(&dev, &q);
    let masks = test_masks(1.0);
    let (w, h) = (320usize, 180usize);
    let original = filmcraft_render::Image { w, h, px: (0..w * h * 4).map(|i| ((i * 37) % 101) as f32 / 100.0).collect() };
    let mut effected = original.clone();
    effected.px.iter_mut().for_each(|v| *v = 1.0 - *v * 0.5);
    let gpu = g.mix(&masks, w, h, &original.px, &effected.px).expect("gpu mix");
    let cov = filmcraft_render::mask::coverage(&masks, w, h).unwrap();
    let mut cpu = effected.clone();
    filmcraft_render::mask::mix(&mut cpu, &original, &cov);
    let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    eprintln!("masked mix: max |cpu − gpu| = {worst:e}");
    assert!(worst < 2e-4, "{worst}");
}

/// A 4:4:4 grey picture with an alpha plane that ramps from transparent (left) to opaque (right).
fn yuv_alpha_frame(w: u32, h: u32, bits: u32) -> Arc<VideoFrame> {
    let n = (w * h) as usize;
    let max = (1u32 << bits) - 1;
    let ramp = |i: usize| ((i % w as usize) as u32 * max / (w - 1)) as u16;
    let data = if bits == 8 {
        PixelData::Yuv8 {
            planes: [Arc::new(vec![180u8; n]), Arc::new(vec![100u8; n]), Arc::new(vec![170u8; n])],
            chroma: Chroma::C444,
            alpha: Some(Arc::new((0..n).map(|i| ramp(i) as u8).collect())),
        }
    } else {
        let s = |v: u32| (v << (bits - 8)) as u16;
        PixelData::Yuv16 {
            planes: [Arc::new(vec![s(180); n]), Arc::new(vec![s(100); n]), Arc::new(vec![s(170); n])],
            chroma: Chroma::C444,
            bits,
            alpha: Some(Arc::new((0..n).map(ramp).collect())),
        }
    };
    Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() })
}

/// A Y'CbCr layer with an alpha plane (ProRes 4444) is drawn with its alpha: the GPU result matches
/// the CPU compositor over an opaque background. (It used to draw such a layer opaque.)
#[test]
fn gpu_draws_yuv_alpha_plane_like_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (320usize, 180usize);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::OceanSunset),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    )
    .unwrap();
    let background = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    for bits in [8, 10, 12] {
        let mut c = GpuCompositor::new(&dev, &q);
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                PlanLayer { frame: background.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
                PlanLayer { frame: yuv_alpha_frame(320, 180, bits), matrix: Affine::IDENTITY, opacity: 0.9, blend: Blend::Normal, fx: None, input: None },
            ],
        };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let mut diffs: Vec<u32> =
            cpu.chunks(4).zip(gpu.chunks(4)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0)).collect();
        diffs.sort_unstable();
        let (p99, max) = (diffs[diffs.len() * 99 / 100], diffs[diffs.len() - 1]);
        assert!(p99 <= 3 && max <= 8, "{bits}-bit: p99 {p99}, max {max}");
        // the transparent left edge shows the background, the opaque right edge the layer
        let at = |img: &[u8], x: usize| (img[(90 * w + x) * 4], img[(90 * w + x) * 4 + 1], img[(90 * w + x) * 4 + 2]);
        let bg = execute_cpu(&FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![PlanLayer { frame: background.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }],
        })
        .over_black_rgba8();
        let (l, r, b0) = (at(&gpu, 1), at(&gpu, w - 2), at(&bg, 1));
        assert!((l.0 as i32 - b0.0 as i32).abs() <= 6 && (l.1 as i32 - b0.1 as i32).abs() <= 6, "{bits}-bit: left edge {l:?} vs background {b0:?}");
        assert!(r != at(&bg, w - 2), "{bits}-bit: right edge {r:?} should differ from the background");
    }
}

#[test]
fn fast_half_float_conversion_matches_the_reference_for_every_value() {
    for h in 0..=u16::MAX {
        let (fast, slow) = (f16_to_f32_fast(h), f16_to_f32(h));
        assert!(fast.to_bits() == slow.to_bits() || (fast.is_nan() && slow.is_nan()), "{h:#06x}: {fast} vs {slow}");
    }
}

/// HDR and wide-gamut Y'CbCr on a Rec. 709 sequence (#965): the shader's colour management
/// (curve, HLG OOTF, BT.2020 → 709 matrix, BT.2390 tone mapping, desaturation) matches the CPU
/// running the same `InputTransform`.
#[test]
fn hdr_input_gpu_matches_cpu() {
    use filmcraft_color::{ColorInfo, ColorPipeline, ColorSpace, InputTransform, Matrix, Primaries, Transfer};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    // a 10-bit luma ramp (to the top code) under neutral, saturated and extreme chroma, 4:4:4
    let (w, h) = (256u32, 6u32);
    let chroma = [(512, 512), (300, 760), (760, 300), (64, 512), (960, 960), (512, 64)];
    let y: Vec<u16> = (0..h).flat_map(|_| (0..w).map(|x| (64 + x * 876 / 255) as u16)).collect();
    let cb: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize].0, w as usize)).collect();
    let cr: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize].1, w as usize)).collect();
    let data = PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 10, alpha: None };
    let hdr = |transfer| ColorInfo { matrix: Matrix::Bt2020Ncl, transfer, primaries: Primaries::Bt2020, range: Range::Limited };
    let cases = [
        ("HLG", hdr(Transfer::Hlg), None),
        ("PQ", hdr(Transfer::Pq), None),
        ("PQ 4000 cd/m²", hdr(Transfer::Pq), Some(4000.0)),
        ("BT.2020 SDR", ColorInfo { primaries: Primaries::Bt2020, ..ColorInfo::REC709 }, None),
    ];
    for (name, color, peak) in cases {
        let frame = Arc::new(VideoFrame { width: w, height: h, data: data.clone(), color, par: (1, 1), pts: Default::default() });
        let cs = ColorSpace::from_info(&color);
        let t = Arc::new(InputTransform::new(cs, color.range, &ColorPipeline::REC709, peak));
        assert!(t.gpu_params().is_some(), "{name}");
        let layer = PlanLayer { input: Some(t), ..PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal) };
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
        let cpu = execute_cpu(&plan).over_black_rgba8();
        c.composite(&plan);
        let (_, _, gpu) = c.read_output().expect("readback");
        let error = cpu.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        // and the stage does something: the default decode of the same frame is far off
        let plain = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
        c.composite(&plain);
        let (_, _, unmanaged) = c.read_output().expect("readback");
        let apart = cpu.iter().zip(&unmanaged).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        eprintln!("{name}: max error {error}, unmanaged differs by up to {apart}");
        assert!(error <= 2, "{name}: GPU differs from the CPU by up to {error} codes");
        assert!(apart > 20, "{name}: colour management changed nothing ({apart})");
    }
}

#[test]
fn gpu_adjustment_step_matches_cpu_between_layers() {
    use filmcraft_project::ParamValue;
    use filmcraft_render::gpufx::FxOp;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (128usize, 72usize);
    // a soft picture at 70 % opacity: the canvas under the adjustment layer is partly transparent
    let below = {
        let px = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                [x, y, 0.5 * (1.0 - x), 1.0]
            })
            .collect();
        PlanLayer::new(Arc::new(VideoFrame::rgba_f32(w as u32, h as u32, px)), Affine::IDENTITY, 0.7, Blend::Normal)
    };
    // an opaque blue box above the adjustment layer, which must stay untouched
    let above = {
        let px = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if (40..70).contains(&x) && (20..40).contains(&y) { [0.0, 0.0, 1.0, 1.0] } else { [0.0; 4] }
            })
            .collect();
        PlanLayer::new(Arc::new(VideoFrame::rgba_f32(w as u32, h as u32, px)), Affine::IDENTITY, 1.0, Blend::Normal)
    };
    let effect = |id: &str, params: &[(&str, f64)]| {
        let mut e = filmcraft_project::find_effect(id).unwrap().instance();
        for (p, v) in params {
            e.param_mut(p).unwrap().value = ParamValue::Float(*v);
        }
        e
    };
    let cx = filmcraft_render::effects::FxCtx {
        t: filmcraft_time::Tick::ZERO,
        px_scale: 1.0,
        mask_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    let chain = [
        effect("proc_amp", &[("brightness", 20.0)]),
        effect("gaussian_blur", &[("blurriness", 8.0)]),
        effect("lumetri", &[("contrast", 20.0), ("exposure", -0.25), ("vignette_amount", -3.0)]),
    ];
    let ops: Vec<FxOp> = chain.iter().map(|e| FxOp::eval(e, &cx, w, h).filter(FxOp::gpu_ok).expect("GPU op")).collect();
    let plan =
        FramePlan::Composite { width: w, height: h, steps: vec![PlanStep::Layers(vec![below]), PlanStep::Adjust { ops }, PlanStep::Layers(vec![above])] };
    let reference = execute_cpu(&plan);
    // with the effect stage the step runs on the GPU; without one (a device that fails its
    // validation) the frame is composited on the CPU, with the same picture
    for stage in [true, false] {
        let mut c = GpuCompositor::new(&dev, &q);
        if stage && c.fx.is_none() {
            eprintln!("no effect stage on this device; skipping its half");
            continue;
        }
        if !stage {
            c.fx = None;
        }
        // Another frame's upload is pending and the pass has no budget left: the adjustment
        // step's own submit, which draws no frame's planes, leaves it for a pass with room.
        let up = c.uploader();
        let other = FramePlan::Layers { width: 64, height: 36, layers: vec![PlanLayer::new(yuv_frame(64, 36), Affine::IDENTITY, 1.0, Blend::Normal)] };
        let waiting = prepare(&other, Some(&up));
        assert_eq!(up.pending_copies(), 1);
        c.set_upload_budget(0);
        c.composite_prepared(&plan, Some(&prepare(&plan, None)));
        assert_eq!(up.pending_copies(), 1, "stage {stage}: the frame took an upload it does not draw");
        drop(waiting);
        // the Program monitor's path (sRGB over black)
        c.composite_prepared(&plan, Some(&prepare(&plan, None)));
        if stage {
            // the next frame reuses the step's working textures instead of making them again
            let created = c.fx.as_ref().map(fx::FxStage::created);
            c.composite_prepared(&plan, Some(&prepare(&plan, None)));
            assert_eq!(c.fx.as_ref().map(fx::FxStage::created), created, "the second frame made new working textures");
        }
        let (_, _, gpu) = c.read_output().expect("readback");
        let mut diffs: Vec<u32> = reference
            .over_black_rgba8()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(gpu.as_chunks::<4>().0)
            .map(|(a, b)| (0..3).map(|k| a[k].abs_diff(b[k]) as u32).max().unwrap())
            .collect();
        diffs.sort_unstable();
        let p99 = diffs[diffs.len() * 99 / 100];
        let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
        eprintln!("adjustment step, stage {stage}: p99 {p99}, mean {mean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "adjustment step, stage {stage}: p99 {p99}, mean {mean}");
        // the export's path (linear premultiplied f32, no resolve)
        let (ew, eh, px) = c.render_export_prepared(&plan, Some(&prepare(&plan, None))).expect("export render");
        assert_eq!((ew as usize, eh as usize), (w, h));
        let worst = px.iter().zip(&reference.px).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!("adjustment step, stage {stage}, export: max |GPU - CPU| {worst}");
        assert!(worst < 0.02, "stage {stage}: export render of the adjustment step differs by {worst}");
        let i = (30 * w + 55) * 4;
        assert_eq!(px.get(i..i + 4), Some(&[0.0, 0.0, 1.0, 1.0][..]), "the layer above the adjustment layer is untouched");
        let want = if stage { (4, 0) } else { (0, 3) };
        assert_eq!((c.gpu_adjustments, c.cpu_adjustments), want, "stage {stage}");
    }
}

/// Regression for #3418: several threads creating instances and devices, using them and dropping
/// them at once deadlocked inside the NVIDIA driver (Vulkan loader critical section in
/// `vkDestroyDevice` against other devices' `create_texture`/`submit`, plus WGL threads started by
/// GL-enabled instances). Each thread makes its own instance and device, as concurrent windows and exports do.
#[test]
fn many_threads_create_use_and_drop_devices_at_once() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    const THREADS: usize = 16;
    const ROUNDS: usize = 4;
    let (tx, rx) = mpsc::channel();
    for _ in 0..THREADS {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut used = 0usize;
            for _ in 0..ROUNDS {
                let Some((device, queue)) = device() else { break };
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::RED), store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                }));
                queue.submit([enc.finish()]);
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
                used += 1;
                drop((view, tex, queue, device));
            }
            let _ = tx.send(used);
        });
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(120);
    for i in 0..THREADS {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(rx.recv_timeout(left).is_ok(), "only {i} of {THREADS} threads finished within 120 s: device setup deadlocked");
    }
}

/// Require an adapter for the explicit lifecycle regressions; absence is not a passing skip.
fn retained_readback_compositor() -> GpuCompositor {
    let (dev, q) = device().expect("GPU adapter required for explicit readback regression");
    let mut compositor = GpuCompositor::new(&dev, &q);
    compositor.accum_readback_buf = Some((
        dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback-cleanup-regression"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }),
        256,
    ));
    compositor
}

/// Failed buffers must leave the cache; a new copy must recreate usable staging storage.
fn assert_readback_retired_and_rendering_recovers(compositor: &mut GpuCompositor) {
    assert!(compositor.accum_readback_buf.is_none(), "failed staging buffer remained cached");
    let plan = FramePlan::Layers { width: 1, height: 1, layers: vec![] };
    let (width, height, rgba) = compositor.render_export_prepared(&plan, None).expect("readback recovers with new staging storage");
    assert_eq!((width, height, rgba.len()), (1, 1, 4));
    assert!(compositor.accum_readback_buf.is_some(), "successful readback must retain its cache");
    let again = compositor.read_accumulator().expect("successful staging storage remains reusable");
    assert_eq!(again, (width, height, rgba));
}

/// A wait failure after a real completed map must not retain the failed staging storage.
#[test]
#[ignore = "requires a working GPU adapter for staging-buffer lifecycle verification"]
fn accumulator_readback_retires_after_failed_wait() {
    let mut compositor = retained_readback_compositor();
    assert!(
        compositor
            .map_accumulator_with_wait((1, 1, 256), |device, rx| {
                assert!(wait_mapped(device, rx).is_some());
                None
            })
            .is_none()
    );
    assert_readback_retired_and_rendering_recovers(&mut compositor);
}

/// Reject hostile conversion dimensions without retaining a successfully mapped buffer.
#[test]
#[ignore = "requires a working GPU adapter for staging-buffer lifecycle verification"]
fn accumulator_readback_retires_after_rejected_conversion() {
    let mut compositor = retained_readback_compositor();
    assert!(compositor.map_accumulator((u32::MAX, u32::MAX, 256)).is_none());
    assert_readback_retired_and_rendering_recovers(&mut compositor);
}

/// Retire a still-pending map before the next copy/readback attempt uses the cache.
#[test]
#[ignore = "requires a working GPU adapter for staging-buffer lifecycle verification"]
fn accumulator_readback_retires_pending_map_after_failed_wait() {
    let mut compositor = retained_readback_compositor();
    assert!(compositor.map_accumulator_with_wait((1, 1, 256), |_, _| None).is_none());
    assert_readback_retired_and_rendering_recovers(&mut compositor);
}

/// Real callback rejection may already leave native storage unmapped; do not unmap it twice.
#[test]
#[ignore = "requires a working GPU adapter for staging-buffer lifecycle verification"]
fn accumulator_readback_retires_rejected_native_map() {
    let mut compositor = retained_readback_compositor();
    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let observed = errors.clone();
    compositor.device.on_uncaptured_error(Arc::new(move |error| {
        observed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("{error}"));
    }));
    compositor.accum_readback_buf.as_ref().unwrap().0.destroy();
    assert!(
        compositor
            .map_accumulator_with_wait((1, 1, 256), |device, rx| {
                device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) }).unwrap();
                assert!(matches!(rx.recv_timeout(MAP_TIMEOUT), Ok(Err(_))), "expected an actual rejected map callback");
                None
            })
            .is_none()
    );
    assert_readback_retired_and_rendering_recovers(&mut compositor);
    assert!(
        !errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|error| error.contains("Buffer is not mapped")),
        "cleanup attempted to unmap an already rejected mapping"
    );
}
