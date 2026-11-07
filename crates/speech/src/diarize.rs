//! Speaker labelling ("who spoke when") without a model.
//!
//! A classical, openly documented method (MFCC statistics + agglomerative clustering, as in the
//! speaker-clustering literature, e.g. Reynolds & Torres-Carrasquillo 2005; no code or data from
//! any implementation is used):
//!
//! 1. The transcript's words are grouped into **chunks**: a new chunk starts at a pause of at least
//!    [`Params::chunk_gap`] or when the chunk reaches [`Params::max_chunk`]; chunks shorter than
//!    [`Params::min_chunk`] are joined to the previous one.
//! 2. Each chunk gets a voice vector: 40-band log-mel energies → DCT → MFCC 1–19 over the chunk's
//!    voiced frames (within 30 dB of its loudest), then the mean and standard deviation of each
//!    coefficient (38 values), each scaled by that coefficient's spread over all voiced frames of
//!    the file.
//! 3. Average-linkage agglomerative clustering with RMS distance merges the closest clusters
//!    until the closest pair is farther apart than [`Params::threshold`] or only
//!    `max_speakers` remain. Clusters holding less than 5 % of the speech are folded into their
//!    nearest neighbour.
//! 4. Speakers are numbered in order of first appearance ("Speaker 1", "Speaker 2"…).
//!
//! It works for interviews and dialogue with distinct voices and clean turns; overlapping speech,
//! very similar voices or music beds confuse it. Labels are editable in the Text panel (rename a
//! speaker, or assign words to a speaker by hand), which is the fallback when it gets them wrong.

use filmcraft_project::{Speaker, Transcript};
use filmcraft_time::Tick;

use crate::{SAMPLE_RATE, TICKS_PER_SAMPLE};

/// Tunables (defaults chosen on LibriSpeech test-clean dialogues; see `docs/transcripts.md`).
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub chunk_gap: Tick,
    pub min_chunk: Tick,
    pub max_chunk: Tick,
    pub threshold: f32,
    pub max_speakers: usize,
}

impl Default for Params {
    fn default() -> Self {
        let ms = |m: i64| Tick(m * TICKS_PER_SAMPLE * SAMPLE_RATE as i64 / 1000);
        Self { chunk_gap: ms(250), min_chunk: ms(700), max_chunk: ms(4000), threshold: 0.4, max_speakers: 6 }
    }
}

const N_MELS: usize = 40;
const N_MFCC: usize = 20;
const FRAME_TICKS: i64 = TICKS_PER_SAMPLE * crate::mel::HOP as i64;
/// Agglomerative linkage is cubic in the number of chunks, so at most this many are clustered: one
/// chunk from each of that many equal stretches of the file, at a quasi-random position in the
/// stretch (see [`sample_chunks`]); every other chunk then joins the cluster whose clustered members
/// it is closest to on average, which is linear in the number of chunks. Folding a small cluster
/// afterwards compares each of its chunks with every other chunk, at worst quadratic, which
/// `MAX_CHUNKS` bounds. A 48-minute interview has about 800 chunks.
const MAX_CLUSTER_CHUNKS: usize = 256;
/// Beyond this many chunks (about eight hours of dense dialogue) the assignment pass, the small-cluster
/// fold and the per-chunk vectors would still be a document-controlled amount of work, so the
/// transcript keeps a single speaker instead.
const MAX_CHUNKS: usize = 8_192;
/// Frames of a chunk that feed its voice vector (60 s). The spectrum, mel and MFCC buffers are built
/// per chunk, never for the whole file (eight hours of power spectrum would be 2.3 GB), so a chunk
/// longer than this, which only hostile parameters allow, is represented by its first minute.
const MAX_CHUNK_FRAMES: usize = 6_000;

