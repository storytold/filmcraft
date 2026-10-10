//! Splitting a clip into independent regions, and skipping silence.
//!
//! Whisper reads the audio in 30-second windows, each starting where the previous one's last
//! complete segment ended. That chain is sequential, so a long clip is first cut into regions
//! (about [`REGION`] long) at its longest pauses near evenly spaced points; every region is
//! transcribed with the usual sequential procedure, and the regions run side by side, so a
//! decoding step serves one window of every region at once. A window never reads audio past its
//! region's end. Silence of 1.5 s or more at the start of a window is skipped (0.2 s of it kept),
//! so it costs no model time. Pauses come from the energy detector of [`crate::vad`].

/// Frames (10 ms) per window.
pub const WINDOW: usize = super::model::N_FRAMES;
/// Target region length in frames (90 s).
pub const REGION: usize = 9000;
/// Silence this long (1.5 s) at the start of a window is skipped.
const SKIP: usize = 150;
/// Silence kept before speech when skipping (0.2 s).
const MARGIN: usize = 20;
/// How far (frames) a region boundary may move from its evenly spaced position to find a pause.
const SEARCH: usize = 1500;

/// Cut `frames` mel frames into `n` regions (`start..end`, in order, covering everything) at the
/// longest silence near every `i · frames / n` (else at the quietest moment there). `silent[i]`
/// flags the 10 ms frame `i` (frames beyond it count as speech); `db` holds frame levels.
pub fn regions(frames: usize, silent: &[bool], db: &[f32], n: usize) -> Vec<(usize, usize)> {
    let n = n.clamp(1, frames.max(1));
    let sil = |i: usize| silent.get(i).copied().unwrap_or(false);
    let mut cuts = vec![0];
    for k in 1..n {
        let target = frames / n * k;
        let (lo, hi) = (target.saturating_sub(SEARCH).max(cuts.last().map_or(0, |c| c + WINDOW / 3)), (target + SEARCH).min(frames));
        if lo >= hi {
            continue;
        }
        // the longest run of silence in lo..hi (cut in its middle)
        let mut best: Option<(usize, usize)> = None;
        let mut i = lo;
        while i < hi {
            if sil(i) {
                let s = i;
                while i < hi && sil(i) {
                    i += 1;
                }
                if best.is_none_or(|b| i - s > b.1 - b.0) {
                    best = Some((s, i));
                }
            } else {
                i += 1;
            }
        }
        let cut = match best {
            Some((a, b)) if b - a >= 10 => a + (b - a) / 2,
            _ => quietest(db, lo, hi),
        };
        if cut > *cuts.last().unwrap_or(&0) && cut < frames {
            cuts.push(cut);
        }
    }
    cuts.push(frames);
    cuts.windows(2).filter(|w| w[0] < w[1]).map(|w| (w[0], w[1])).collect()
}

/// Where decoding should resume at or after `p` (before `end`): past any silence of 1.5 s or more,
/// keeping 0.2 s of it. Returns `end` when only silence is left.
pub fn skip_silence(p: usize, end: usize, silent: &[bool]) -> usize {
    let sil = |i: usize| silent.get(i).copied().unwrap_or(false);
    let mut e = p;
    while e < end && sil(e) {
        e += 1;
    }
    if e >= end {
        return if e - p >= SKIP { end } else { p };
    }
    if e - p >= SKIP { e - MARGIN } else { p }
}

/// The frame in `from..to` with the lowest level (averaged over ±2 frames; the latest on ties).
fn quietest(db: &[f32], from: usize, to: usize) -> usize {
    let level = |i: usize| -> f32 {
        let (a, b) = (i.saturating_sub(2), (i + 3).min(db.len()));
        db.get(a..b).filter(|s| !s.is_empty()).map(|s| s.iter().sum::<f32>() / s.len() as f32).unwrap_or(0.0)
    };
    (from..to).max_by(|&a, &b| level(b).total_cmp(&level(a))).unwrap_or(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covers(r: &[(usize, usize)], frames: usize) {
        assert_eq!(r.first().map(|r| r.0), Some(0));
        assert_eq!(r.last().map(|r| r.1), Some(frames));
        for w in r.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        assert!(r.iter().all(|r| r.0 < r.1));
    }

    #[test]
    fn regions_end_at_the_longest_nearby_pause() {
        let frames = 30_000;
        let mut s = vec![false; frames];
        // around the 1/3 point (10 000): a short pause at 9 500, a long one at 10 800
        for i in (9_500..9_530).chain(10_800..10_900).chain(19_000..19_040) {
            s[i] = true;
        }
        let db: Vec<f32> = s.iter().map(|&x| if x { -70.0 } else { -20.0 }).collect();
        let r = regions(frames, &s, &db, 3);
        covers(&r, frames);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].1, 10_850);
        assert_eq!(r[1].1, 19_020);
        // one region: the whole clip
        assert_eq!(regions(frames, &s, &db, 1), vec![(0, frames)]);
    }

    #[test]
    fn silence_is_skipped_with_a_margin() {
        let mut s = vec![false; 1000];
        for v in &mut s[100..400] {
            *v = true;
        }
        assert_eq!(skip_silence(100, 1000, &s), 380);
        assert_eq!(skip_silence(300, 1000, &s), 300); // only 1 s left of it
        assert_eq!(skip_silence(0, 1000, &s), 0);
        assert_eq!(skip_silence(100, 400, &s), 400); // nothing but silence to the end
        assert_eq!(skip_silence(390, 400, &s), 390);
    }

    #[test]
    fn hostile_inputs() {
        assert_eq!(regions(0, &[], &[], 4), vec![]);
        covers(&regions(5, &[], &[], 100), 5);
        covers(&regions(50_000, &[true; 10], &[], 7), 50_000);
        covers(&regions(50_000, &vec![true; 90_000], &[-90.0; 3], 16), 50_000);
        assert_eq!(skip_silence(10, 5, &[]), 10);
    }
}
