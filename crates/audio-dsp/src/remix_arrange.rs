use super::{Analysis, MIN_PIECE_BEATS, Params, Piece, Plan, RemixError, XFADE_S, joints_for};

#[derive(Clone)]
struct State {
    pieces: Vec<Piece>,
    src: i64,
    out: i64,
    score: f64,
}

/// Bounded beam search over alternative arrangements, rather than committing
/// to the first locally attractive joint. Relative beat phase is a preference,
/// not an assumption about the track's time signature.
pub(super) fn arrange(a: &Analysis, target: i64, params: Params) -> Result<Plan, RemixError> {
    arrange_with_threshold(a, target, params, None)
}

pub(super) fn arrange_with_threshold(a: &Analysis, target: i64, params: Params, melody_floor: Option<f32>) -> Result<Plan, RemixError> {
    let detailed = match search(a, target, params, None, melody_floor) {
        Ok(plan) => plan,
        Err(RemixError::TargetTooShort) if (34.0..68.0).contains(&params.segments) => return search(a, target, params, Some(1), melody_floor),
        Err(error) => return Err(error),
    };
    // At the middle Segments setting, an extra cut must improve compatibility
    // enough to justify its perceptual cost. High Segments still requests many edits.
    if (34.0..68.0).contains(&params.segments)
        && detailed.pieces.len() == 3
        && let Ok(simple) = search(a, target, params, Some(1), melody_floor)
    {
        let quality = |plan: &Plan| {
            let cuts = plan.cuts();
            let score: f64 = cuts
                .iter()
                .map(|&(out, input)| {
                    let i = a.beats.binary_search(&out).unwrap_or(0);
                    let j = a.beats.binary_search(&input).unwrap_or(0);
                    f64::from(a.jump_score(i, j))
                })
                .sum();
            score / cuts.len().max(1) as f64 - 0.10 * cuts.len() as f64 - 0.12 * plan.len().abs_diff(target) as f64 / a.period
        };
        if quality(&simple) >= quality(&detailed) {
            return Ok(simple);
        }
    }
    Ok(detailed)
}

