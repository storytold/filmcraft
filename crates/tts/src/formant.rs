//! The built-in voices: an original source–filter formant synthesizer.
//!
//! Text is turned into phones by simple English spelling rules ([`phones`]): digraphs (`th`, `sh`,
//! `ee`, `oo`, `igh`…), a silent final `e` that lengthens the vowel before it (`make`, `time`),
//! and digits read as words. Each phone is a target (three formant frequencies and bandwidths,
//! voicing and noise amplitudes, a duration). The renderer moves between targets with short
//! transitions, drives a cascade of three two-pole resonators with a glottal pulse train for voiced
//! sounds, and a resonator at the phone's noise frequency with white noise for fricatives, bursts
//! and aspiration. Pitch follows a falling line across each sentence and rises at a question mark.
//!
//! Vowel formants are the published adult averages of Peterson & Barney, "Control Methods Used in
//! a Study of the Vowels" (JASA 24, 1952). The digital resonator is the standard two-pole form
//! described in D. H. Klatt, "Software for a cascade/parallel formant synthesizer" (JASA 67, 1980).
//! Everything else is original. The result is intelligible-ish and clearly robotic: it is the
//! no-download fallback, not the main voice.

use crate::script::{self, Segment};
use crate::{Audio, Gender, MAX_SECONDS, Params, SAMPLE_RATE, TtsError, Voice, VoiceInfo, check_text};

/// A built-in voice: catalogue entry plus its speaking parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceSpec {
    pub info: VoiceInfo,
    /// Base pitch in Hz.
    pub f0: f64,
    /// Formant scale (vocal tract length): 1 = adult male averages.
    pub formant_scale: f64,
    /// Breath noise mixed into voicing, 0–1.
    pub breath: f32,
}

const LICENSE: &str = "Built into FilmCraft (project licence)";

pub static VOICES: &[VoiceSpec] = &[
    VoiceSpec {
        info: VoiceInfo {
            id: "basic-female",
            name: "Basic Female",
            language: "en-US",
            gender: Gender::Female,
            engine: "basic",
            description: "Built-in voice. No download; robotic.",
            license: LICENSE,
            installed: true,
        },
        f0: 205.0,
        formant_scale: 1.17,
        breath: 0.06,
    },
    VoiceSpec {
        info: VoiceInfo {
            id: "basic-male",
            name: "Basic Male",
            language: "en-US",
            gender: Gender::Male,
            engine: "basic",
            description: "Built-in voice. No download; robotic.",
            license: LICENSE,
            installed: true,
        },
        f0: 112.0,
        formant_scale: 1.0,
        breath: 0.03,
    },
];

/// A built-in voice ready to speak.
pub struct FormantVoice {
    spec: VoiceSpec,
}

impl FormantVoice {
    pub fn new(spec: VoiceSpec) -> Self {
        FormantVoice { spec }
    }
}

impl Voice for FormantVoice {
    fn info(&self) -> VoiceInfo {
        self.spec.info
    }

    fn synthesize(&self, text: &str, params: &Params) -> Result<Audio, TtsError> {
        check_text(text)?;
        params.check()?;
        let segments = script::parse(text);
        // phones per speech segment, and the length check before anything is rendered
        let mut plan: Vec<Piece> = Vec::with_capacity(segments.len());
        let mut total_ms = 0.0f64;
        let mut spoken = false;
        for seg in &segments {
            match seg {
                Segment::Pause { millis } => {
                    total_ms += f64::from(*millis);
                    plan.push(Piece::Pause(*millis));
                }
                Segment::Speech(s) => {
                    let ph = phones(s);
                    spoken |= ph.iter().any(|p| p.kind != Kind::Silence);
                    total_ms += ph.iter().map(|p| f64::from(p.dur_ms)).sum::<f64>() / params.pace;
                    plan.push(Piece::Speech(ph));
                }
            }
            if total_ms > MAX_SECONDS * 1000.0 {
                return Err(TtsError::AudioTooLong);
            }
        }
        if !spoken {
            return Err(TtsError::Empty);
        }
        let estimate = (total_ms / 1000.0 * f64::from(SAMPLE_RATE)).ceil() as usize;
        let mut out: Vec<f32> = Vec::new();
        out.try_reserve(estimate.saturating_add(SAMPLE_RATE as usize)).map_err(|e| TtsError::Invalid(format!("unable to allocate the narration: {e}")))?;
        for piece in &plan {
            match piece {
                Piece::Pause(ms) => {
                    let n = (f64::from(*ms) / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize;
                    out.resize(out.len().saturating_add(n), 0.0);
                }
                Piece::Speech(ph) => render(&self.spec, ph, params, &mut out),
            }
        }
        // one gain for the whole narration so loudness doesn't jump between pieces
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 0.0 && peak.is_finite() {
            let g = 0.8 / peak;
            for s in &mut out {
                *s *= g;
            }
        } else if !peak.is_finite() {
            return Err(TtsError::Invalid("synthesis produced invalid samples".into()));
        }
        Ok(Audio { samples: out, sample_rate: SAMPLE_RATE })
    }
}

enum Piece {
    Pause(u32),
    Speech(Vec<Phone>),
}

// ------------------------------------------------------------------------------------- phones

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Vowel,
    Glide,
    Nasal,
    Fricative,
    Closure,
    Burst,
    Silence,
}

