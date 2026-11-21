//! Scope maths on generated frames with known values.

use crate::*;
use filmcraft_color::rgb_to_ycbcr;

fn rgba8(w: usize, h: usize, mut f: impl FnMut(usize, usize) -> [u8; 3]) -> Vec<u8> {
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

fn solid(w: usize, h: usize, c: [u8; 3]) -> Signal {
    Signal::from_rgba8(w, h, &rgba8(w, h, |_, _| c))
}

/// 75 % colour bars (code 191): white, yellow, cyan, green, magenta, red, blue, black; 32 px each.
const BAR: u8 = 191;
const BARS: [[u8; 3]; 8] = [[BAR, BAR, BAR], [BAR, BAR, 0], [0, BAR, BAR], [0, BAR, 0], [BAR, 0, BAR], [BAR, 0, 0], [0, 0, BAR], [0, 0, 0]];

fn bars() -> Signal {
    Signal::from_rgba8(256, 16, &rgba8(256, 16, |x, _| BARS[x / 32]))
}

fn code(v: u8) -> f32 {
    v as f32 / 255.0
}

#[test]
fn single_colour_lands_in_exact_histogram_bins() {
    let s = solid(64, 36, [200, 100, 50]);
    let n = 64 * 36;
    let h = histogram(&s, &Params::default());
    assert_eq!(h.samples, n);
    assert_eq!((h.r[200], h.g[100], h.b[50]), (n, n, n));
    assert_eq!(h.r.iter().sum::<u32>(), n);
    let y = rgb_to_ycbcr(code(200), code(100), code(50), Matrix::Bt709)[0];
    assert_eq!(h.y[(y * 255.0).round() as usize], n);
    assert_eq!((h.below, h.above), ([0; 4], [0; 4]));
}

#[test]
fn ramp_fills_every_bin_and_waveform_row_equals_code_value() {
    let s = Signal::from_rgba8(256, 10, &rgba8(256, 10, |x, _| [x as u8; 3]));
    let p = Params::default();
    let h = histogram(&s, &p);
    for k in 0..256 {
        assert_eq!((h.r[k], h.g[k], h.b[k], h.y[k]), (10, 10, 10, 10), "bin {k}");
    }
    for kind in [WaveformType::Luma, WaveformType::YcNoChroma] {
        let w = waveform(&s, kind, &p);
        let y = w.trace("Y").unwrap();
        for x in 0..256 {
            assert_eq!(y.rows_in(x), vec![x], "{kind:?} column {x}");
            assert_eq!(y.at(x, x), 10);
        }
    }
    let w = waveform(&s, WaveformType::Rgb, &p);
    assert_eq!(w.traces.len(), 3);
    for t in &w.traces {
        assert!((0..256).all(|x| t.rows_in(x) == vec![x]), "{}", t.name);
    }
}

#[test]
fn colour_bars_parade_levels_and_vectorscope_targets() {
    let s = bars();
    let p = Params::default();
    // RGB parade: every bar column holds its channel's code value exactly
    let par = parade(&s, ParadeType::Rgb, &p);
    for (ch, name) in ["R", "G", "B"].iter().enumerate() {
        let t = par.trace(name).unwrap();
        for (i, bar) in BARS.iter().enumerate() {
            for x in i * 32..(i + 1) * 32 {
                assert_eq!(t.rows_in(x), vec![bar[ch] as usize], "{name} bar {i}");
                assert_eq!(t.at(x, bar[ch] as usize), 16);
            }
        }
    }
    // RGB-White adds the luma trace; YUV has Y, Cb, Cr
    assert_eq!(parade(&s, ParadeType::RgbWhite, &p).traces.iter().map(|t| t.name).collect::<Vec<_>>(), ["R", "G", "B", "Y"]);
    assert_eq!(parade(&s, ParadeType::Yuv, &p).traces.iter().map(|t| t.name).collect::<Vec<_>>(), ["Y", "Cb", "Cr"]);
    // Vectorscope: each coloured bar on its 75 % target cell, white and black in the centre
    for m in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
        let p = Params { matrix: m, ..Params::default() };
        let v = vectorscope_yuv(&s, &p);
        assert_eq!(v.samples, 256 * 16);
        let bar_samples = 32 * 16;
        for t in targets(m, code(BAR)) {
            let (x, y) = v.cell(t.cb, t.cr).unwrap();
            assert_eq!(v.at(x, y), bar_samples, "{m:?} {}", t.name);
        }
        let (cx, cy) = v.cell(0.0, 0.0).unwrap();
        assert_eq!(v.at(cx, cy), 2 * bar_samples);
        // the densest spots: the centre (two bars), then the six targets
        let pk = summary::peaks(&v, 10, 0.01);
        assert_eq!(pk.len(), 7);
        assert_eq!((pk[0].x, pk[0].y, pk[0].share), (cx, cy, 0.25));
        assert!(pk[1..].iter().all(|k| k.share == 0.125));
    }
}

