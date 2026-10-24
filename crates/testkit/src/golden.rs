//! Golden-image testing: RGBA8 images, PNG I/O, difference metrics and reference checks.
//!
//! A golden test renders an image, then calls [`assert_golden`] with the path of a committed
//! reference PNG and a [`Tolerance`]. On mismatch the actual image and an amplified difference
//! image are written to `<workspace>/target/golden-failures/` for inspection.
//!
//! `FILMCRAFT_BLESS=1` (re)writes the reference instead of comparing, plus its `.attribution`
//! sidecar when missing (AGENTS.md §1). References must be produced by FilmCraft itself (original
//! work), be small (< [`MAX_GOLDEN_BYTES`]) and be listed in `ATTRIBUTION.md`.

use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};

/// Size limit for a committed golden PNG.
pub const MAX_GOLDEN_BYTES: u64 = 50 * 1024;

/// An 8-bit RGBA image, row-major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Rgba8 {
    pub fn new(w: u32, h: u32, px: Vec<u8>) -> Self {
        assert_eq!(px.len(), w as usize * h as usize * 4, "pixel buffer size");
        Self { w, h, px }
    }
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.w as usize + x as usize) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
    fn opaque(&self) -> bool {
        self.px.as_chunks::<4>().0.iter().all(|p| p[3] == 255)
    }
}

/// Encode as PNG (RGB when fully opaque, else RGBA), maximum compression.
pub fn encode_png(img: &Rgba8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        let opaque = img.opaque();
        enc.set_color(if opaque { png::ColorType::Rgb } else { png::ColorType::Rgba });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        if opaque {
            let rgb: Vec<u8> = img.px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            w.write_image_data(&rgb).map_err(|e| e.to_string())?;
        } else {
            w.write_image_data(&img.px).map_err(|e| e.to_string())?;
        }
        w.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decode a PNG (8-bit gray/RGB/RGBA, palette expanded) to RGBA8.
pub fn decode_png(bytes: &[u8]) -> Result<Rgba8, String> {
    let mut dec = png::Decoder::new(BufReader::new(Cursor::new(bytes)));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("PNG too large")?];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width, info.height);
    let n = w as usize * h as usize;
    let px = match info.color_type {
        png::ColorType::Rgba => buf[..n * 4].to_vec(),
        png::ColorType::Rgb => buf[..n * 3].as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..n * 2].as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf[..n].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("unexpanded palette PNG".into()),
    };
    Ok(Rgba8 { w, h, px })
}

pub fn read_png(path: &Path) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode_png(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_png(path: &Path, img: &Rgba8) -> Result<u64, String> {
    let bytes = encode_png(img)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes.len() as u64)
}

/// Per-pixel difference statistics over all four channels (8-bit levels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    /// Largest absolute channel difference.
    pub max_abs: u8,
    /// Mean absolute channel difference.
    pub mean_abs: f64,
    /// 99th percentile of the per-pixel maximum channel difference.
    pub p99: u8,
    /// PSNR in dB over all channels (`f64::INFINITY` when identical).
    pub psnr: f64,
    /// Pixels with any channel different.
    pub differing: usize,
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PSNR {:.2} dB, max {}, p99 {}, mean {:.3}, {} pixels differ", self.psnr, self.max_abs, self.p99, self.mean_abs, self.differing)
    }
}

/// Compare two images of equal size.
pub fn diff(a: &Rgba8, b: &Rgba8) -> Result<Diff, String> {
    if (a.w, a.h) != (b.w, b.h) {
        return Err(format!("size {}x{} vs {}x{}", a.w, a.h, b.w, b.h));
    }
    let mut hist = [0usize; 256];
    let (mut sum, mut sq, mut max, mut differing) = (0u64, 0u64, 0u8, 0usize);
    for (p, q) in a.px.as_chunks::<4>().0.iter().zip(b.px.as_chunks::<4>().0) {
        let mut m = 0u8;
        for k in 0..4 {
            let d = p[k].abs_diff(q[k]);
            sum += d as u64;
            sq += d as u64 * d as u64;
            m = m.max(d);
        }
        hist[m as usize] += 1;
        max = max.max(m);
        differing += (m > 0) as usize;
    }
    let pixels = (a.w as usize * a.h as usize).max(1);
    let target = (pixels * 99).div_ceil(100);
    let mut acc = 0;
    let mut p99 = 0u8;
    for (v, n) in hist.iter().enumerate() {
        acc += n;
        if acc >= target {
            p99 = v as u8;
            break;
        }
    }
    let n = (pixels * 4) as f64;
    let mse = sq as f64 / n;
    let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
    Ok(Diff { max_abs: max, mean_abs: sum as f64 / n, p99, psnr, differing })
}

/// Acceptance thresholds; every set limit must hold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    pub min_psnr: f64,
    pub max_abs: u8,
    pub p99: u8,
}

impl Tolerance {
    /// Bit-exact.
    pub const EXACT: Tolerance = Tolerance { min_psnr: f64::INFINITY, max_abs: 0, p99: 0 };
    /// Default for CPU renders compared with CPU-made references: absorbs float differences
    /// between platforms (FMA, SIMD widths, thread scheduling of reductions) but not real changes.
    pub const RENDER: Tolerance = Tolerance { min_psnr: 45.0, max_abs: 12, p99: 2 };