/// One synthesis target.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Phone {
    kind: Kind,
    /// Formant frequencies (adult male, scaled per voice) and bandwidths, Hz.
    f: [f32; 3],
    bw: [f32; 3],
    /// Voicing amplitude 0–1.
    voice: f32,
    /// Noise amplitude 0–1, through a resonator at `noise_f` / `noise_bw`.
    noise: f32,
    noise_f: f32,
    noise_bw: f32,
    dur_ms: f32,
    /// Pitch multiplier at this phone (stress, sentence end).
    pitch: f32,
    /// Ends a sentence (pitch reset after it).
    sentence_end: bool,
}

const BW: [f32; 3] = [80.0, 100.0, 140.0];
/// Neutral tract: what silences and closures move towards.
const NEUTRAL: [f32; 3] = [500.0, 1500.0, 2500.0];

fn base(kind: Kind, f: [f32; 3], dur_ms: f32) -> Phone {
    Phone { kind, f, bw: BW, voice: 0.0, noise: 0.0, noise_f: 3000.0, noise_bw: 2000.0, dur_ms, pitch: 1.0, sentence_end: false }
}

fn vowel(f: [f32; 3], dur_ms: f32) -> Phone {
    Phone { voice: 1.0, ..base(Kind::Vowel, f, dur_ms) }
}

fn glide(f: [f32; 3]) -> Phone {
    Phone { voice: 0.8, ..base(Kind::Glide, f, 65.0) }
}

fn nasal(f: [f32; 3]) -> Phone {
    Phone { voice: 0.45, bw: [60.0, 200.0, 300.0], ..base(Kind::Nasal, f, 75.0) }
}

fn fricative(noise_f: f32, noise_bw: f32, noise: f32, voiced: bool, dur_ms: f32) -> Phone {
    Phone { voice: if voiced { 0.35 } else { 0.0 }, noise, noise_f, noise_bw, ..base(Kind::Fricative, NEUTRAL, dur_ms) }
}

fn silence(dur_ms: f32) -> Phone {
    base(Kind::Silence, NEUTRAL, dur_ms)
}

/// A stop: closure then burst.
fn stop(out: &mut Vec<Phone>, burst_f: f32, voiced: bool) {
    out.push(Phone { voice: if voiced { 0.12 } else { 0.0 }, ..base(Kind::Closure, NEUTRAL, if voiced { 45.0 } else { 55.0 }) });
    out.push(Phone { noise: 0.9, noise_f: burst_f, noise_bw: 1800.0, ..base(Kind::Burst, NEUTRAL, if voiced { 15.0 } else { 30.0 }) });
}

// Peterson & Barney (1952) adult male averages: F1, F2, F3.
const AA: [f32; 3] = [730.0, 1090.0, 2440.0];
const AE: [f32; 3] = [660.0, 1720.0, 2410.0];
const AH: [f32; 3] = [640.0, 1190.0, 2390.0];
const AO: [f32; 3] = [570.0, 840.0, 2410.0];
const EH: [f32; 3] = [530.0, 1840.0, 2480.0];
const ER: [f32; 3] = [490.0, 1350.0, 1690.0];
const IH: [f32; 3] = [390.0, 1990.0, 2550.0];
const IY: [f32; 3] = [270.0, 2290.0, 3010.0];
const UH: [f32; 3] = [440.0, 1020.0, 2240.0];
const UW: [f32; 3] = [300.0, 870.0, 2240.0];

fn diphthong(out: &mut Vec<Phone>, a: [f32; 3], b: [f32; 3], stressed: bool) {
    let d = if stressed { 95.0 } else { 70.0 };
    out.push(vowel(a, d));
    out.push(vowel(b, d * 0.8));
}