fn search(a: &Analysis, target: i64, params: Params, joint_override: Option<usize>, melody_floor: Option<f32>) -> Result<Plan, RemixError> {
    let m = a.beats.len();
    if a.sample_rate == 0
        || a.len <= 0
        || target <= 0
        || !a.period.is_finite()
        || a.period < 1.0
        || !params.segments.is_finite()
        || !params.variations.is_finite()
        || !(2 * MIN_PIECE_BEATS + 2..=4096).contains(&m)
        || m.checked_mul(m) != Some(a.sim.len())
        || a.sim.iter().any(|x| !x.is_finite())
        || a.melody_sim.len() != a.sim.len()
        || a.melody_sim.iter().any(|x| !x.is_finite())
        || a.melody_present.len() != m
        || a.levels.len() != m
        || a.levels.iter().any(|x| !x.is_finite() || *x < 0.0)
        || a.beats.first().is_none_or(|b| *b < 0)
        || a.beats.last().is_none_or(|b| *b >= a.len)
        || a.beats.windows(2).any(|w| w[0] >= w[1])
        || a.len > i64::MAX / 1024
        || target > a.len.saturating_mul(16)
    {
        return Err(RemixError::InvalidInput);
    }
    let xfade = (XFADE_S * f64::from(a.sample_rate)).round() as i64;
    let tolerance = (a.period / 2.0).round().max(1.0) as i64;
    if target.abs_diff(a.len) <= tolerance as u64 {
        return Ok(Plan::identity(a.len, xfade));
    }
    let minimum = (MIN_PIECE_BEATS as f64 * a.period).ceil() as i64;
    if target < minimum.saturating_mul(2) || a.len < minimum.saturating_mul(2) {
        return Err(RemixError::TargetTooShort);
    }
    let delta = a.len - target;
    // Protect a closing phrase, rather than just the final four beats. Every
    // non-final piece stops before this suffix, so it can occur only once.
    let outro_length = (16.0 * a.period).min(a.len as f64 * 0.35).max(minimum as f64) as i64;
    let outro_start = a.len - outro_length;
    let mut joints = joint_override.unwrap_or_else(|| joints_for(params.segments)).min((delta.unsigned_abs() as f64 / a.period).round().max(1.0) as usize);
    while joints > 1 && target / (joints as i64 + 1) < minimum {
        joints -= 1;
    }
    if delta < 0 {
        let max_span = (outro_start - minimum).max(1);
        let required = ((-delta) as f64 / max_span as f64).ceil() as usize;
        joints = joints.max(required);
    }
    if joints > 32 || target / (joints as i64 + 1) < minimum {
        return Err(RemixError::TargetTooShort);
    }
    let min_jump = MIN_PIECE_BEATS.min((delta.unsigned_abs() as f64 / a.period / joints as f64).floor().max(1.0) as usize);
    let window = 1 + (7.0 * params.variations.clamp(0.0, 100.0) / 100.0).round() as usize;
    let mut beam = vec![State { pieces: Vec::new(), src: 0, out: 0, score: 0.0 }];
    for step in 0..joints {
        let remaining = (joints - step) as i64;
        let last = remaining == 1;
        let mut next = Vec::new();
        for state in &beam {
            let out_left = target - state.out;
            let desired_piece = out_left / (remaining + 1);
            let span = ((a.len - state.src) - out_left) / remaining;
            // Search all outgoing beats. Incoming candidates stay near the
            // required jump length, bounding work to O(beam * beats * window).
            for (i, &outgoing) in a.beats.iter().enumerate() {
                let piece_len = outgoing - state.src;
                if piece_len < minimum || outgoing > outro_start || out_left - piece_len < minimum * remaining {
                    continue;
                }
                let expected = outgoing.saturating_add(span);
                let nearest = a.beats.partition_point(|&v| v < expected).min(m - 1);
                for j in nearest.saturating_sub(window)..=nearest.saturating_add(window).min(m - 1) {
                    let Some(&incoming) = a.beats.get(j) else { continue };
                    if i.abs_diff(j) < min_jump || (incoming > outgoing) != (delta > 0) || incoming < minimum || incoming > outro_start {
                        continue;
                    }
                    let new_out = state.out + piece_len;
                    let residual = (a.len - incoming) - (target - new_out);
                    if last {
                        if residual.unsigned_abs() > (tolerance * 2) as u64 {
                            continue;
                        }
                    } else if residual.signum() != delta.signum() || (residual.unsigned_abs() as f64) < (remaining - 1) as f64 * a.period * 0.75 {
                        continue;
                    }
                    if melody_floor.is_some_and(|floor| !a.melody_compatible(i, j, floor)) {
                        continue;
                    }
                    let phase_penalty = if i.abs_diff(j).is_multiple_of(4) { 0.0 } else { 0.06 };
                    let spacing_penalty = 0.015 * (piece_len - desired_piece).unsigned_abs() as f64 / a.len as f64;
                    let timing_penalty = if last { 0.12 * residual.unsigned_abs() as f64 / a.period } else { 0.0 };
                    let score = state.score + f64::from(a.jump_score(i, j)) - phase_penalty - spacing_penalty - timing_penalty;
                    let mut pieces = state.pieces.clone();
                    pieces.push(Piece { src: state.src, len: piece_len });
                    next.push(State { pieces, src: incoming, out: new_out, score });
                    // Periodic pruning caps memory even for long source tracks.
                    if next.len() >= 256 {
                        prune(&mut next);
                    }
                }
            }
        }
        prune(&mut next);
        if next.is_empty() {
            return Err(RemixError::TargetTooShort);
        }
        beam = next;
    }
    let Some(mut winner) = beam.into_iter().next() else { return Err(RemixError::TargetTooShort) };
    winner.pieces.push(Piece { src: winner.src, len: a.len - winner.src });
    Ok(Plan { pieces: winner.pieces, xfade })
}