/// Word index ranges of the chunks.
pub fn chunks(t: &Transcript, p: &Params) -> Vec<std::ops::Range<usize>> {
    let w = &t.words;
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut a = 0;
    for i in 1..=w.len() {
        if i == w.len() || w[i].start - w[i - 1].end >= p.chunk_gap || w[i].end - w[a].start > p.max_chunk {
            if a < i {
                out.push(a..i);
            }
            a = i;
        }
    }
    // join short chunks to the previous one
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for c in out {
        let dur = w[c.end - 1].end - w[c.start].start;
        match merged.last_mut() {
            Some(l) if dur < p.min_chunk => l.end = c.end,
            _ => merged.push(c),
        }
    }
    // a short first chunk joins the next
    if merged.len() > 1 && w[merged[0].end - 1].end - w[0].start < p.min_chunk {
        let first = merged.remove(0);
        merged[0].start = first.start;
    }
    merged
}

fn mfcc_frames(audio: &[f32]) -> (usize, Vec<f32>, Vec<f32>) {
    let (frames, mel) = crate::mel::mel_energies(audio, N_MELS);
    // per frame: log energies (dB-ish) and the DCT-II of the log mel
    let mut energy = vec![0f32; frames];
    let mut mfcc = vec![0f32; frames * N_MFCC];
    let dct: Vec<f32> = (0..N_MFCC * N_MELS)
        .map(|i| {
            let (k, m) = (i / N_MELS, i % N_MELS);
            (std::f32::consts::PI * k as f32 * (m as f32 + 0.5) / N_MELS as f32).cos()
        })
        .collect();
    let mut lm = [0f32; N_MELS];
    for f in 0..frames {
        let mut e = 0f32;
        for m in 0..N_MELS {
            let v = mel[m * frames + f];
            e += v;
            lm[m] = (v + 1e-10).ln();
        }
        energy[f] = 10.0 * (e + 1e-10).log10();
        for k in 0..N_MFCC {
            mfcc[f * N_MFCC + k] = (0..N_MELS).map(|m| dct[k * N_MELS + m] * lm[m]).sum();
        }
    }
    (frames, energy, mfcc)
}

/// Energy and MFCCs of frames `a..b` of `audio`, computed from that stretch alone (plus one analysis
/// window past it, so the last frames are centred as they would be in the whole file). Returns the
/// number of frames produced, at most `b - a`.
fn chunk_mfcc(audio: &[f32], a: usize, b: usize) -> (usize, Vec<f32>, Vec<f32>) {
    let lo = a.saturating_mul(crate::mel::HOP).min(audio.len());
    let hi = b.saturating_mul(crate::mel::HOP).saturating_add(crate::mel::N_FFT).min(audio.len());
    let Some(slice) = audio.get(lo..hi) else { return (0, Vec::new(), Vec::new()) };
    let (frames, mut energy, mut mfcc) = mfcc_frames(slice);
    let keep = frames.min(b.saturating_sub(a));
    energy.truncate(keep);
    mfcc.truncate(keep * N_MFCC);
    (keep, energy, mfcc)
}

fn rms_distance(a: &[f32], b: &[f32]) -> f32 {
    (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.len().max(1) as f32).sqrt()
}

/// The chunks that take part in the agglomerative step: all of them when there are at most
/// [`MAX_CLUSTER_CHUNKS`], otherwise one from each of that many equal stretches of the file. The
/// pick inside a stretch follows the golden-ratio sequence, so it is spread over the stretch's
/// positions rather than tied to one of them: taking the heaviest (or the first, or the last) chunk
/// of every stretch would sample one voice only from a dialogue whose turns alternate two chunks per
/// stretch, or whose answers always outweigh the questions. A chunk with no voiced frames carries no
/// voice, so the heaviest chunk of the stretch stands in for it.
fn sample_chunks(n: usize, weight: &[f32]) -> Vec<usize> {
    if n <= MAX_CLUSTER_CHUNKS {
        return (0..n).collect();
    }
    const GOLDEN: f64 = 0.618_033_988_749_895;
    let w = |i: usize| weight.get(i).copied().unwrap_or(0.0);
    (0..MAX_CLUSTER_CHUNKS)
        .filter_map(|k| {
            let lo = k * n / MAX_CLUSTER_CHUNKS;
            let hi = ((k + 1) * n / MAX_CLUSTER_CHUNKS).clamp(lo + 1, n);
            let pos = ((k + 1) as f64 * GOLDEN).fract();
            let pick = lo + ((pos * (hi - lo) as f64) as usize).min(hi - lo - 1);
            if w(pick) > 0.0 { Some(pick) } else { (lo..hi).max_by(|&a, &b| w(a).total_cmp(&w(b))) }
        })
        .collect()
}