const DIGITS: [&str; 10] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine"];

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Phones for a piece of speech (no pause markers).
fn phones(text: &str) -> Vec<Phone> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut question_from = 0usize;
    for c in text.chars().chain(std::iter::once(' ')) {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphabetic() || c == '\'' {
            if c != '\'' {
                word.push(c);
            }
            continue;
        }
        if !word.is_empty() {
            speak_word(&word, &mut out);
            word.clear();
        }
        match c {
            '0'..='9' => {
                let d = DIGITS.get(c as usize - '0' as usize).copied().unwrap_or("");
                speak_word(d, &mut out);
                out.push(silence(30.0));
            }
            ',' | ';' | ':' | '-' | '(' | ')' => out.push(silence(170.0)),
            '.' | '!' | '?' => {
                if c == '?' {
                    // rise over the last word of the question
                    let n = out.len();
                    let from =
                        out.get(question_from..).map_or(n, |s| s.iter().rposition(|p| p.kind == Kind::Silence).map_or(question_from, |i| question_from + i));
                    for p in out.get_mut(from..).into_iter().flatten() {
                        p.pitch *= 1.25;
                    }
                }
                if let Some(last) = out.last_mut() {
                    last.sentence_end = true;
                }
                out.push(silence(330.0));
                question_from = out.len();
            }
            _ => {
                if out.last().is_some_and(|p| p.kind != Kind::Silence) {
                    out.push(silence(25.0));
                }
            }
        }
    }
    out
}

