use super::*;

/// Describe the fixed 4x4 NV12/P010 picture used by the retention regression tests.
fn test_layout(bits: u32) -> Layout {
    Layout {
        pixel_format: if bits == 8 { NV12_VIDEO } else { P010_VIDEO },
        coded: (4, 4),
        crop: (0, 0, 4, 4),
        chroma: Chroma::C420,
        bits,
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        alpha: false,
    }
}

/// Allocate and initialize a real CoreVideo buffer with deterministic luma and chroma values.
fn buffer(bits: u32) -> CFRetained<CVImageBuffer> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: fixed valid dimensions/format and a valid output pointer; no borrowed storage.
    let status = unsafe { objc2_core_video::CVPixelBufferCreate(None, 4, 4, test_layout(bits).pixel_format, None, NonNull::from(&mut raw)) };
    assert_eq!(status, 0);
    // SAFETY: creation succeeded and returns a retained (+1) buffer.
    let pb = unsafe { CFRetained::from_raw(NonNull::new(raw).expect("created pixel buffer")) };
    // SAFETY: the pixel buffer is live and uniquely held while its test pixels are initialized.
    assert_eq!(unsafe { CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::empty()) }, 0);
    for i in 0..2 {
        let base = CVPixelBufferGetBaseAddressOfPlane(&pb, i) as *mut u8;
        let len = CVPixelBufferGetBytesPerRowOfPlane(&pb, i) * CVPixelBufferGetHeightOfPlane(&pb, i);
        assert!(!base.is_null());
        // SAFETY: CoreVideo maps this plane's stride * height bytes; the exclusive buffer is locked.
        unsafe { std::slice::from_raw_parts_mut(base, len) }.fill(0x25 + i as u8 * 0x10);
    }
    // SAFETY: matches the successful lock above, before the buffer is shared.
    unsafe { CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::empty()) };
    pb
}

/// Wrap a test buffer with a selectable view width so oversized views can force copy validation errors.
fn surface(bits: u32, w: usize) -> VtSurface {
    VtSurface {
        pb: Mutex::new(Some(buffer(bits))),
        layout: test_layout(bits),
        ox: 0,
        oy: 0,
        w,
        h: 4,
        u8_planes: Mutex::new(None),
        u16_planes: Mutex::new(None),
    }
}

/// Create empty decoder callback and retention queues for the requested test bit depth.
fn shared(bits: u32) -> Shared {
    Shared { layout: Mutex::new(test_layout(bits)), out: Mutex::new(Vec::new()), surfaces: Mutex::new(VecDeque::new()) }
}

/// A failed CPU copy must preserve the same buffer for repeated failures and eventual recovery.
#[test]
fn failed_surface_eviction_preserves_the_buffer_for_repeated_reads_and_recovery() {
    for bits in [8, 10] {
        // A real CoreVideo buffer with a deliberately oversized requested view forces the actual
        // plane-copy validation to fail without depending on a driver lock failure.
        let mut s = surface(bits, 5);
        for _ in 0..2 {
            assert_eq!(s.evict_to_cpu().unwrap_err(), "decoded picture planes are smaller than the picture");
            if bits == 8 {
                assert!(s.planes_u8().is_none());
            } else {
                assert!(s.planes_u16().is_none());
            }
            assert!(s.pb.lock().unwrap().is_some(), "{bits}-bit original must survive a failed copy");
            assert!(s.u8_planes.lock().unwrap().is_none());
            assert!(s.u16_planes.lock().unwrap().is_none());
        }
        s.w = 4;
        s.evict_to_cpu().expect("the same buffer must remain readable after correcting the view");
        assert!(s.pb.lock().unwrap().is_none());
        s.evict_to_cpu().expect("repeat eviction uses the cached planes");
        if bits == 8 {
            let [y, uv] = s.planes_u8().expect("CPU planes survive releasing the buffer");
            assert_eq!(y.as_slice(), &[0x25; 16]);
            assert_eq!(uv.as_slice(), &[0x35; 8]);
        } else {
            let [y, uv] = s.planes_u16().expect("CPU planes survive releasing the buffer");
            assert_eq!(y.as_slice(), &[0x2525; 16]);
            assert_eq!(uv.as_slice(), &[0x3535; 8]);
        }
    }
}

/// Failed eviction must reject the new output while keeping the oldest buffer and queue entry.
#[test]
fn failed_surface_eviction_rejects_new_frames_without_losing_tracking() {
    for bits in [8, 10] {
        let state = shared(bits);
        let held: Vec<_> = (0..MAX_RETAINED_SURFACES).map(|i| Arc::new(surface(bits, if i == 0 { 5 } else { 4 }))).collect();
        for s in &held {
            state.track_surface(s).unwrap();
        }
        let pb = buffer(bits);
        for _ in 0..2 {
            let error = copy_out(&pb, &test_layout(bits), &state).unwrap_err();
            assert!(error.contains("cannot evict retained VideoToolbox surface"), "{error}");
            assert!(error.contains("decoded picture planes are smaller than the picture"), "{error}");
            let q = state.surfaces.lock().unwrap();
            assert_eq!(q.len(), MAX_RETAINED_SURFACES);
            assert!(Weak::ptr_eq(q.front().unwrap(), &Arc::downgrade(&held[0])));
            assert!(held.iter().all(|s| s.pb.lock().unwrap().is_some()));
        }
    }
}

/// Successful eviction must bound native ownership without changing the retained CPU pixel values.
#[test]
fn successful_surface_eviction_keeps_the_retention_limit_and_original_pixels() {
    for bits in [8, 10] {
        let state = shared(bits);
        let mut held = Vec::new();
        for _ in 0..MAX_RETAINED_SURFACES * 3 {
            let s = Arc::new(surface(bits, 4));
            state.track_surface(&s).unwrap();
            held.push(s);
            assert!(state.surfaces.lock().unwrap().len() <= MAX_RETAINED_SURFACES);
            assert_eq!(held.iter().filter(|s| s.pb.lock().unwrap().is_some()).count(), held.len().min(MAX_RETAINED_SURFACES));
        }
        for s in &held {
            if bits == 8 {
                let [y, uv] = s.planes_u8().unwrap();
                assert_eq!(y.as_slice(), &[0x25; 16]);
                assert_eq!(uv.as_slice(), &[0x35; 8]);
            } else {
                let [y, uv] = s.planes_u16().unwrap();
                assert_eq!(y.as_slice(), &[0x2525; 16]);
                assert_eq!(uv.as_slice(), &[0x3535; 8]);
            }
        }
    }
}
