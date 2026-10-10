//! The Token-and-Duration Transducer decoder (Xu et al. 2023, "Efficient Sequence Transduction by
//! Jointly Predicting Tokens and Durations"), decoded greedily.
//!
//! - **Prediction network**: token embedding (the blank id doubles as the start symbol and embeds
//!   to the zero row) and a 2-layer LSTM (`pred_hidden` 640). The embedding is folded into the
//!   first layer's input weights at load time (one table row per token).
//! - **Joint network**: `relu(enc(f_t) + pred(g_u))` → one linear layer giving `V + 1` token logits
//!   (the last one is blank) followed by one logit per allowed duration (0…4 frames).
//! - **Greedy decoding** (the label-looping procedure of the TDT paper): at frame `t` take the best
//!   token and the best duration `d`. A blank advances `t` by `max(d, 1)` and keeps the predictor
//!   state. A token is emitted at frame `t` with duration `d`, the predictor consumes it, and `t`
//!   advances by `d`; after `max_symbols` tokens on one frame, `t` advances by one. After
//!   [`RESET_AFTER_FRAMES`] frames without a token the prediction network restarts from the
//!   start symbol (a long-form safeguard, see the constant).
//!
//! The matrix–vector products run on the CPU in plain Rust (the decoder is sequential and small);
//! the encoder projection of all frames is one matrix product.

/// A dense row-major matrix.
pub struct Mat {
    pub rows: usize,
    pub cols: usize,
    pub w: Vec<f32>,
}

impl Mat {
    pub fn new(rows: usize, cols: usize, w: Vec<f32>) -> Option<Self> {
        (rows.checked_mul(cols)? == w.len()).then_some(Self { rows, cols, w })
    }

    /// `out += self · x`, rows split across threads for large matrices.
    fn matvec(&self, x: &[f32], out: &mut [f32]) {
        use rayon::prelude::*;
        const ROWS: usize = 256;
        if self.rows * self.cols < 1 << 18 {
            for (o, row) in out.iter_mut().zip(self.w.chunks_exact(self.cols)) {
                *o += dot(row, x);
            }
            return;
        }
        out.par_chunks_mut(ROWS).zip(self.w.par_chunks(ROWS * self.cols)).for_each(|(o, w)| {
            for (o, row) in o.iter_mut().zip(w.chunks_exact(self.cols)) {
                *o += dot(row, x);
            }
        });
    }
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    // eight independent accumulators let the compiler vectorise
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

pub struct LstmLayer {
    /// `(4h, in)`, gate order i, f, g, o
    pub w_ih: Mat,
    /// `(4h, h)`
    pub w_hh: Mat,
    /// `b_ih + b_hh`, `4h`
    pub b: Vec<f32>,
}

/// Predictor state: per layer `(h, c)`.
#[derive(Clone)]
pub struct State {
    h: Vec<Vec<f32>>,
    c: Vec<Vec<f32>>,
}

pub struct Tdt {
    /// `(v + 1, 4h)`: each token's embedding through the first LSTM layer's input weights plus
    /// both of its biases, precomputed (the embedding itself is not needed afterwards).
    pub embed_gates: Mat,
    pub lstm: Vec<LstmLayer>,
    pub hidden: usize,
    /// joint `pred` projection `(j, hidden)` + bias
    pub pred: Mat,
    pub pred_b: Vec<f32>,
    /// output layer `(v + 1 + durations, j)` + bias
    pub out: Mat,
    pub out_b: Vec<f32>,
    /// Token id of blank (= vocabulary size).
    pub blank: usize,
    pub durations: Vec<usize>,
    pub max_symbols: usize,
    /// Restart the prediction network (as at the start of a clip) after this many frames without
    /// a token; `None` keeps its state for the whole piece.
    pub reset_after: Option<usize>,
}

/// Frames of silence (blanks) after which the predictor restarts: 2 s. After a sentence-final
/// token and a pause the model sometimes stops emitting for the rest of a long piece; restarting
/// the predictor, as at the beginning of an utterance, brings it back.
pub const RESET_AFTER_FRAMES: usize = 25;

/// One emitted token.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub id: u32,
    /// Encoder frame (80 ms) the token was emitted at.
    pub frame: usize,
    /// Predicted duration in frames.
    pub duration: usize,
    /// Softmax probability of the token among the token logits.
    pub prob: f32,
}