/// Spelling rules for one lowercase ASCII word.
fn speak_word(w: &str, out: &mut Vec<Phone>) {
    let c: Vec<char> = w.chars().collect();
    let n = c.len();
    let at = |i: usize| c.get(i).copied().unwrap_or(' ');
    // a final silent e makes the vowel before the last consonant long: make, time, home, cute
    let magic_e = n >= 3 && at(n - 1) == 'e' && !is_vowel(at(n - 2)) && is_vowel(at(n - 3));
    let mut stressed = true;
    let mut i = 0;
    while i < n {
        let (a, b, d) = (at(i), at(i + 1), at(i + 2));
        let long = magic_e && i == n - 3;
        let vd = if stressed { 115.0 } else { 80.0 };
        let before = out.len();
        let step = match a {
            'a' if b == 'i' || b == 'y' => {
                diphthong(out, EH, IY, stressed);
                2
            }
            'a' if b == 'u' || b == 'w' => {
                out.push(vowel(AO, vd));
                2
            }
            'a' if b == 'r' => {
                out.push(vowel(AA, vd));
                out.push(glide([420.0, 1300.0, 1600.0]));
                2
            }
            'a' if long => {
                diphthong(out, EH, IY, stressed);
                1
            }
            'a' => {
                out.push(vowel(if stressed { AE } else { AH }, vd));
                1
            }
            'e' if b == 'e' || b == 'a' => {
                out.push(vowel(IY, vd + 20.0));
                2
            }
            'e' if b == 'r' => {
                out.push(vowel(ER, vd));
                2
            }
            'e' if b == 'y' && i + 2 == n => {
                out.push(vowel(IY, vd));
                2
            }
            'e' if i == n - 1 && n > 2 => 1, // silent final e
            'e' => {
                out.push(vowel(if i == n - 1 { IY } else { EH }, vd));
                1
            }
            'i' if b == 'g' && d == 'h' => {
                diphthong(out, AA, IY, stressed);
                3
            }
            'i' if long || (i == n - 1 && n <= 2) => {
                diphthong(out, AA, IY, stressed);
                1
            }
            'i' if b == 'r' => {
                out.push(vowel(ER, vd));
                2
            }
            'i' => {
                out.push(vowel(IH, vd));
                1
            }
            'o' if b == 'o' => {
                out.push(vowel(if d == 'k' { UH } else { UW }, vd + 15.0));
                2
            }
            'o' if b == 'u' || b == 'w' => {
                diphthong(out, AA, UW, stressed);
                2
            }
            'o' if b == 'i' || b == 'y' => {
                diphthong(out, AO, IY, stressed);
                2
            }
            'o' if b == 'a' || long || i == n - 1 => {
                diphthong(out, AO, UW, stressed);
                if b == 'a' { 2 } else { 1 }
            }
            'o' if b == 'r' => {
                out.push(vowel(AO, vd));
                out.push(glide([420.0, 1300.0, 1600.0]));
                2
            }
            'o' => {
                out.push(vowel(AA, vd));
                1
            }
            'u' if long => {
                out.push(glide([280.0, 2200.0, 3000.0]));
                out.push(vowel(UW, vd));
                1
            }
            'u' if b == 'r' => {
                out.push(vowel(ER, vd));
                2
            }
            'u' => {
                out.push(vowel(AH, vd));
                1
            }
            'y' if i == 0 => {
                out.push(glide([280.0, 2200.0, 3000.0]));
                1
            }
            'y' => {
                if i == n - 1 && n <= 3 {
                    diphthong(out, AA, IY, stressed);
                } else {
                    out.push(vowel(if i == n - 1 { IY } else { IH }, vd));
                }
                1
            }
            't' if b == 'h' => {
                out.push(fricative(6000.0, 4000.0, 0.25, false, 80.0));
                2
            }
            's' if b == 'h' => {
                out.push(fricative(2600.0, 1400.0, 0.7, false, 110.0));
                2
            }
            'c' if b == 'h' => {
                stop(out, 4000.0, false);
                out.push(fricative(2600.0, 1400.0, 0.7, false, 70.0));
                2
            }
            'p' if b == 'h' => {
                out.push(fricative(5000.0, 4000.0, 0.3, false, 90.0));
                2
            }
            'w' if b == 'h' => {
                out.push(glide([300.0, 610.0, 2200.0]));
                2
            }
            'n' if b == 'g' => {
                out.push(nasal([250.0, 2300.0, 2700.0]));
                2
            }
            'c' if b == 'k' => {
                stop(out, 2000.0, false);
                2
            }
            'q' if b == 'u' => {
                stop(out, 2000.0, false);
                out.push(glide([300.0, 610.0, 2200.0]));
                2
            }
            'c' if matches!(b, 'e' | 'i' | 'y') => {
                out.push(fricative(6000.0, 2000.0, 0.6, false, 100.0));
                1
            }
            'c' | 'k' | 'q' => {
                stop(out, 2000.0, false);
                1
            }
            'g' if matches!(b, 'e' | 'i') && i > 0 => {
                stop(out, 3500.0, true);
                out.push(fricative(2600.0, 1400.0, 0.5, true, 60.0));
                1
            }
            'g' => {
                stop(out, 2000.0, true);
                1
            }
            'j' => {
                stop(out, 3500.0, true);
                out.push(fricative(2600.0, 1400.0, 0.5, true, 60.0));
                1
            }
            'b' => {
                stop(out, 800.0, true);
                1
            }
            'd' => {
                stop(out, 3500.0, true);
                1
            }
            'p' => {
                stop(out, 800.0, false);
                1
            }
            't' => {
                stop(out, 4000.0, false);
                1
            }
            'x' => {
                stop(out, 2000.0, false);
                out.push(fricative(6000.0, 2000.0, 0.6, false, 90.0));
                1
            }
            'f' => {
                out.push(fricative(5000.0, 4000.0, 0.3, false, 90.0));
                1
            }
            'v' => {
                out.push(fricative(5000.0, 4000.0, 0.25, true, 70.0));
                1
            }
            's' => {
                out.push(fricative(6000.0, 2000.0, 0.6, false, 100.0));
                1
            }
            'z' => {
                out.push(fricative(6000.0, 2000.0, 0.45, true, 85.0));
                1
            }
            'h' => {
                out.push(fricative(1500.0, 2500.0, 0.3, false, 55.0));
                1
            }
            'l' => {
                out.push(glide([360.0, 1300.0, 2700.0]));
                1
            }
            'r' => {
                out.push(glide([420.0, 1300.0, 1600.0]));
                1
            }
            'w' => {
                out.push(glide([300.0, 610.0, 2200.0]));
                1
            }
            'm' => {
                out.push(nasal([250.0, 1100.0, 2400.0]));
                1
            }
            'n' => {
                out.push(nasal([250.0, 1700.0, 2600.0]));
                1
            }
            _ => 1,
        };
        let added_vowel = out.get(before..).is_some_and(|s| s.iter().any(|p| p.kind == Kind::Vowel));
        if added_vowel && stressed {
            for p in out.get_mut(before..).into_iter().flatten() {
                p.pitch *= 1.08;
            }
            stressed = false;
        }
        // doubled consonants are one sound: "ll", "ss", "tt"
        let mut skip = step;
        while !is_vowel(a) && at(i + skip) == a {
            skip += 1;
        }
        i += skip;
    }
}

