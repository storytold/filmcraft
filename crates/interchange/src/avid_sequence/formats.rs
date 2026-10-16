//! Sequence raster and edit-clock interpretation. Format labels corroborate clocks;
//! source media and motion-adapter input clocks remain in their own domains.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Clock {
    recorded: FrameRate,
    pub actual: FrameRate,
}
impl Clock {
    pub fn rate(self, v: &Value) -> Result<FrameRate> {
        let r = rate(v)?;
        Ok(if r == self.recorded { self.actual } else { r })
    }
    pub fn ticks(self, v: &Value, key: &str) -> Result<Tick> {
        ticks_at(v, key, self.rate(v)?)
    }
}
pub(super) struct Format {
    pub settings: SequenceSettings,
    pub clock: Clock,
    pub clock_conflict: bool,
    pub warnings: Vec<String>,
}
fn label_rate(label: &str) -> Option<FrameRate> {
    Some(match label.trim_end_matches('p') {
        "23.976" => FrameRate::new(24000, 1001),
        "29.97" => FrameRate::new(30000, 1001),
        "47.952" | "47.95" => FrameRate::new(48000, 1001),
        "59.94" => FrameRate::new(60000, 1001),
        "119.88" => FrameRate::new(120000, 1001),
        "24" => FrameRate::FPS_24,
        "25" => FrameRate::new(25, 1),
        "30" => FrameRate::new(30, 1),
        "48" => FrameRate::new(48, 1),
        "50" => FrameRate::new(50, 1),
        "60" => FrameRate::new(60, 1),
        "120" => FrameRate::new(120, 1),
        other => {
            let n = other.parse::<i64>().ok().filter(|n| (1..=1000).contains(n))?;
            FrameRate::new(n, 1)
        }
    })
}
/// Explicit dimensions avoid conflating the several DCI/2K/4K apertures.
fn raster(label: &str) -> Option<(u32, u32)> {
    match label {
        "HD 1080p" | "1080p" | "HD 1080i" | "1080i" => return Some((1920, 1080)),
        "HD 720p" | "720p" => return Some((1280, 720)),
        "UHD" | "UHD 2160p" | "2160p" => return Some((3840, 2160)),
        _ => {}
    }
    let explicit = ["Custom ", "UHD ", "4K ", "2K "].iter().find_map(|p| label.strip_prefix(p)).unwrap_or(label);
    let (w, h) = explicit.strip_suffix('p')?.split_once('x')?;
    let size = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    filmcraft_project::validate_frame_size(size.0, size.1).ok()?;
    Some(size)
}
pub(super) fn resolve(m: &Value, best_effort: bool) -> Result<Format> {
    let recorded = rate(m)?;
    let label = m.pointer("/attributes/SEQUERNCE_FORMAT_STRING").and_then(Value::as_str).filter(|s| s.len() <= 256 && !s.contains('\0'));
    let parts = label.and_then(|s| s.rsplit_once('/'));
    let mut actual = recorded;
    let mut clock_conflict = false;
    let mut warnings = Vec::new();
    let mut size = None;
    if let Some((format, suffix)) = parts {
        size = raster(format.trim());
        if let Some(mut expected) = label_rate(suffix.trim()) {
            if matches!(format.trim(), "HD 1080i" | "1080i") {
                // The label names fields per second; the composition counts frames.
                expected = FrameRate::new(expected.num, expected.den * 2);
                if !best_effort {
                    return Err(bad("interlaced sequence field handling is not supported"));
                }
                warnings.push(
                    "Interlaced sequence imported into a progressive timeline at its frame rate; field order and field rendering are not transferred.".into(),
                );
            }
            if !avid_catalog::picture_rate_matches(m, expected) {
                clock_conflict = true;
                if !best_effort {
                    return Err(bad("sequence format label conflicts with recorded edit rate"));
                }
                warnings.push(
                    "Sequence format label conflicts with its recorded edit rate; retaining the recorded clock without substitution. Check sequence settings."
                        .into(),
                );
            } else {
                actual = expected;
            }
        } else if !best_effort {
            return Err(bad("sequence format has an unsupported frame-rate label"));
        } else {
            warnings.push("Sequence frame-rate label is unrecognized; using the recorded rational edit rate without substitution.".into());
        }
    }
    let (width, height) = match size {
        Some(size) => size,
        None if best_effort => {
            warnings.push("Sequence raster could not be established from its format label; using 1920×1080 square pixels. Check sequence settings; original format is retained.".into());
            (1920, 1080)
        }
        None => return Err(bad("sequence raster is unknown or unsupported")),
    };
    if actual != recorded {
        warnings.push(format!(
            "Sequence format label corroborates {}/{} fps for the recorded {}/{} edit rate. Original rates/counts are retained.",
            actual.num, actual.den, recorded.num, recorded.den
        ));
    }
    let settings = SequenceSettings { width, height, frame_rate: actual, preset: label.unwrap_or("Avid sequence").into(), ..Default::default() };
    settings.validate().map_err(bad)?;
    Ok(Format { settings, clock: Clock { recorded, actual }, clock_conflict, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mob(label: &str, n: i64, d: i64) -> Value {
        json!({"attributes":{"SEQUERNCE_FORMAT_STRING":label},"rate":{"numerator":n,"denominator":d}})
    }
    #[test]
    fn formats_and_clocks_are_independent_and_exact() {
        for (label, recorded, expected, size) in [
            ("HD 720p/25", (25, 1), (25, 1), (1280, 720)),
            ("HD 1080p/23.976", (23976, 1000), (24000, 1001), (1920, 1080)),
            ("HD 1080p/29.97", (30000, 1001), (30000, 1001), (1920, 1080)),
            ("UHD/59.94", (5994, 100), (60000, 1001), (3840, 2160)),
            ("2160p/25", (25, 1), (25, 1), (3840, 2160)),
            ("4K 4096x2160p/24", (24, 1), (24, 1), (4096, 2160)),
            ("4K 4096x1716p/24", (24, 1), (24, 1), (4096, 1716)),
            ("Custom 2048x858p/25", (25, 1), (25, 1), (2048, 858)),
        ] {
            let f = resolve(&mob(label, recorded.0, recorded.1), false).unwrap();
            assert_eq!((f.settings.width, f.settings.height), size);
            assert_eq!(f.clock.actual, FrameRate::new(expected.0, expected.1));
            let node = json!({"length":108000,"rate":{"numerator":recorded.0,"denominator":recorded.1}});
            assert_eq!(f.clock.ticks(&node, "length").unwrap(), f.clock.actual.tick_of(108000));
            // A differently clocked media input is never replaced with the timeline clock.
            let source = json!({"rate":{"numerator":30,"denominator":1}});
            assert_eq!(f.clock.rate(&source).unwrap(), FrameRate::new(30, 1));
        }
    }
    #[test]
    fn unknown_conflicting_and_hostile_formats_are_explicit() {
        for label in ["4K/24", "unknown/24", "Custom 0x1080p/24", "Custom 999999999999x1080p/24", "Custom 32768x32768p/24"] {
            let m = mob(label, 24, 1);
            assert!(resolve(&m, false).is_err());
            assert!(resolve(&m, true).unwrap().warnings.iter().any(|w| w.contains("raster")));
        }
        let conflict = mob("HD 1080p/24", 25, 1);
        assert!(resolve(&conflict, false).is_err());
        let f = resolve(&conflict, true).unwrap();
        assert_eq!(f.clock.actual, FrameRate::new(25, 1));
        assert!(f.warnings.iter().any(|w| w.contains("conflicts")));
        for (n, d) in [(0, 1), (-24, 1), (24, 0), (i64::MAX, 1)] {
            assert!(std::panic::catch_unwind(|| resolve(&mob("HD 1080p/24", n, d), true)).unwrap().is_err());
        }
        let f = resolve(&mob("HD 1080i/59.94", 2997, 100), true).unwrap();
        assert_eq!(f.clock.actual, FrameRate::FPS_29_97);
        assert!(f.warnings.iter().any(|w| w.contains("field order")));
        assert!(resolve(&mob("HD 1080i/59.94", 2997, 100), false).is_err());
        let conflict = resolve(&mob("HD 1080i/59.94", 5994, 100), true).unwrap();
        assert_eq!(conflict.clock.actual, FrameRate::new(5994, 100));
        assert!(conflict.warnings.iter().any(|w| w.contains("conflicts")));
    }
}