impl Tdt {
    pub fn initial_state(&self) -> State {
        let z = vec![0f32; self.hidden];
        State { h: vec![z.clone(); self.lstm.len()], c: vec![z; self.lstm.len()] }
    }

    /// Feed `token` to the prediction network: the joint's predictor projection and the new state.
    pub fn predict(&self, token: usize, st: &State) -> (Vec<f32>, State) {
        let h = self.hidden;
        let mut x: Vec<f32> = Vec::new();
        let mut next = st.clone();
        let mut gates = vec![0f32; 4 * h];
        for (l, layer) in self.lstm.iter().enumerate() {
            if l == 0 {
                let c = self.embed_gates.cols;
                match self.embed_gates.w.get(token * c..(token + 1) * c) {
                    Some(g) if c == gates.len() => gates.copy_from_slice(g),
                    _ => gates.copy_from_slice(&layer.b),
                }
            } else {
                gates.copy_from_slice(&layer.b);
                layer.w_ih.matvec(&x, &mut gates);
            }
            layer.w_hh.matvec(&st.h[l], &mut gates);
            let (c, hn) = (&mut next.c[l], &mut next.h[l]);
            for k in 0..h {
                let i = sigmoid(gates[k]);
                let f = sigmoid(gates[h + k]);
                let g = gates[2 * h + k].tanh();
                let o = sigmoid(gates[3 * h + k]);
                c[k] = f * c[k] + i * g;
                hn[k] = o * c[k].tanh();
            }
            x.clone_from(hn);
        }
        let mut p = self.pred_b.clone();
        self.pred.matvec(&x, &mut p);
        (p, next)
    }

    /// Greedy TDT decoding of projected encoder frames `enc` (`frames × j`, the joint `enc`
    /// projection with its bias). `cancel` is polled every 256 steps.
    pub fn decode(&self, enc: &[f32], frames: usize, cancel: &mut dyn FnMut() -> bool) -> Option<Vec<Token>> {
        let j = self.pred.rows;
        let nd = self.durations.len();
        let nv = self.blank + 1;
        if enc.len() != frames * j || self.out.rows != nv + nd || nd == 0 {
            return Some(Vec::new());
        }
        let mut tokens = Vec::new();
        let (mut g, mut state) = self.predict(self.blank, &self.initial_state());
        let mut hidden = vec![0f32; j];
        let mut logits = vec![0f32; self.out.rows];
        let (mut t, mut at_t) = (0usize, 0usize);
        let mut steps = 0usize;
        let mut blank_run = 0usize;
        while t < frames {
            steps += 1;
            if steps.is_multiple_of(256) && cancel() {
                return None;
            }
            let f = &enc[t * j..(t + 1) * j];
            for ((h, a), b) in hidden.iter_mut().zip(f).zip(&g) {
                *h = (a + b).max(0.0);
            }
            logits.copy_from_slice(&self.out_b);
            self.out.matvec(&hidden, &mut logits);
            let (tok, dur) = logits.split_at(nv);
            let k = argmax(tok);
            let d = self.durations.get(argmax(dur)).copied().unwrap_or(1);
            if k == self.blank {
                t += d.max(1);
                at_t = 0;
                blank_run += d.max(1);
                if self.reset_after.is_some_and(|n| blank_run >= n) && !tokens.is_empty() {
                    (g, state) = self.predict(self.blank, &self.initial_state());
                    blank_run = 0;
                }
                continue;
            }
            blank_run = 0;
            let m = tok.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let z: f32 = tok.iter().map(|v| (v - m).exp()).sum();
            tokens.push(Token { id: k as u32, frame: t, duration: d, prob: 1.0 / z.max(1.0) });
            (g, state) = self.predict(k, &state);
            if d > 0 {
                t += d;
                at_t = 0;
            } else {
                at_t += 1;
                if at_t >= self.max_symbols {
                    t += 1;
                    at_t = 0;
                }
            }
        }
        Some(tokens)
    }
}