// ------------------------------------------------------------------------------------- render

/// Two-pole resonator (unity gain at DC).
#[derive(Clone, Copy, Default)]
struct Resonator {
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn set(&mut self, f: f32, bw: f32) {
        let t = 1.0 / SAMPLE_RATE as f32;
        let f = f.clamp(50.0, SAMPLE_RATE as f32 * 0.45);
        let bw = bw.clamp(20.0, 8000.0);
        self.c = -(-2.0 * std::f32::consts::PI * bw * t).exp();
        self.b = 2.0 * (-std::f32::consts::PI * bw * t).exp() * (2.0 * std::f32::consts::PI * f * t).cos();
        self.a = 1.0 - self.b - self.c;
    }
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Deterministic white noise (xorshift32), −1..1.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Parameters are updated every `BLOCK` samples.
const BLOCK: usize = 48;
/// Transition into each phone, ms.
const TRANSITION_MS: f32 = 30.0;

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn render(spec: &VoiceSpec, ph: &[Phone], params: &Params, out: &mut Vec<f32>) {
    let sr = SAMPLE_RATE as f32;
    let shift = 2f64.powf(params.semitones / 12.0);
    let f0_base = (spec.f0 * shift) as f32;
    let fscale = (spec.formant_scale * 2f64.powf(params.semitones / 12.0 * 0.15)) as f32;
    let pace = params.pace as f32;
    let mut res = [Resonator::default(); 3];
    let mut nres = Resonator::default();
    let mut noise = Noise(0x9E37_79B9);
    let mut phase = 0.0f32;
    let mut prev_glottal = 0.0f32;
    let mut dc = (0.0f32, 0.0f32);
    // sentence pitch line: from 1.1 down to 0.85 of the base over each sentence
    let sentence_len = |from: usize| -> f32 { ph.get(from..).map_or(0, |s| s.iter().position(|p| p.sentence_end).map_or(s.len(), |e| e + 1)) as f32 };
    let mut sentence_start = 0usize;
    let mut sentence_n = sentence_len(0).max(1.0);
    let mut prev = silence(0.0);
    for (k, p) in ph.iter().enumerate() {
        let n = ((p.dur_ms / pace) / 1000.0 * sr).round().max(1.0) as usize;
        let trans = ((TRANSITION_MS / pace) / 1000.0 * sr).min(n as f32 * 0.5).max(1.0);
        let line = 1.1 - 0.25 * ((k - sentence_start) as f32 / sentence_n);
        let mut i = 0;
        while i < n {
            let t = ((i as f32) / trans).min(1.0);
            let f = [0, 1, 2].map(|j| lerp(prev.f[j], p.f[j], t) * fscale);
            for (j, r) in res.iter_mut().enumerate() {
                r.set(f[j], p.bw[j] * fscale.sqrt());
            }
            nres.set(lerp(prev.noise_f, p.noise_f, t), lerp(prev.noise_bw, p.noise_bw, t));
            let voice = lerp(prev.voice, p.voice, t);
            let namp = lerp(prev.noise, p.noise, t);
            let f0 = (f0_base * line * lerp(prev.pitch, p.pitch, t)).clamp(40.0, 600.0);
            let end = (i + BLOCK).min(n);
            for _ in i..end {
                phase += f0 / sr;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                // glottal flow pulse (open 60 % of the period), differentiated for lip radiation
                let g = if phase < 0.6 { (std::f32::consts::PI * phase / 0.6).sin().powi(2) } else { 0.0 };
                let dg = g - prev_glottal;
                prev_glottal = g;
                let breath = noise.next() * spec.breath * g;
                let mut v = (dg * 6.0 + breath) * voice;
                for r in &mut res {
                    v = r.tick(v);
                }
                let nz = nres.tick(noise.next()) * namp * 0.6;
                // DC blocker
                let x = v + nz;
                let y = x - dc.0 + 0.995 * dc.1;
                dc = (x, y);
                out.push(y);
            }
            i = end;
        }
        prev = *p;
        if p.sentence_end {
            sentence_start = k + 1;
            sentence_n = sentence_len(k + 1).max(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice;

    fn say(id: &str, text: &str, p: Params) -> Audio {
        voice(id).unwrap().synthesize(text, &p).unwrap()
    }

    /// Dominant pitch of the middle of `s` by autocorrelation, 60–500 Hz.
    fn pitch(s: &[f32]) -> f64 {
        let mid = &s[s.len() / 3..s.len() * 2 / 3];
        let sr = f64::from(SAMPLE_RATE);
        let (lo, hi) = ((sr / 500.0) as usize, (sr / 60.0) as usize);
        let best = (lo..hi).max_by(|&a, &b| {
            let ca: f64 = mid.iter().zip(&mid[a..]).map(|(x, y)| f64::from(x * y)).sum();
            let cb: f64 = mid.iter().zip(&mid[b..]).map(|(x, y)| f64::from(x * y)).sum();
            ca.total_cmp(&cb)
        });
        sr / best.unwrap() as f64
    }

    #[test]
    fn speaks_deterministically_with_valid_samples() {
        let a = say("basic-female", "Hello world, this is FilmCraft. Ready?", Params::default());
        let b = say("basic-female", "Hello world, this is FilmCraft. Ready?", Params::default());
        assert_eq!(a, b);
        assert_eq!(a.sample_rate, SAMPLE_RATE);
        assert!(a.seconds() > 1.0 && a.seconds() < 8.0, "{}", a.seconds());
        assert!(a.samples.iter().all(|s| s.is_finite() && s.abs() <= 0.8001));
        let rms = (a.samples.iter().map(|s| f64::from(s * s)).sum::<f64>() / a.samples.len() as f64).sqrt();
        assert!(rms > 0.03, "too quiet: {rms}");
    }

    #[test]
    fn pause_markers_are_exact_silence() {
        let plain = say("basic-male", "One. Two.", Params::default());
        let paused = say("basic-male", "One. [pause 1s] Two.", Params::default());
        assert_eq!(paused.samples.len(), plain.samples.len() + SAMPLE_RATE as usize);
        let one = say("basic-male", "One. ", Params::default()).samples.len();
        assert!(paused.samples[one..one + SAMPLE_RATE as usize].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn pace_scales_duration() {
        let text = "The quick brown fox jumps over the lazy dog.";
        let n1 = say("basic-female", text, Params::default()).samples.len() as f64;
        let n2 = say("basic-female", text, Params { pace: 2.0, ..Default::default() }).samples.len() as f64;
        let nh = say("basic-female", text, Params { pace: 0.5, ..Default::default() }).samples.len() as f64;
        assert!((n2 / n1 - 0.5).abs() < 0.05, "{}", n2 / n1);
        assert!((nh / n1 - 2.0).abs() < 0.1, "{}", nh / n1);
    }

    #[test]
    fn pitch_follows_voice_and_setting() {
        let text = "aaaaaaaaaaaaaaaaaaaa";
        let female = pitch(&say("basic-female", text, Params::default()).samples);
        let male = pitch(&say("basic-male", text, Params::default()).samples);
        let high = pitch(&say("basic-male", text, Params { semitones: 6.0, ..Default::default() }).samples);
        assert!(female > male * 1.5, "female {female} male {male}");
        assert!((high / male - 2f64.powf(0.5)).abs() < 0.12, "high {high} male {male}");
    }

    #[test]
    fn refuses_what_it_cannot_say() {
        let v = voice("basic-female").unwrap();
        assert_eq!(v.synthesize("", &Params::default()), Err(TtsError::Empty));
        assert_eq!(v.synthesize("👍 🎬", &Params::default()), Err(TtsError::Empty));
        assert_eq!(v.synthesize("日本語", &Params::default()), Err(TtsError::Empty));
        assert_eq!(v.synthesize("[pause 2s]", &Params::default()), Err(TtsError::Empty));
        assert!(matches!(v.synthesize("hi", &Params { pace: f64::NAN, ..Default::default() }), Err(TtsError::Invalid(_))));
        // 64 KB of letters would be over the 30-minute limit: refused before rendering
        assert_eq!(v.synthesize(&"a ".repeat(crate::MAX_TEXT_BYTES / 2), &Params { pace: 0.5, ..Default::default() }), Err(TtsError::AudioTooLong));
    }

    #[test]
    fn hostile_scripts_never_panic() {
        let v = voice("basic-male").unwrap();
        for s in ["a\0b", "x\u{301}y", "'''", "9999999999999999999999", "[pause 1s]a[pause", "Ünïcödé façade", "?!?!?!", "a?b?c?", "zzzzzzzz", "e"] {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.synthesize(s, &Params::default())));
            assert!(r.is_ok(), "panicked on {s:?}");
        }
    }
}
