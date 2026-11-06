//! Transcript-independent silence detection for decoded interleaved audio.
//! The returned ranges are sample-frame offsets, suitable for mapping to timeline ticks.

/// A half-open interval of silent audio frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SilenceRange {
    pub start: usize,
    pub end: usize,
}

/// Detect silent regions using a peak amplitude threshold.
/// Non-finite samples are treated as active audio rather than removed.
/// Invalid settings produce no cuts.
pub fn detect_silence(
    interleaved: &[f32],
    channels: usize,
    sample_rate: u32,
    threshold_dbfs: f32,
    min_seconds: f64,
    padding_seconds: f64,
) -> Vec<SilenceRange> {
    if channels == 0
        || sample_rate == 0
        || !threshold_dbfs.is_finite()
        || threshold_dbfs > 0.0
        || !min_seconds.is_finite()
        || min_seconds < 0.0
        || !padding_seconds.is_finite()
        || padding_seconds < 0.0
        || !interleaved.len().is_multiple_of(channels)
    {
        return Vec::new();
    }
    let rate = f64::from(sample_rate);
    let min_frames = (min_seconds * rate).ceil();
    let pad_frames = (padding_seconds * rate).ceil();
    if !min_frames.is_finite() || !pad_frames.is_finite() || min_frames > usize::MAX as f64 || pad_frames > usize::MAX as f64 {
        return Vec::new();
    }
    let minimum = min_frames as usize;
    let padding = pad_frames as usize;
    let threshold = 10.0_f32.powf(threshold_dbfs / 20.0);
    let frames = interleaved.len() / channels;
    let mut result = Vec::new();
    let mut start = None;
    for frame in 0..frames {
        let from = frame * channels;
        let quiet = interleaved[from..from + channels].iter().all(|sample| sample.is_finite() && sample.abs() <= threshold);
        if quiet {
            if start.is_none() {
                start = Some(frame);
            }
        } else if let Some(begin) = start.take() {
            add_range(&mut result, begin, frame, minimum, padding);
        }
    }
    if let Some(begin) = start {
        add_range(&mut result, begin, frames, minimum, padding);
    }
    result
}

fn add_range(result: &mut Vec<SilenceRange>, start: usize, end: usize, minimum: usize, padding: usize) {
    if end.saturating_sub(start) < minimum {
        return;
    }
    let cut_start = start.saturating_add(padding);
    let cut_end = end.saturating_sub(padding);
    if cut_start < cut_end {
        result.push(SilenceRange { start: cut_start, end: cut_end });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_pads_stereo_silence() {
        let mut samples = vec![0.5; 20];
        samples[4..16].fill(0.0);
        assert_eq!(detect_silence(&samples, 2, 10, -35.0, 0.4, 0.1), vec![SilenceRange { start: 3, end: 7 }]);
    }

    #[test]
    fn respects_minimum_duration() {
        let samples = [0.8, 0.0, 0.0, 0.8];
        assert!(detect_silence(&samples, 1, 10, -40.0, 0.3, 0.0).is_empty());
    }

    #[test]
    fn rejects_invalid_settings_and_nonfinite_audio() {
        assert!(detect_silence(&[0.0], 0, 10, -40.0, 0.0, 0.0).is_empty());
        assert!(detect_silence(&[0.0], 1, 0, -40.0, 0.0, 0.0).is_empty());
        assert!(detect_silence(&[0.0], 1, 10, f32::NAN, 0.0, 0.0).is_empty());
        assert!(detect_silence(&[0.0], 1, 10, -40.0, -1.0, 0.0).is_empty());
        assert!(detect_silence(&[f32::NAN], 1, 10, -40.0, 0.0, 0.0).is_empty());
    }
}
