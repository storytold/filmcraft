//! Word timing from cross-attention: token-to-audio alignment by dynamic time warping over the
//! averaged, normalised attention of the alignment heads (see the module docs of `whisper`).

/// Start/end (in mel frames from the window start, 10 ms) of each of `n_text` text tokens, from
/// the cross-attention logits of the alignment heads (each `tokens × n_ctx`, row-major) of a pass
/// over `prompt ‖ text ‖ <|endoftext|>`; `first` is the row that predicts the first text token.
pub fn align(qk: &[Vec<f32>], tokens: usize, n_ctx: usize, first: usize, n_text: usize, seg_frames: usize) -> Vec<(usize, usize)> {
    let h = qk.len();
    let rows = n_text + 1;
    let f = (seg_frames / 2).clamp(1, n_ctx.max(1));
    if h == 0 || n_ctx == 0 || first + rows > tokens || qk.iter().any(|m| m.len() < tokens * n_ctx) {
        return vec![(0, 0); n_text];
    }
    let mut matrix = vec![0f32; rows * f];
    for head in qk {
        // softmax over the window's audio frames, row by row
        let mut w = vec![0f32; tokens * f];
        for (i, r) in w.chunks_mut(f).enumerate() {
            r.copy_from_slice(&head[i * n_ctx..i * n_ctx + f]);
            crate::nn::softmax(r);
        }
        // standardise over tokens for every frame
        let mut z = vec![0f32; tokens * f];
        for j in 0..f {
            let mean = (0..tokens).map(|i| w[i * f + j]).sum::<f32>() / tokens as f32;
            let var = (0..tokens).map(|i| (w[i * f + j] - mean).powi(2)).sum::<f32>() / tokens as f32;
            let sd = var.sqrt().max(1e-8);
            for i in 0..tokens {
                z[i * f + j] = (w[i * f + j] - mean) / sd;
            }
        }
        for r in 0..rows {
            let row = median_filter(&z[(first + r) * f..(first + r + 1) * f], 7);
            for j in 0..f {
                matrix[r * f + j] += row[j] / h as f32;
            }
        }
    }
    let neg: Vec<f32> = matrix.iter().map(|v| -v).collect();
    let path = dtw(&neg, rows, f);
    // the frame where each row is first reached
    let mut jump = vec![usize::MAX; rows];
    for &(i, j) in &path {
        if jump[i] == usize::MAX {
            jump[i] = j;
        }
    }
    let mut out = Vec::with_capacity(n_text);
    for i in 0..n_text {
        let s = jump[i].min(f);
        let e = jump[i + 1].min(f).max(s);
        out.push((s * 2, e * 2));
    }
    out
}
/// Median filter with reflect padding.
pub fn median_filter(x: &[f32], width: usize) -> Vec<f32> {
    let n = x.len();
    if n == 0 || width <= 1 {
        return x.to_vec();
    }
    let half = width / 2;
    let at = |i: isize| -> f32 {
        if n == 1 {
            return x[0];
        }
        let p = 2 * (n as isize - 1);
        let mut j = i.rem_euclid(p);
        if j >= n as isize {
            j = p - j;
        }
        x[j as usize]
    };
    let mut buf = vec![0f32; width];
    (0..n)
        .map(|i| {
            for (k, b) in buf.iter_mut().enumerate() {
                *b = at(i as isize + k as isize - half as isize);
            }
            buf.sort_by(f32::total_cmp);
            buf[half]
        })
        .collect()
}

/// Dynamic time warping through `cost` (`n × m`, row-major), monotonic steps (diagonal, down,
/// right). Returns the path from (0, 0) to (n − 1, m − 1).
pub fn dtw(cost: &[f32], n: usize, m: usize) -> Vec<(usize, usize)> {
    let inf = f32::INFINITY;
    let mut acc = vec![inf; (n + 1) * (m + 1)];
    let mut trace = vec![0u8; (n + 1) * (m + 1)];
    acc[0] = 0.0;
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for j in 1..=m {
        for i in 1..=n {
            let c0 = acc[idx(i - 1, j - 1)];
            let c1 = acc[idx(i - 1, j)];
            let c2 = acc[idx(i, j - 1)];
            let (c, t) = if c0 < c1 && c0 < c2 {
                (c0, 0)
            } else if c1 < c0 && c1 < c2 {
                (c1, 1)
            } else {
                (c2, 2)
            };
            acc[idx(i, j)] = cost[(i - 1) * m + (j - 1)] + c;
            trace[idx(i, j)] = t;
        }
    }
    for j in 0..=m {
        trace[idx(0, j)] = 2;
    }
    for i in 0..=n {
        trace[idx(i, 0)] = 1;
    }
    let (mut i, mut j) = (n, m);
    let mut path = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            path.push((i - 1, j - 1));
        }
        match trace[idx(i, j)] {
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_filter_removes_spikes() {
        assert_eq!(median_filter(&[0.0, 0.0, 9.0, 0.0, 0.0], 3), vec![0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(median_filter(&[1.0, 2.0, 3.0], 1), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn dtw_follows_the_cheap_diagonal_band() {
        // 3 rows × 6 columns, cheap cells at row 0: cols 0-1, row 1: cols 2-3, row 2: cols 4-5
        let mut c = vec![1.0f32; 18];
        for (r, cols) in [(0, [0, 1]), (1, [2, 3]), (2, [4, 5])] {
            for k in cols {
                c[r * 6 + k] = 0.0;
            }
        }
        let p = dtw(&c, 3, 6);
        assert_eq!(p.first(), Some(&(0, 0)));
        assert_eq!(p.last(), Some(&(2, 5)));
        let first_col = |row: usize| p.iter().find(|x| x.0 == row).unwrap().1;
        assert_eq!((first_col(0), first_col(1), first_col(2)), (0, 2, 4));
    }
}