/// Label the speakers of `t` from `audio` (mono 16 kHz, the transcript's time base). Replaces
/// `t.speakers` and every word's speaker; returns the number of speakers found.
pub fn diarize(audio: &[f32], t: &mut Transcript, p: &Params) -> usize {
    let ch = chunks(t, p);
    if t.words.is_empty() {
        return 0;
    }
    let one = |t: &mut Transcript| {
        t.speakers = vec![Speaker { name: "Speaker 1".into() }];
        for w in &mut t.words {
            w.speaker = Some(0);
        }
        1
    };
    if ch.len() < 2 {
        return one(t);
    }
    if ch.len() > MAX_CHUNKS {
        return one(t);
    }
    let frames = audio.len() / crate::mel::HOP;
    // chunk vectors: mean and std of MFCC 1..N over the chunk's voiced frames. Features are computed
    // chunk by chunk, so memory is bounded by the longest chunk, not by the file.
    let dim = 2 * (N_MFCC - 1);
    let mut vecs: Vec<Vec<f32>> = Vec::new();
    let mut weight: Vec<f32> = Vec::new();
    // running sums of the MFCCs over every chunk's voiced frames, for the yardstick below
    let mut sum = [0f64; N_MFCC];
    let mut sumsq = [0f64; N_MFCC];
    let mut count = 0f64;
    for c in &ch {
        let a = ((t.words[c.start].start.0 / FRAME_TICKS).max(0) as usize).min(frames);
        let b = ((t.words[c.end - 1].end.0 / FRAME_TICKS).max(0) as usize).min(frames).max(a).min(a.saturating_add(MAX_CHUNK_FRAMES));
        let (cf, energy, mfcc) = chunk_mfcc(audio, a, b);
        let peak = energy.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let voiced: Vec<usize> = (0..cf).filter(|&f| energy[f] > peak - 30.0).collect();
        let mut v = vec![0f32; dim];
        if !voiced.is_empty() {
            let n = voiced.len() as f32;
            for k in 1..N_MFCC {
                let mean = voiced.iter().map(|&f| mfcc[f * N_MFCC + k]).sum::<f32>() / n;
                let var = voiced.iter().map(|&f| (mfcc[f * N_MFCC + k] - mean).powi(2)).sum::<f32>() / n;
                v[k - 1] = mean;
                v[N_MFCC - 1 + k - 1] = var.sqrt();
                for &f in &voiced {
                    let x = mfcc[f * N_MFCC + k] as f64;
                    sum[k] += x;
                    sumsq[k] += x * x;
                }
            }
            count += n as f64;
        }
        vecs.push(v);
        weight.push(voiced.len() as f32);
    }
    // scale each dimension by its spread over the voiced frames of every chunk (an absolute
    // yardstick: one voice stays close to itself however many chunks there are)
    let nf = count.max(1.0);
    for k in 1..N_MFCC {
        let mean = sum[k] / nf;
        let sd = ((sumsq[k] / nf - mean * mean).max(0.0).sqrt() as f32).max(1e-6);
        for v in &mut vecs {
            v[k - 1] = (v[k - 1] - mean as f32) / sd;
            v[N_MFCC - 1 + k - 1] /= sd;
        }
    }
    // the chunks that take part in the agglomerative step (all of them on a short file)
    let n = vecs.len();
    let sample = sample_chunks(n, &weight);
    let m = sample.len();
    let vec_of = |s: usize| vecs.get(sample.get(s).copied().unwrap_or(0)).map(Vec::as_slice).unwrap_or(&[]);
    // average-linkage agglomerative clustering over the sample (indices into `sample`)
    let mut dist = vec![0f32; m * m];
    for i in 0..m {
        for j in i + 1..m {
            let d = rms_distance(vec_of(i), vec_of(j));
            dist[i * m + j] = d;
            dist[j * m + i] = d;
        }
    }
    let mut clusters: Vec<Vec<usize>> = (0..m).map(|i| vec![i]).collect();
    let link = |a: &[usize], b: &[usize]| -> f32 {
        let mut s = 0f32;
        for &i in a {
            for &j in b {
                s += dist.get(i * m + j).copied().unwrap_or(0.0);
            }
        }
        s / (a.len() * b.len()).max(1) as f32
    };
    let max_speakers = p.max_speakers.max(1);
    loop {
        if clusters.len() <= 1 {
            break;
        }
        let mut best = (f32::INFINITY, 0, 0);
        for i in 0..clusters.len() {
            for j in i + 1..clusters.len() {
                let d = link(&clusters[i], &clusters[j]);
                if d < best.0 {
                    best = (d, i, j);
                }
            }
        }
        if best.0 > p.threshold && clusters.len() <= max_speakers {
            break;
        }
        let b = clusters.remove(best.2);
        clusters[best.1].extend(b);
    }
    // every chunk's cluster: a sampled chunk keeps its own, any other joins the cluster whose
    // sampled members it is closest to on average
    let mut label = vec![usize::MAX; n];
    for (ci, c) in clusters.iter().enumerate() {
        for &s in c {
            if let Some(l) = sample.get(s).and_then(|&i| label.get_mut(i)) {
                *l = ci;
            }
        }
    }
    for i in 0..n {
        if label[i] != usize::MAX {
            continue;
        }
        let v = vecs.get(i).map(Vec::as_slice).unwrap_or(&[]);
        let nearest = clusters
            .iter()
            .enumerate()
            .map(|(ci, c)| (c.iter().map(|&s| rms_distance(v, vec_of(s))).sum::<f32>() / c.len().max(1) as f32, ci))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|x| x.1)
            .unwrap_or(0);
        label[i] = nearest;
    }
    // fold clusters with < 5 % of the voiced frames into their nearest neighbour (over all chunks)
    let mut members: Vec<Vec<usize>> = (0..clusters.len()).map(|ci| (0..n).filter(|&i| label[i] == ci).collect()).collect();
    let link_all = |a: &[usize], b: &[usize]| -> f32 {
        let mut s = 0f32;
        for &i in a {
            for &j in b {
                s += rms_distance(vecs.get(i).map(Vec::as_slice).unwrap_or(&[]), vecs.get(j).map(Vec::as_slice).unwrap_or(&[]));
            }
        }
        s / (a.len() * b.len()).max(1) as f32
    };
    let total: f32 = weight.iter().sum::<f32>().max(1.0);
    loop {
        if members.len() <= 1 {
            break;
        }
        let small = members.iter().position(|c| c.iter().map(|&i| weight.get(i).copied().unwrap_or(0.0)).sum::<f32>() < 0.05 * total);
        let Some(si) = small else { break };
        let s = members.remove(si);
        let near = (0..members.len()).map(|a| (link_all(&s, &members[a]), a)).min_by(|x, y| x.0.total_cmp(&y.0)).map_or(0, |x| x.1);
        members[near].extend(s);
    }
    if members.len() <= 1 {
        return one(t);
    }
    // number speakers by first appearance
    for (ci, c) in members.iter().enumerate() {
        for &i in c {
            if let Some(l) = label.get_mut(i) {
                *l = ci;
            }
        }
    }
    let mut order: Vec<usize> = Vec::new();
    for &l in &label {
        if !order.contains(&l) {
            order.push(l);
        }
    }
    for (k, c) in ch.iter().enumerate() {
        let sp = order.iter().position(|&o| o == label[k]).unwrap_or(0) as u32;
        for w in &mut t.words[c.clone()] {
            w.speaker = Some(sp);
        }
    }
    t.speakers = (0..order.len()).map(|i| Speaker { name: format!("Speaker {}", i + 1) }).collect();
    order.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick;
    use filmcraft_project::Word;

    /// A synthetic "voice": a pulse train at `f0` through two resonances (formants), with a slow
    /// syllable envelope.
    fn voice(f0: f32, formants: [(f32, f32); 2], secs: f32, seed: u32) -> Vec<f32> {
        let n = (secs * 16_000.0) as usize;
        let mut out = vec![0f32; n];
        let mut phase = 0f32;
        let mut st = [[0f32; 2]; 2];
        let mut rng = seed;
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f32 / 16_000.0;
            let jitter = 1.0 + 0.03 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
            phase += f0 * jitter / 16_000.0;
            let mut x = if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            };
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            x += ((rng >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.02;
            let mut y = 0f32;
            for (k, (fc, bw)) in formants.iter().enumerate() {
                let r = (-std::f32::consts::PI * bw / 16_000.0).exp();
                let a1 = 2.0 * r * (2.0 * std::f32::consts::PI * fc / 16_000.0).cos();
                let a2 = -r * r;
                let v = x + a1 * st[k][0] + a2 * st[k][1];
                st[k][1] = st[k][0];
                st[k][0] = v;
                y += v;
            }
            let env = (std::f32::consts::PI * (t * 4.0).fract()).sin().max(0.0);
            *o = y * env * 0.05;
        }
        out
    }

    #[test]
    fn two_synthetic_voices_are_told_apart() {
        let a = |s| voice(110.0, [(600.0, 90.0), (1100.0, 120.0)], 2.0, s);
        let b = |s| voice(230.0, [(400.0, 80.0), (2600.0, 150.0)], 2.0, s);
        let silence = vec![0f32; 8000];
        let turns = [true, false, true, true, false, false, true, false];
        let mut audio = Vec::new();
        let mut words = Vec::new();
        for (k, &is_a) in turns.iter().enumerate() {
            let t0 = audio.len() as f64 / 16_000.0;
            audio.extend(if is_a { a(k as u32) } else { b(k as u32) });
            // four 0.5 s "words" per turn
            for w in 0..4 {
                let s = t0 + w as f64 * 0.5;
                words.push(Word::new(format!("w{k}.{w}"), seconds_tick(s), seconds_tick(s + 0.45)));
            }
            audio.extend(&silence);
        }
        let mut t = Transcript { words, ..Default::default() };
        let n = diarize(&audio, &mut t, &Params::default());
        assert_eq!(n, 2);
        for (k, &is_a) in turns.iter().enumerate() {
            let want = if is_a { 0 } else { 1 };
            for w in &t.words[k * 4..k * 4 + 4] {
                assert_eq!(w.speaker, Some(want), "turn {k}");
            }
        }
        assert_eq!(t.speakers[1].name, "Speaker 2");
        t.check().unwrap();
    }

    #[test]
    fn one_voice_is_one_speaker() {
        let audio = voice(140.0, [(500.0, 90.0), (1500.0, 120.0)], 12.0, 1);
        let words = (0..20).map(|i| Word::new("x", seconds_tick(i as f64 * 0.6), seconds_tick(i as f64 * 0.6 + 0.4))).collect();
        let mut t = Transcript { words, ..Default::default() };
        assert_eq!(diarize(&audio, &mut t, &Params::default()), 1);
        assert!(t.words.iter().all(|w| w.speaker == Some(0)));
    }

    #[test]
    fn chunks_split_at_pauses_and_join_short_ones() {
        let w = |a: f64, b: f64| Word::new("x", seconds_tick(a), seconds_tick(b));
        let t = Transcript { words: vec![w(0.0, 0.5), w(0.5, 1.0), w(2.0, 2.2), w(3.0, 3.6), w(3.6, 4.4)], ..Default::default() };
        assert_eq!(chunks(&t, &Params::default()), vec![0..3, 3..5]);
    }

    #[test]
    fn a_long_file_with_more_chunks_than_are_clustered_still_gets_its_speakers() {
        // two voices taking 300 turns of 1.5 s: about 300 chunks, more than MAX_CLUSTER_CHUNKS
        let a = |s| voice(110.0, [(600.0, 90.0), (1100.0, 120.0)], 1.5, s);
        let b = |s| voice(230.0, [(400.0, 80.0), (2600.0, 150.0)], 1.5, s);
        let silence = vec![0f32; 8000];
        let turns: Vec<bool> = (0..300).map(|k| matches!(k % 5, 0 | 2 | 3)).collect();
        let mut audio = Vec::new();
        let mut words = Vec::new();
        for (k, &is_a) in turns.iter().enumerate() {
            let t0 = audio.len() as f64 / 16_000.0;
            audio.extend(if is_a { a(k as u32) } else { b(k as u32) });
            for w in 0..3 {
                let s = t0 + w as f64 * 0.5;
                words.push(Word::new(format!("w{k}.{w}"), seconds_tick(s), seconds_tick(s + 0.45)));
            }
            audio.extend(&silence);
        }
        let mut t = Transcript { words, ..Default::default() };
        assert!(chunks(&t, &Params::default()).len() > MAX_CLUSTER_CHUNKS);
        let n = diarize(&audio, &mut t, &Params::default());
        assert_eq!(n, 2);
        let wrong = turns.iter().enumerate().filter(|&(k, &is_a)| t.words[k * 3].speaker != Some(if is_a { 0 } else { 1 })).count();
        assert_eq!(wrong, 0, "turns labelled with the wrong speaker");
        t.check().unwrap();
    }

    /// `turns` of two voices (`true` is voice A) with per-voice turn lengths, 0.3 s apart; three
    /// equal "words" per turn. Returns the audio and the transcript.
    fn dialogue(turns: &[bool], secs_a: f32, secs_b: f32) -> (Vec<f32>, Transcript) {
        let silence = vec![0f32; 4800];
        let mut audio = Vec::new();
        let mut words = Vec::new();
        for (k, &is_a) in turns.iter().enumerate() {
            let t0 = audio.len() as f64 / 16_000.0;
            let secs = if is_a { secs_a } else { secs_b };
            audio.extend(if is_a {
                voice(110.0, [(600.0, 90.0), (1100.0, 120.0)], secs, k as u32)
            } else {
                voice(230.0, [(400.0, 80.0), (2600.0, 150.0)], secs, k as u32)
            });
            let step = secs as f64 / 3.0;
            for w in 0..3 {
                let s = t0 + w as f64 * step;
                words.push(Word::new(format!("w{k}.{w}"), seconds_tick(s), seconds_tick(s + step * 0.9)));
            }
            audio.extend(&silence);
        }
        (audio, Transcript { words, ..Default::default() })
    }

    fn wrong_turns(t: &Transcript, turns: &[bool]) -> usize {
        turns.iter().enumerate().filter(|&(k, &is_a)| t.words[k * 3].speaker != Some(if is_a { 0 } else { 1 })).count()
    }

    #[test]
    fn strictly_alternating_equal_turns_are_sampled_from_both_voices() {
        // 512 turns, A B A B..., all the same length: two chunks per sampling stretch, and the two
        // weigh the same, so a tie-break that always takes the same side would sample one voice only
        let turns: Vec<bool> = (0..512).map(|k| k % 2 == 0).collect();
        let (audio, mut t) = dialogue(&turns, 0.8, 0.8);
        assert_eq!(chunks(&t, &Params::default()).len(), 512);
        assert_eq!(diarize(&audio, &mut t, &Params::default()), 2);
        assert_eq!(wrong_turns(&t, &turns), 0, "turns labelled with the wrong speaker");
        t.check().unwrap();
    }

    #[test]
    fn short_questions_between_long_answers_are_still_a_speaker() {
        // 512 turns, a 0.8 s question then a 1.6 s answer: the answer outweighs the question in every
        // two-chunk stretch, so a sample of the heaviest chunks would hold the answering voice only
        let turns: Vec<bool> = (0..512).map(|k| k % 2 == 0).collect();
        let (audio, mut t) = dialogue(&turns, 0.8, 1.6);
        assert_eq!(chunks(&t, &Params::default()).len(), 512);
        assert_eq!(diarize(&audio, &mut t, &Params::default()), 2);
        assert_eq!(wrong_turns(&t, &turns), 0, "turns labelled with the wrong speaker");
        t.check().unwrap();
    }

    #[test]
    fn excessive_chunks_fall_back_without_clustering() {
        let words = (0..=MAX_CHUNKS).map(|i| Word::new("x", seconds_tick(i as f64 * 2.0), seconds_tick(i as f64 * 2.0 + 1.0))).collect();
        let mut t = Transcript { words, ..Default::default() };
        assert_eq!(diarize(&[], &mut t, &Params::default()), 1);
        assert!(t.words.iter().all(|word| word.speaker == Some(0)));
    }
}
