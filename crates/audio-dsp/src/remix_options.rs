use super::{Analysis, MIN_PIECE_BEATS, Params, Piece, Plan, XFADE_S};

#[derive(Clone)]
struct State {
    pieces: Vec<Piece>,
    src: i64,
    out: i64,
    score: f32,
}

pub(super) fn alternatives(a: &Analysis, target: i64) -> Vec<Plan> {
    with_floor(a, target, 0.50)
}

pub(super) fn with_floor(a: &Analysis, target: i64, floor: f32) -> Vec<Plan> {
    if super::plan(a, a.len, Params::default()).is_err() || target <= 0 || target > a.len.saturating_mul(16) {
        return Vec::new();
    }
    let minimum = (MIN_PIECE_BEATS as f64 * a.period).ceil() as i64;
    let closing = a.len - (16.0 * a.period).min(a.len as f64 * 0.35).max(minimum as f64) as i64;
    let fade = (XFADE_S * f64::from(a.sample_rate)).round() as i64;
    let bucket = (a.period * 0.5).max(1.0);
    let rank = |(sa, a): &(f32, Plan), (sb, b): &(f32, Plan)| {
        let distance = |p: &Plan| (p.len().abs_diff(target) as f64 / bucket).round() as u64;
        distance(a).cmp(&distance(b)).then(sb.total_cmp(sa)).then(a.len().cmp(&b.len()))
    };
    let mut candidates = vec![(1.0f32, Plan::identity(a.len, fade))];
    let mut insert = |score, plan| {
        candidates.push((score, plan));
        if candidates.len() >= 256 {
            candidates.sort_by(rank);
            let mut lengths = std::collections::BTreeSet::new();
            candidates.retain(|(_, p)| lengths.insert((p.len() as f64 / bucket).round() as i64));
            candidates.truncate(128);
        }
    };
    // At most 32 destinations per outgoing beat: preserve both best matches and
    // matches near the requested skip. This caps edge storage at 4096 * 32.
    let mut edges = vec![Vec::new(); a.beats.len()];
    for (i, &outgoing) in a.beats.iter().enumerate() {
        if outgoing < minimum || outgoing > closing {
            continue;
        }
        let mut choices = Vec::new();
        for (j, &incoming) in a.beats.iter().enumerate() {
            if incoming < minimum
                || incoming > closing
                || i.abs_diff(j) < MIN_PIECE_BEATS
                || !i.abs_diff(j).is_multiple_of(4)
                || (incoming > outgoing) != (target < a.len)
                || !a.melody_compatible(i, j, floor)
            {
                continue;
            }
            let score = a.jump_score(i, j);
            if score < 0.40 {
                continue;
            }
            choices.push((incoming, score));
        }
        choices.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut keep: Vec<_> = choices.iter().take(16).copied().collect();
        choices.sort_by_key(|(input, _)| (a.len - input + outgoing).abs_diff(target));
        for choice in choices.into_iter().take(16) {
            if !keep.iter().any(|(input, _)| *input == choice.0) {
                keep.push(choice);
            }
        }
        edges[i] = keep;
    }
    if target < a.len {
        let mut beam = vec![State { pieces: Vec::new(), src: 0, out: 0, score: 0.0 }];
        for _ in 0..4 {
            let mut next = Vec::new();
            for state in &beam {
                for (i, &outgoing) in a.beats.iter().enumerate() {
                    let length = outgoing - state.src;
                    if length < minimum {
                        continue;
                    }
                    for &(incoming, score) in &edges[i] {
                        let mut pieces = state.pieces.clone();
                        pieces.push(Piece { src: state.src, len: length });
                        let out = state.out + length;
                        let mut complete = pieces.clone();
                        complete.push(Piece { src: incoming, len: a.len - incoming });
                        let quality = (state.score + score) / pieces.len() as f32 - 0.06 * (pieces.len() - 1) as f32;
                        let plan = Plan { pieces: complete, xfade: fade };
                        let length = plan.len();
                        insert(quality, plan);
                        // Further forward jumps can only shorten this arrangement.
                        if length as f64 >= target as f64 - a.period {
                            next.push(State { pieces, src: incoming, out, score: state.score + score });
                            if next.len() >= 256 {
                                prune(&mut next, a.len, target, bucket);
                            }
                        }
                    }
                }
            }
            prune(&mut next, a.len, target, bucket);
            if next.is_empty() {
                break;
            }
            beam = next;
        }
    } else {
        for (i, &outgoing) in a.beats.iter().enumerate() {
            for &(incoming, score) in &edges[i] {
                for count in 1..=8 {
                    let mut pieces = vec![Piece { src: 0, len: outgoing }];
                    for _ in 1..count {
                        pieces.push(Piece { src: incoming, len: outgoing - incoming });
                    }
                    pieces.push(Piece { src: incoming, len: a.len - incoming });
                    insert(score - 0.06 * (count - 1) as f32, Plan { pieces, xfade: fade });
                }
            }
        }
    }
    candidates.sort_by(rank);
    let mut output: Vec<Plan> = Vec::new();
    let separation = (a.period * 0.75).max(1.0) as u64;
    for (_, plan) in candidates {
        if output.iter().all(|p| p.len().abs_diff(plan.len()) >= separation) {
            output.push(plan);
        }
        if output.len() == 3 {
            break;
        }
    }
    output
}

fn prune(states: &mut Vec<State>, length: i64, target: i64, bucket: f64) {
    states.sort_by(|a, b| {
        let distance = |s: &State| ((s.out + length - s.src).abs_diff(target) as f64 / bucket).round() as u64;
        distance(a).cmp(&distance(b)).then(b.score.total_cmp(&a.score))
    });
    let mut seen = std::collections::BTreeSet::new();
    states.retain(|s| seen.insert((s.src, s.out)));
    states.truncate(64);
}