fn argmax(x: &[f32]) -> usize {
    let mut best = 0;
    for (i, v) in x.iter().enumerate() {
        if *v > x[best] {
            best = i;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy joint whose output depends only on the encoder frame: frame f's projection one-hot
    /// selects the logits directly (pred projection is zero).
    fn toy(frame_logits: &[(usize, usize)], vocab: usize) -> (Tdt, Vec<f32>) {
        let durations = vec![0, 1, 2, 3, 4];
        let nv = vocab + 1;
        let jdim = nv + durations.len();
        // out = identity, so hidden == logits; enc frame = one-hot token + one-hot duration
        let mut out = vec![0f32; jdim * jdim];
        for i in 0..jdim {
            out[i * jdim + i] = 1.0;
        }
        let mut enc = Vec::new();
        for &(tok, dur) in frame_logits {
            let mut f = vec![0f32; jdim];
            f[tok] = 5.0;
            f[nv + dur] = 5.0;
            enc.extend(f);
        }
        let h = 2;
        let tdt = Tdt {
            embed_gates: Mat::new(nv, 4 * h, vec![0.0; nv * 4 * h]).unwrap(),
            lstm: vec![LstmLayer {
                w_ih: Mat::new(4 * h, 2, vec![0.0; 8 * h]).unwrap(),
                w_hh: Mat::new(4 * h, h, vec![0.0; 4 * h * h]).unwrap(),
                b: vec![0.0; 4 * h],
            }],
            hidden: h,
            pred: Mat::new(jdim, h, vec![0.0; jdim * h]).unwrap(),
            pred_b: vec![0.0; jdim],
            out: Mat::new(jdim, jdim, out).unwrap(),
            out_b: vec![0.0; jdim],
            blank: vocab,
            durations,
            max_symbols: 3,
            reset_after: None,
        };
        (tdt, enc)
    }

    #[test]
    fn greedy_follows_durations() {
        let blank = 4;
        // frame 0: token 1 dur 2 → frame 2: blank dur 0 (forced 1) → frame 3: token 2 dur 1 → frame 4: blank dur 3 → end
        let (tdt, enc) = toy(&[(1, 2), (3, 1), (blank, 0), (2, 1), (blank, 3)], 4);
        let toks = tdt.decode(&enc, 5, &mut || false).unwrap();
        let got: Vec<(u32, usize, usize)> = toks.iter().map(|t| (t.id, t.frame, t.duration)).collect();
        assert_eq!(got, vec![(1, 0, 2), (2, 3, 1)]);
        assert!(toks.iter().all(|t| t.prob > 0.5 && t.prob <= 1.0));
    }

    #[test]
    fn zero_duration_tokens_are_capped_per_frame() {
        // token 0 with duration 0 forever on frame 0: max_symbols (3) tokens, then frame 1
        let (tdt, enc) = toy(&[(0, 0), (4, 4)], 4);
        let toks = tdt.decode(&enc, 2, &mut || false).unwrap();
        assert_eq!(toks.len(), 3);
        assert!(toks.iter().all(|t| t.frame == 0));
        assert_eq!(tdt.decode(&enc, 2, &mut || true).map(|t| t.len()), Some(3));
        // malformed sizes decode to nothing instead of panicking
        assert_eq!(tdt.decode(&enc[..5], 2, &mut || false), Some(Vec::new()));
    }
}