#[test]
fn vectorscope_positions_of_primaries_are_known() {
    let p = Params::default();
    // BT.709 red: Cb = −0.2126 / 1.8556 = −0.11457, Cr = 0.5
    let v = vectorscope_yuv(&solid(8, 8, [255, 0, 0]), &p);
    let pk = summary::peaks(&v, 1, 0.0)[0];
    assert_eq!((pk.x, pk.y), (103, 21));
    assert_eq!((pk.share, v.samples), (1.0, 64));
    assert!((red_angle(Matrix::Bt709) - 102.906).abs() < 0.01, "{}", red_angle(Matrix::Bt709));
    assert!((red_angle(Matrix::Bt601) - 108.645).abs() < 0.01, "{}", red_angle(Matrix::Bt601));
    // the mean chroma of a flat colour is its own
    let [_, cb, cr] = rgb_to_ycbcr(1.0, 0.0, 0.0, Matrix::Bt709);
    assert!((v.mean[0] - cb).abs() < 1e-6 && (v.mean[1] - cr).abs() < 1e-6);
    // greys sit in the centre for every matrix
    for m in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
        let v = vectorscope_yuv(&solid(4, 4, [77, 77, 77]), &Params { matrix: m, ..p });
        assert_eq!(v.at(127, 127), 16, "{m:?}");
    }
    // the skin-tone line sits between red and yellow
    let yl = targets(Matrix::Bt709, 1.0)[5];
    assert!(red_angle(Matrix::Bt709) < SKIN_TONE_DEG && SKIN_TONE_DEG < angle_deg(yl.cb, yl.cr));
}

#[test]
fn hls_vectorscope_puts_hue_on_the_angle_and_saturation_on_the_radius() {
    let p = Params::default();
    let red = red_angle(p.matrix);
    for (rgb, hue) in [([255, 0, 0], 0.0f32), ([255, 255, 0], 60.0), ([0, 255, 0], 120.0), ([0, 255, 255], 180.0), ([0, 0, 255], 240.0), ([255, 0, 255], 300.0)]
    {
        let v = vectorscope_hls(&solid(4, 4, rgb), &p);
        let a = (red + hue).to_radians();
        let want = cell_of(0.5 * a.cos(), 0.5 * a.sin(), p.vector_size, VECTOR_EXTENT).unwrap();
        let pk = summary::peaks(&v, 1, 0.0)[0];
        assert_eq!((pk.x, pk.y), want, "{rgb:?}");
        assert!((pk.magnitude - 100.0).abs() < 0.6, "{rgb:?} {}", pk.magnitude);
    }
    // half-saturated red (HSL s = 0.5) at half the radius; grey in the centre
    let v = vectorscope_hls(&solid(4, 4, [191, 64, 64]), &p);
    let pk = summary::peaks(&v, 1, 0.0)[0];
    assert!((pk.magnitude - 50.0).abs() < 1.0 && (pk.angle_deg - red as f64).abs() < 0.5, "{pk:?}");
    let v = vectorscope_hls(&solid(4, 4, [90, 90, 90]), &p);
    assert_eq!(v.at(127, 127), 16);
}

#[test]
fn yc_waveform_draws_chroma_around_luma() {
    let p = Params::default();
    let w = waveform(&solid(4, 4, [128, 128, 128]), WaveformType::Yc, &p);
    assert_eq!((w.trace("Y").unwrap().rows_in(0), w.trace("C").unwrap().rows_in(0)), (vec![128], vec![128]));
    let w = waveform(&solid(4, 4, [255, 0, 0]), WaveformType::Yc, &p);
    let [y, cb, cr] = rgb_to_ycbcr(1.0, 0.0, 0.0, Matrix::Bt709);
    let c = (cb * cb + cr * cr).sqrt();
    let row = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as usize;
    assert_eq!(w.trace("Y").unwrap().rows_in(2), vec![row(y)]);
    assert_eq!(w.trace("C").unwrap().rows_in(2), vec![row(y - c), row(y + c)]);
}

#[test]
fn yuv_parade_offsets_chroma_to_mid_scale() {
    let w = parade(&solid(4, 4, [128, 128, 128]), ParadeType::Yuv, &Params::default());
    assert_eq!(w.trace("Y").unwrap().rows_in(0), vec![128]);
    assert_eq!(w.trace("Cb").unwrap().rows_in(0), vec![128]);
    assert_eq!(w.trace("Cr").unwrap().rows_in(0), vec![128]);
}