    pub fn accepts(&self, d: &Diff) -> bool {
        d.psnr >= self.min_psnr && d.max_abs <= self.max_abs && d.p99 <= self.p99
    }
}

/// Whether `FILMCRAFT_BLESS` asks to regenerate references.
pub fn blessing() -> bool {
    std::env::var("FILMCRAFT_BLESS").is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// Where failing comparisons write `<name>.actual.png` / `<name>.diff.png`.
pub fn failures_dir() -> PathBuf {
    crate::workspace_root().join("target").join("golden-failures")
}

/// Difference image: |a − b| per channel ×8, opaque.
pub fn diff_image(a: &Rgba8, b: &Rgba8) -> Rgba8 {
    let px =
        a.px.as_chunks::<4>()
            .0
            .iter()
            .zip(b.px.as_chunks::<4>().0)
            .flat_map(|(p, q)| {
                let d = |k: usize| (p[k].abs_diff(q[k]) as u32 * 8).min(255) as u8;
                let al = d(3);
                [d(0).max(al), d(1).max(al), d(2).max(al), 255]
            })
            .collect();
    Rgba8 { w: a.w, h: a.h, px }
}

/// Today's date (UTC) as YYYY-MM-DD, for attribution sidecars.
fn today() -> String {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (y, t) = utc_parts(since.as_secs());
    // `{:04}` rather than `%Y`, which writes a `+` before years past 9999.
    format!("{y:04}-{}", t.format("%m-%d"))
}

/// Unix seconds → (proleptic Gregorian year, the same wall-clock instant moved into chrono's range
/// by whole 400-year cycles). The Gregorian calendar repeats every 400 years (146_097 days), so
/// month and day come from chrono and only the year is carried, which keeps every input exact
/// instead of collapsing out-of-range years to the epoch. `secs` is `u64` (the clock is clamped at
/// the epoch before 1970), so `days` always fits an `i64`.
fn utc_parts(secs: u64) -> (i64, chrono::NaiveDateTime) {
    use chrono::Datelike;
    let (days, sod) = ((secs / 86_400) as i64, (secs % 86_400) as i64);
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest + 719_163` (1970-01-01 is CE day 719_163) is always a valid CE day, and `sod` < 86_400.
    let date = i32::try_from(rest + 719_163).ok().and_then(chrono::NaiveDate::from_num_days_from_ce_opt);
    let time = u32::try_from(sod).ok().and_then(|s| chrono::NaiveTime::from_num_seconds_from_midnight_opt(s, 0));
    match (date, time) {
        (Some(d), Some(t)) => {
            let y = i128::from(d.year()) + i128::from(cycles) * 400;
            (i64::try_from(y).unwrap_or(i64::MAX), d.and_time(t))
        }
        _ => (1970, chrono::NaiveDateTime::default()), // unreachable
    }
}

/// The attribution sidecar text for a generated golden image.
pub fn sidecar_text(file_name: &str, title: &str, generated_by: &str) -> String {
    format!(
        "asset:        {file_name}\n\
         title:        {title}\n\
         author:       FilmCraft contributors\n\
         source:       original work (rendered by FilmCraft's own renderer from a procedurally generated project; no external media)\n\
         license:      MIT OR Apache-2.0\n\
         license-file:\n\
         added:        {} by FilmCraft contributors (FILMCRAFT_BLESS=1)\n\
         notes:        Golden reference image generated by {generated_by}. Regenerate with FILMCRAFT_BLESS=1. Contains no Adobe artwork.\n",
        today()
    )
}

/// The `ATTRIBUTION.md` table row for a golden image at workspace-relative `rel`.
pub fn attribution_row(rel: &str, generated_by: &str) -> String {
    format!("| `{rel}` | FilmCraft contributors | Original work: golden reference rendered by FilmCraft ({generated_by}) | MIT OR Apache-2.0 |")
}

/// Compare `actual` with the golden PNG at `golden` (or bless it). `title` and `generated_by`
/// fill the attribution sidecar written on bless. Returns the difference on success.
pub fn check_golden(golden: &Path, actual: &Rgba8, tol: Tolerance, title: &str, generated_by: &str) -> Result<Diff, String> {
    let name = golden.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if blessing() {
        let size = write_png(golden, actual)?;
        if size > MAX_GOLDEN_BYTES {
            return Err(format!("{}: {size} bytes exceeds the {MAX_GOLDEN_BYTES}-byte golden limit; render smaller", golden.display()));
        }
        let side = PathBuf::from(format!("{}.attribution", golden.display()));
        if !side.exists() {
            let file = golden.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            std::fs::write(&side, sidecar_text(&file, title, generated_by)).map_err(|e| e.to_string())?;
        }
        let root = crate::workspace_root();
        let rel = golden.strip_prefix(&root).unwrap_or(golden).to_string_lossy().replace('\\', "/");
        let index = std::fs::read_to_string(root.join("ATTRIBUTION.md")).unwrap_or_default();
        if !index.contains(&format!("`{rel}`")) {
            eprintln!("golden {rel}: add this row to ATTRIBUTION.md:\n{}", attribution_row(&rel, generated_by));
        }
        eprintln!("blessed {rel} ({size} bytes)");
        return Ok(Diff { max_abs: 0, mean_abs: 0.0, p99: 0, psnr: f64::INFINITY, differing: 0 });
    }
    let expected = read_png(golden).map_err(|e| format!("{e}\n(missing or unreadable golden; generate it with FILMCRAFT_BLESS=1)"))?;
    let d = diff(&expected, actual).map_err(|e| format!("{}: {e}", golden.display()))?;
    if tol.accepts(&d) {
        return Ok(d);
    }
    let dir = failures_dir();
    let a = dir.join(format!("{name}.actual.png"));
    let _ = write_png(&a, actual);
    let _ = write_png(&dir.join(format!("{name}.diff.png")), &diff_image(&expected, actual));
    Err(format!(
        "{}: {d} (tolerance: PSNR ≥ {} dB, max ≤ {}, p99 ≤ {}); actual and diff written to {}",
        golden.display(),
        tol.min_psnr,
        tol.max_abs,
        tol.p99,
        dir.display()
    ))
}

/// [`check_golden`] that panics on failure.
pub fn assert_golden(golden: &Path, actual: &Rgba8, tol: Tolerance, title: &str, generated_by: &str) -> Diff {
    match check_golden(golden, actual, tol, title, generated_by) {
        Ok(d) => d,
        Err(e) => panic!("golden mismatch: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(secs: u64) -> String {
        let (y, t) = utc_parts(secs);
        format!("{y:04}-{}", t.format("%m-%d"))
    }

    #[test]
    fn utc_parts_preserves_out_of_range_years() {
        // `today()` uses `utc_parts`. Years far outside chrono's own range are preserved (the old
        // hand-rolled civil-from-days kept them) rather than collapsing to the epoch.
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_790_775_907), "2026-09-30");
        assert_eq!(ymd(951_782_400), "2000-02-29"); // 2000 is a leap year
        assert_eq!(ymd(253_402_300_800), "10000-01-01"); // just past chrono's range
        assert_eq!(ymd(10_000_000_000_000), "318857-05-20");
        assert_eq!(ymd(u64::MAX), "584554051223-11-09");
    }

    fn ramp(w: u32, h: u32, alpha: u8) -> Rgba8 {
        let px = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 77, alpha]).collect();
        Rgba8::new(w, h, px)
    }

    #[test]
    fn png_round_trip_rgb_and_rgba() {
        for a in [255, 128] {
            let img = ramp(37, 21, a);
            let png = encode_png(&img).unwrap();
            assert_eq!(decode_png(&png).unwrap(), img);
        }
    }

    #[test]
    fn diff_metrics() {
        let a = ramp(10, 10, 255);
        assert_eq!(diff(&a, &a).unwrap().psnr, f64::INFINITY);
        let mut b = a.clone();
        b.px[0] = b.px[0].wrapping_add(10); // one channel of one pixel
        let d = diff(&a, &b).unwrap();
        assert_eq!((d.max_abs, d.p99, d.differing), (10, 0, 1));
        assert!((d.mean_abs - 10.0 / 400.0).abs() < 1e-12);
        let mse: f64 = 100.0 / 400.0;
        assert!((d.psnr - 10.0 * (255.0f64 * 255.0 / mse).log10()).abs() < 1e-9);
        assert!(Tolerance::RENDER.accepts(&d));
        assert!(!Tolerance::EXACT.accepts(&d));
        assert!(diff(&a, &ramp(10, 11, 255)).is_err());
    }

    #[test]
    fn check_golden_accepts_within_tolerance_and_reports_failures() {
        if blessing() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("filmcraft-testkit-golden-{}", std::process::id()));
        let path = dir.join("ramp.png");
        let img = ramp(32, 16, 255);
        write_png(&path, &img).unwrap();
        assert!(check_golden(&path, &img, Tolerance::EXACT, "t", "unit test").is_ok());
        let mut near = img.clone();
        near.px[5] += 1;
        assert!(check_golden(&path, &near, Tolerance::RENDER, "t", "unit test").is_ok());
        let mut far = img.clone();
        far.px.iter_mut().step_by(4).for_each(|v| *v = 255 - *v);
        let e = check_golden(&path, &far, Tolerance::RENDER, "t", "unit test").unwrap_err();
        assert!(e.contains("PSNR"), "{e}");
        assert!(check_golden(&dir.join("missing.png"), &img, Tolerance::RENDER, "t", "unit test").unwrap_err().contains("FILMCRAFT_BLESS"));
    }

    #[test]
    fn date_and_sidecar_fields() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert!(t.starts_with("20"));
        let s = sidecar_text("x.png", "X", "test y");
        for f in ["asset:", "title:", "author:", "source:", "license:", "added:"] {
            assert!(s.lines().any(|l| l.starts_with(f) && l.len() > f.len() + 2), "{f}");
        }
    }
}