fn prune(states: &mut Vec<State>) {
    states.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.src.cmp(&b.src)).then(a.out.cmp(&b.out)));
    let mut seen = std::collections::BTreeSet::new();
    states.retain(|state| seen.insert((state.src, state.out)));
    states.truncate(16);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Analysis {
        let m = 64;
        Analysis {
            sample_rate: 1000,
            len: 32_413,
            beats: (0..m).map(|i| 200 + i as i64 * 500).collect(),
            period: 500.0,
            bpm: 120.0,
            sim: vec![0.8; m * m],
            levels: vec![0.1; m],
            melody_sim: vec![0.0; m * m],
            melody_present: vec![false; m],
        }
    }

    #[test]
    fn shortening_and_extension_rearrange_instead_of_trimming() {
        let a = grid();
        for target in [26_000, 46_000] {
            let p = arrange(&a, target, Params::default()).unwrap();
            assert!(p.len().abs_diff(target) <= 500);
            assert!(p.pieces.len() > 1);
            assert_eq!(p.pieces[0].src, 0);
            let tail = p.pieces.last().unwrap();
            assert_eq!(tail.src + tail.len, a.len);
            for (outgoing, incoming) in p.cuts() {
                assert_eq!(outgoing < incoming, target < a.len);
                assert!(a.beats.contains(&outgoing) && a.beats.contains(&incoming));
                assert!(outgoing.abs_diff(incoming) >= 2000);
            }
        }
    }

    #[test]
    fn malformed_analysis_is_rejected_without_panicking() {
        let a = grid();
        let mut bad = a.clone();
        bad.sim.clear();
        assert_eq!(arrange(&bad, 26_000, Params::default()), Err(RemixError::InvalidInput));
        bad = a.clone();
        bad.beats[1] = bad.beats[0];
        assert_eq!(arrange(&bad, 26_000, Params::default()), Err(RemixError::InvalidInput));
        for period in [0.0, f64::NAN, f64::INFINITY] {
            bad = a.clone();
            bad.period = period;
            assert_eq!(arrange(&bad, 26_000, Params::default()), Err(RemixError::InvalidInput));
        }
        for target in [i64::MIN, -1, 0, i64::MAX] {
            assert_eq!(arrange(&a, target, Params::default()), Err(RemixError::InvalidInput));
        }
        assert_eq!(arrange(&a, 26_000, Params { segments: f64::NAN, variations: 50.0 }), Err(RemixError::InvalidInput));
    }
    #[test]
    fn closing_phrase_occurs_only_once() {
        let a = grid();
        let closing_start = a.len - (16.0 * a.period).min(a.len as f64 * 0.35) as i64;
        for target in [16_000, 46_000, 70_000] {
            let plan = arrange(&a, target, Params::default()).unwrap();
            let played_tail: i64 = plan.pieces.iter().map(|p| (p.src + p.len - closing_start.max(p.src)).max(0)).sum();
            assert_eq!(played_tail, a.len - closing_start, "closing phrase repeated at target {target}");
            assert!(plan.pieces.iter().take(plan.pieces.len() - 1).all(|p| p.src + p.len <= closing_start));
        }
    }

    #[test]
    fn similar_spectra_do_not_hide_a_large_volume_jump() {
        let mut a = grid();
        a.levels.fill(0.2);
        a.levels[24] = 0.02;
        a.levels[25] = 0.02;
        assert!(a.jump_score(12, 32) > a.jump_score(12, 24) + 0.2);
    }

    #[test]
    fn default_segments_can_use_a_single_good_cut() {
        let a = grid();
        assert_eq!(arrange(&a, 16_000, Params::default()).unwrap().pieces.len(), 2);
    }
    #[test]
    fn short_targets_can_fall_back_to_a_single_cut() {
        let (music, _) = super::super::test_music(48_000, 120.0, 16, 3);
        let channels: Vec<_> = music.iter().map(Vec::as_slice).collect();
        let a = super::super::analyze(&channels, 48_000).unwrap();
        let plan = arrange(&a, 12 * 48_000, Params::default()).unwrap();
        assert!(plan.len().abs_diff(12 * 48_000) <= a.period as u64);
    }
    #[test]
    fn strict_matching_rejects_different_notes_and_offers_unchanged_tempo() {
        let mut a = grid();
        a.melody_present.fill(true);
        a.melody_sim.fill(0.1);
        assert!(super::super::plan_with_melody_floor(&a, 16_000, Params::default(), 0.5).is_err());
        let alternatives = super::super::melodic_alternatives(&a, 16_000);
        assert_eq!(alternatives.len(), 1);
        assert_eq!(alternatives[0].len(), a.len);
        a.melody_sim.fill(0.8);
        let alternatives = super::super::melodic_alternatives(&a, 16_000);
        assert!(!alternatives.is_empty());
        for plan in alternatives {
            let applied = super::super::plan_with_melody_floor(&a, plan.len(), Params::default(), 0.5).unwrap();
            assert_eq!(applied.len(), plan.len());
            assert_eq!(applied.pieces.first().unwrap().src, 0);
            let tail = applied.pieces.last().unwrap();
            assert_eq!(tail.src + tail.len, a.len);
        }
        a.melody_present.fill(false);
        assert!(super::super::plan_with_melody_floor(&a, 16_000, Params::default(), 0.5).is_err());
    }
    #[test]
    fn local_phrase_matches_can_bridge_context_variation_but_not_arbitrary_notes() {
        let mut a = grid();
        let m = a.beats.len();
        a.melody_present.fill(true);
        a.melody_sim.fill(0.36);
        for (i, j) in [(7, 39), (8, 40), (7, 40), (8, 41)] {
            a.melody_sim[i * m + j] = 0.8;
        }
        assert!(a.melody_score(8, 40).unwrap() < 0.5);
        assert!(a.melody_compatible(8, 40, 0.5));
        assert!(!a.melody_compatible(8, 41, 0.5), "different bar phase needs a strong full-context match");
        a.sim.fill(0.4);
        assert!(!a.melody_compatible(8, 40, 0.5), "local notes alone cannot override different harmony/timbre");
        a.sim.fill(0.8);
        a.melody_sim.fill(0.1);
        for (i, j) in [(7, 39), (8, 40)] {
            a.melody_sim[i * m + j] = 0.8;
        }
        assert!(!a.melody_compatible(8, 40, 0.5), "a brief coincidence cannot override incompatible context");
    }

    #[test]
    fn short_duration_choices_can_combine_multiple_nonoverlapping_edits() {
        let mut a = grid();
        let m = a.beats.len();
        a.melody_present.fill(true);
        for (i, j) in [(8usize, 24usize), (36, 44)] {
            for k in -4isize..4 {
                let x = i.checked_add_signed(k).unwrap();
                let y = j.checked_add_signed(k).unwrap();
                a.melody_sim[x * m + y] = 0.9;
            }
        }
        let target = a.len - 12_000;
        let choices = super::super::melodic_alternatives(&a, target);
        assert!(choices[0].len().abs_diff(target) <= a.period as u64, "{choices:?}");
        assert!(choices[0].cuts().len() >= 2);
        for choice in choices {
            let applied = super::super::plan_with_melody_floor(&a, choice.len(), Params::default(), 0.5).unwrap();
            assert!(applied.len().abs_diff(choice.len()) <= a.period as u64);
            assert_eq!(applied.pieces[0].src, 0);
            let tail = applied.pieces.last().unwrap();
            assert_eq!(tail.src + tail.len, a.len);
        }
    }
}