#[test]
fn decimation_keeps_whole_samples() {
    let px = rgba8(1920, 1080, |x, y| [(x % 256) as u8, (y % 256) as u8, 7]);
    let s = Signal::from_rgba8(1920, 1080, &px);
    assert_eq!((s.w, s.h), (480, 270));
    assert_eq!(s.rgb[1], [code(4), 0.0, code(7)]);
    assert_eq!(s.rgb[480], [0.0, code(4), code(7)]);
    let s = Signal::from_rgba8(1921, 9, &rgba8(1921, 9, |_, _| [1, 2, 3]));
    assert_eq!((s.w, s.h), (385, 9));
    assert!(Signal::from_rgba8(10, 10, &[0; 12]).is_empty());
}

#[test]
fn clamp_signal_controls_out_of_range_values() {
    let s = Signal::from_fn(4, 4, |x, _| if x < 2 { [1.2, 0.5, -0.05] } else { [0.5, 0.5, 0.5] });
    let on = Params::default();
    let off = Params { clamp: false, ..on };
    let h = histogram(&s, &on);
    assert_eq!((h.r[255], h.above[0], h.b[0], h.below[2]), (8, 8, 8, 8));
    let h = histogram(&s, &off);
    assert_eq!((h.r[255], h.above[0], h.b[0], h.below[2]), (0, 8, 0, 8));
    // waveform: clamped onto the top row, or plotted above 100 % within −10..110
    let w = waveform(&s, WaveformType::Rgb, &on);
    assert_eq!(w.trace("R").unwrap().rows_in(0), vec![255]);
    let w = waveform(&s, WaveformType::Rgb, &off);
    assert_eq!((w.lo, w.hi), (-0.1, 1.1));
    assert!(w.trace("R").unwrap().rows_in(0).is_empty(), "1.2 is outside −10..110");
    let r = w.trace("B").unwrap().rows_in(0);
    assert_eq!(r, vec![row_of(-0.05, -0.1, 1.1, 256).unwrap()]);
}

#[test]
fn hdr_axis_helpers() {
    assert!(nits_to_pq(0.0) < 1e-5);
    assert!((nits_to_pq(10_000.0) - 1.0).abs() < 1e-6);
    // reference white (203 cd/m²) ≈ 58 % of the PQ range (BT.2408)
    let w = linear_to_pq([1.0, 1.0, 1.0, 1.0])[0];
    assert!((w - nits_to_pq(203.0)).abs() < 1e-6 && (w - 0.58).abs() < 0.01, "{w}");
    // premultiplied: half-covered white is still white
    assert!((linear_to_pq([0.5, 0.5, 0.5, 0.5])[0] - w).abs() < 1e-6);
    assert!((sdr_to_pq(1.0) - nits_to_pq(100.0)).abs() < 1e-6);
}

#[test]
fn nan_and_empty_signals_do_not_panic() {
    let s = Signal::from_fn(3, 3, |x, _| if x == 1 { [f32::NAN; 3] } else { [0.5; 3] });
    for clamp in [true, false] {
        let p = Params { clamp, ..Params::default() };
        for k in ScopeKind::ALL {
            let _ = compute(k, &s, &p, WaveformType::Yc, ParadeType::RgbWhite);
            let _ = compute(k, &Signal::default(), &p, WaveformType::Rgb, ParadeType::Rgb);
        }
    }
    assert!(summary::channel_stats(&Signal::default(), Matrix::Bt709).is_none());
}

#[test]
fn summaries_report_levels_in_percent() {
    let s = solid(8, 8, [200, 100, 50]);
    let st = summary::channel_stats(&s, Matrix::Bt709).unwrap();
    assert_eq!((st.r.mean, st.g.mean, st.b.mean), (78.43, 39.22, 19.61));
    assert_eq!((st.r.min, st.r.max), (78.43, 78.43));
    // a two-level frame: left half black, right half white
    let s = Signal::from_rgba8(100, 4, &rgba8(100, 4, |x, _| if x < 50 { [0; 3] } else { [255; 3] }));
    let w = waveform(&s, WaveformType::Luma, &Params::default());
    let cols = summary::trace_columns(&w, w.trace("Y").unwrap(), 4);
    assert_eq!(cols.len(), 4);
    assert_eq!(cols[0].unwrap(), summary::Level { min: 0.0, max: 0.0, mean: 0.0 });
    assert_eq!(cols[3].unwrap(), summary::Level { min: 100.0, max: 100.0, mean: 100.0 });
    assert_eq!((cols[1].unwrap().max, cols[2].unwrap().min), (0.0, 100.0));
    let c = summary::coarse(&vectorscope_yuv(&bars(), &Params::default()), 16);
    assert_eq!(c.iter().map(|e| e[2]).sum::<u32>(), 256 * 16);
}

