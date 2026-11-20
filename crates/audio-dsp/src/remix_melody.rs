//! Ordered semitone-register fingerprints. Unlike beat-averaged chroma these
//! retain note order within a beat and distinguish pitches in different octaves.
//! This describes the polyphonic mix; it is not a lead-instrument transcription.

const NOTES: usize = 48; // MIDI 48..95, C3..B6
const STEPS: usize = 4;
type PhraseBeat = [[f32; NOTES]; STEPS];

pub(super) fn similarities(magnitudes: &[Vec<f32>], sr: u32, window: usize, hop: usize, beats: &[i64], length: usize) -> (Vec<f32>, Vec<bool>) {
    let mut frames: Vec<[f32; NOTES]> = magnitudes
        .iter()
        .map(|spectrum| {
            let mut profile = [0f32; NOTES];
            for (note, value) in profile.iter_mut().enumerate() {
                let frequency = 440.0 * 2f64.powf((48.0 + note as f64 - 69.0) / 12.0);
                for (harmonic, weight) in [(1.0, 0.7), (2.0, 0.2), (3.0, 0.1)] {
                    let bin = (frequency * harmonic * window as f64 / f64::from(sr)).round() as usize;
                    if bin < 10 || bin.saturating_add(10) >= spectrum.len() {
                        continue;
                    }
                    let peak = spectrum.get(bin - 1..=bin + 1).unwrap_or(&[]).iter().copied().fold(0f32, f32::max);
                    let mut floor: Vec<f32> =
                        spectrum.get(bin - 10..bin - 3).unwrap_or(&[]).iter().chain(spectrum.get(bin + 4..=bin + 10).unwrap_or(&[])).copied().collect();
                    floor.sort_by(f32::total_cmp);
                    let background = floor.get(floor.len() / 2).copied().unwrap_or(0.0);
                    *value += weight * (peak - background).max(0.0);
                }
            }
            profile
        })
        .collect();
    // Attenuate persistent accompaniment so a changing melody contributes more
    // than a common sustained chord. Do not normalize away note order or register.
    let mut background = [0f32; NOTES];
    for frame in &frames {
        for (mean, value) in background.iter_mut().zip(frame) {
            *mean += value / frames.len().max(1) as f32;
        }
    }
    for frame in &mut frames {
        for (value, mean) in frame.iter_mut().zip(background) {
            *value = (*value - 0.8 * mean).max(0.0);
        }
    }
    let centres: Vec<i64> = (0..frames.len()).map(|i| (i * hop + window / 2) as i64).collect();
    let mut descriptors: Vec<PhraseBeat> = Vec::with_capacity(beats.len());
    let mut present = Vec::with_capacity(beats.len());
    for (index, &start) in beats.iter().enumerate() {
        let end = beats.get(index + 1).copied().unwrap_or(length as i64);
        let mut beat = [[0f32; NOTES]; STEPS];
        let mut active = 0;
        for (step, profile) in beat.iter_mut().enumerate() {
            let from = start + (end - start) * step as i64 / STEPS as i64;
            let to = start + (end - start) * (step + 1) as i64 / STEPS as i64;
            let first = centres.partition_point(|&x| x < from);
            let last = centres.partition_point(|&x| x < to);
            if first < last {
                for frame in frames.get(first..last).unwrap_or(&[]) {
                    for (value, x) in profile.iter_mut().zip(frame) {
                        *value += x / (last - first) as f32;
                    }
                }
            } else if let Some(frame) = frames.get(centres.partition_point(|&x| x < (from + to) / 2).min(frames.len().saturating_sub(1))) {
                *profile = *frame;
            }
            let norm = profile.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-5 {
                for x in profile {
                    *x /= norm;
                }
                active += 1;
            }
        }
        present.push(active >= 2);
        descriptors.push(beat);
    }
    let m = beats.len();
    let mut output = vec![0f32; m * m];
    for (i, a) in descriptors.iter().enumerate() {
        for (j, b) in descriptors.iter().enumerate() {
            let value = a
                .iter()
                .zip(b)
                .map(|(x, y)| {
                    let nx = x.iter().any(|v| *v > 0.0);
                    let ny = y.iter().any(|v| *v > 0.0);
                    if !nx && !ny { 1.0 } else { x.iter().zip(y).map(|(a, b)| a * b).sum::<f32>().clamp(0.0, 1.0) }
                })
                .sum::<f32>()
                / STEPS as f32;
            if let Some(slot) = output.get_mut(i * m + j) {
                *slot = value;
            }
        }
    }
    (output, present)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_note_order_and_octave_over_the_same_chord() {
        let sr = 12_000;
        let beat = 6000;
        let m = 32;
        let mut music = vec![0f32; beat * m];
        let notes = [72.0, 76.0, 79.0, 83.0];
        for (i, sample) in music.iter_mut().enumerate() {
            let section = i / (beat * 8);
            let subdivision = (i % beat) / (beat / 4);
            let pitch = if section == 2 { notes[3 - subdivision] } else { notes[subdivision] } + if section == 3 { 12.0 } else { 0.0 };
            let frequency = 440.0 * 2f64.powf((pitch - 69.0) / 12.0);
            let t = i as f64 / f64::from(sr);
            let local = (i % (beat / 4)) as f64 / f64::from(sr);
            let envelope = (local * 300.0).min(1.0) * ((0.125 - local) * 300.0).min(1.0);
            let lead = envelope * (0.35 * (std::f64::consts::TAU * frequency * t).sin() + 0.10 * (std::f64::consts::TAU * 2.0 * frequency * t).sin());
            let chord = [261.6256, 329.6276, 391.9954].iter().map(|f| 0.055 * (std::f64::consts::TAU * f * t).sin()).sum::<f64>();
            *sample = (lead + chord) as f32;
        }
        let beats: Vec<_> = (0..m).map(|i| (i * beat) as i64).collect();
        let magnitudes = super::super::stft_mag(&music, 2048, 256);
        let (sim, present) = similarities(&magnitudes, sr, 2048, 256, &beats, music.len());
        let same = sim[4 * m + 12];
        let reordered = sim[4 * m + 20];
        let octave = sim[4 * m + 28];
        eprintln!("same melody {same}, reversed notes {reordered}, different octave {octave}");
        assert!(present[4]);
        assert!(same > 0.95);
        assert!(same > reordered + 0.25, "note order must be distinguished even with a shared chord");
        assert!(same > octave + 0.20, "octave must not collapse to the same chroma");
    }
}