#[test]
fn paint_adds_traces_and_leaves_empty_cells_transparent() {
    let s = solid(4, 8, [128, 128, 128]);
    let w = waveform(&s, WaveformType::Rgb, &Params::default());
    let layers: Vec<(&Grid, [f32; 3])> = w.traces.iter().zip([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).collect();
    let (pw, ph, px) = paint::grids(&layers, paint::waveform_reference(w.per_column), 1.0);
    assert_eq!((pw, ph, px.len()), (4, 256, 4 * 256 * 4));
    // row 128 from the bottom = image row 127; R+G+B coincide → white
    let i = (127 * 4) * 4;
    assert_eq!(&px[i..i + 4], &[255, 255, 255, 255]);
    assert_eq!(&px[0..4], &[0, 0, 0, 0]);
    // dimmed is darker than bright
    assert!(paint::intensity(3, 100.0, Brightness::Dimmed.gain()) < paint::intensity(3, 100.0, Brightness::Bright.gain()));
    let v = vectorscope_yuv(&bars(), &Params::default());
    let (n, _, vp) = paint::vectorscope(&v, [1.0; 3], 1.0, true);
    assert_eq!(vp.len(), n * n * 4);
    // seven spots of 3 × 3 texels each (none touch)
    assert_eq!(vp.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count(), 7 * 9);
}

#[test]
fn names_round_trip() {
    for k in ScopeKind::ALL {
        assert_eq!(ScopeKind::from_name(k.name()), Some(k));
        assert_eq!(ScopeKind::from_name(k.label()), Some(k));
        assert_eq!(serde_json::to_value(k).unwrap(), serde_json::Value::from(k.name()));
    }
    assert_eq!(ScopeKind::from_name("vectorscope"), Some(ScopeKind::VectorscopeYuv));
    assert_eq!(WaveformType::from_name("YC no Chroma"), Some(WaveformType::YcNoChroma));
    assert_eq!(ParadeType::from_name("rgb-white"), Some(ParadeType::RgbWhite));
    assert_eq!(ColorSpace::from_name("709"), Some(ColorSpace::Rec709));
    assert_eq!(ColorSpace::from_name("Rec. 2100"), Some(ColorSpace::Rec2100));
    assert_eq!(Scale::from_name("8 bit"), Some(Scale::Bits8));
    assert_eq!(serde_json::to_value(Targets::Percent100).unwrap(), serde_json::Value::from("100"));
    assert_eq!(ColorSpace::Auto.resolve(true), ColorSpace::Rec2100);
    assert_eq!(ColorSpace::Auto.resolve(false).matrix(), Matrix::Bt709);
}

/// `cargo test --release -p filmcraft-scopes perf -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_scopes_at_1080p() {
    let mut seed = 0x1234_5678u32;
    let px = rgba8(1920, 1080, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        [seed as u8, (seed >> 8) as u8, (seed >> 16) as u8]
    });
    let reps = 20;
    let t = std::time::Instant::now();
    for _ in 0..reps {
        std::hint::black_box(Signal::from_rgba8(1920, 1080, &px));
    }
    println!("decimate 1920x1080: {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / reps as f64);
    let s = Signal::from_rgba8(1920, 1080, &px);
    let p = Params::default();
    for (k, wt, pt) in [
        (ScopeKind::Waveform, WaveformType::Rgb, ParadeType::Rgb),
        (ScopeKind::Waveform, WaveformType::Yc, ParadeType::Rgb),
        (ScopeKind::Parade, WaveformType::Rgb, ParadeType::RgbWhite),
        (ScopeKind::Histogram, WaveformType::Rgb, ParadeType::Rgb),
        (ScopeKind::VectorscopeYuv, WaveformType::Rgb, ParadeType::Rgb),
        (ScopeKind::VectorscopeHls, WaveformType::Rgb, ParadeType::Rgb),
    ] {
        let t = std::time::Instant::now();
        for _ in 0..reps {
            std::hint::black_box(compute(k, &s, &p, wt, pt));
        }
        println!("{k:?} {wt:?}/{pt:?}: {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / reps as f64);
    }
}

#[test]
fn angle_deg_tiny_negative_angle_stays_below_360() {
    for (cb, cr) in [(0.3, -1e-9), (1.0, -1e-12)] {
        let a = angle_deg(cb, cr);
        assert!((0.0..360.0).contains(&a), "angle_deg({cb}, {cr}) = {a}");
    }
}
