//! IIR filter design from classical analog prototypes (Scientific Filter).
//!
//! Textbook method: a normalised low-pass prototype (zeros, poles, gain) — Butterworth, Bessel
//! (roots of the reverse Bessel polynomial, normalised to −3 dB at Ω = 1), Chebyshev type I and
//! elliptic (Cauer; Landen-transformation formulation of the Jacobi elliptic functions, after
//! S. J. Orfanidis, *Lecture Notes on Elliptic Filter Design*, 2006) — is transformed to
//! low-pass / high-pass / band-pass / band-stop at pre-warped analog frequencies, mapped to
//! the z-plane with the bilinear transform and factored into second-order sections.
//!
//! Everything works on fixed-size arrays so a design never allocates (it runs from
//! `set_param` on the audio thread).

use crate::biquad::Coeffs;
use std::f64::consts::PI;
use std::ops::{Add, Div, Mul, Neg, Sub};

/// Highest prototype order.
pub const MAX_ORDER: usize = 12;
/// Most second-order sections a design produces (band-pass/stop double the order).
pub const MAX_SECTIONS: usize = MAX_ORDER;
const MAX_ROOTS: usize = 2 * MAX_ORDER;

/// Minimal complex number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    pub const fn real(re: f64) -> C {
        C { re, im: 0.0 }
    }
    pub fn scale(self, s: f64) -> C {
        C::new(self.re * s, self.im * s)
    }
    pub fn conj(self) -> C {
        C::new(self.re, -self.im)
    }
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    pub fn sqrt(self) -> C {
        let r = self.abs();
        let re = ((r + self.re) * 0.5).max(0.0).sqrt();
        let im = ((r - self.re) * 0.5).max(0.0).sqrt();
        C::new(re, if self.im < 0.0 { -im } else { im })
    }
    pub fn exp(self) -> C {
        let e = self.re.exp();
        C::new(e * self.im.cos(), e * self.im.sin())
    }
    pub fn ln(self) -> C {
        C::new(self.abs().ln(), self.im.atan2(self.re))
    }
    pub fn cos(self) -> C {
        C::new(self.re.cos() * self.im.cosh(), -self.re.sin() * self.im.sinh())
    }
    pub fn sin(self) -> C {
        C::new(self.re.sin() * self.im.cosh(), self.re.cos() * self.im.sinh())
    }
    /// acos(w) = −j·ln(w + j·√(1 − w²)).
    pub fn acos(self) -> C {
        let one = C::real(1.0);
        let s = one.sub(self.mul(self)).sqrt();
        let v = self.add(C::new(-s.im, s.re)).ln();
        C::new(v.im, -v.re)
    }
}

impl Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

impl Div for C {
    type Output = C;
    fn div(self, o: C) -> C {
        let d = o.re * o.re + o.im * o.im;
        C::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

impl Neg for C {
    type Output = C;
    fn neg(self) -> C {
        C::new(-self.re, -self.im)
    }
}

/// Fixed-capacity root list.
#[derive(Clone, Copy, Debug)]
struct Roots {
    v: [C; MAX_ROOTS],
    n: usize,
}

impl Roots {
    const fn new() -> Roots {
        Roots { v: [C { re: 0.0, im: 0.0 }; MAX_ROOTS], n: 0 }
    }
    fn push(&mut self, c: C) {
        if self.n < MAX_ROOTS {
            self.v[self.n] = c;
            self.n += 1;
        }
    }
    fn as_slice(&self) -> &[C] {
        &self.v[..self.n]
    }
    fn prod_neg(&self) -> C {
        self.as_slice().iter().fold(C::real(1.0), |a, &r| a.mul(r.neg()))
    }
}

/// Analog zeros/poles/gain.
#[derive(Clone, Copy, Debug)]
struct Zpk {
    z: Roots,
    p: Roots,
    k: f64,
}

/// Prototype family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Bessel,
    Butterworth,
    Chebyshev,
    Elliptic,
}

/// Response type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    LowPass,
    HighPass,
    BandPass,
    BandStop,
}

/// A cascade of second-order sections.
#[derive(Clone, Copy, Debug)]
pub struct Sections {
    pub c: [Coeffs; MAX_SECTIONS],
    pub n: usize,
}

impl Sections {
    pub fn as_slice(&self) -> &[Coeffs] {
        &self.c[..self.n]
    }
    /// Magnitude of the cascade in dB.
    pub fn magnitude_db(&self, freq: f64, sample_rate: f64) -> f64 {
        self.as_slice().iter().map(|c| c.magnitude_db(freq, sample_rate)).sum()
    }
}

// ------------------------------------------------------------------------------- prototypes

fn butterworth(n: usize) -> Zpk {
    let mut p = Roots::new();
    for k in 0..n {
        let a = PI * (2 * k + n + 1) as f64 / (2 * n) as f64;
        p.push(C::new(a.cos(), a.sin()));
    }
    Zpk { z: Roots::new(), p, k: 1.0 }
}

fn chebyshev1(n: usize, rp: f64) -> Zpk {
    let eps = (10f64.powf(rp / 10.0) - 1.0).sqrt();
    let mu = (1.0 / eps).asinh() / n as f64;
    let mut p = Roots::new();
    for k in 0..n {
        let th = PI * (2 * k + 1) as f64 / (2 * n) as f64;
        p.push(C::new(-mu.sinh() * th.sin(), mu.cosh() * th.cos()));
    }
    let mut k = p.prod_neg().re;
    if n.is_multiple_of(2) {
        k /= (1.0 + eps * eps).sqrt();
    }
    Zpk { z: Roots::new(), p, k }
}

/// Roots of a monic polynomial (`c[i]` = coefficient of sⁱ, `c[n]` = 1) by Durand–Kerner.
fn poly_roots(c: &[f64], out: &mut Roots) {
    let n = c.len() - 1;
    let eval = |s: C| -> C {
        let mut acc = C::real(c[n]);
        for i in (0..n).rev() {
            acc = acc.mul(s).add(C::real(c[i]));
        }
        acc
    };
    // Initial guesses on a circle of the Cauchy-bound radius.
    let radius = 1.0 + c[..n].iter().fold(0.0f64, |m, v| m.max(v.abs())).powf(1.0 / n as f64);
    let mut r = [C::default(); MAX_ROOTS];
    for (i, ri) in r.iter_mut().enumerate().take(n) {
        let a = 2.0 * PI * i as f64 / n as f64 + 0.4;
        *ri = C::new(radius * a.cos(), radius * a.sin());
    }
    for _ in 0..500 {
        let mut delta = 0.0f64;
        for i in 0..n {
            let mut den = C::real(1.0);
            for j in 0..n {
                if j != i {
                    den = den.mul(r[i].sub(r[j]));
                }
            }
            let step = eval(r[i]).div(den);
            r[i] = r[i].sub(step);
            delta = delta.max(step.abs());
        }
        if delta < 1e-14 {
            break;
        }
    }
    out.n = 0;
    for &ri in r.iter().take(n) {
        out.push(ri);
    }
}

fn bessel(n: usize) -> Zpk {
    // Reverse Bessel polynomial: a_k = (2n − k)! / (2^(n−k) k! (n − k)!), monic (a_n = 1).
    let mut c = [0.0f64; MAX_ORDER + 1];
    let fact = |m: usize| -> f64 { (1..=m).fold(1.0, |a, v| a * v as f64) };
    for (k, ck) in c.iter_mut().enumerate().take(n + 1) {
        *ck = fact(2 * n - k) / (2f64.powi((n - k) as i32) * fact(k) * fact(n - k));
    }
    let mut p = Roots::new();
    poly_roots(&c[..=n], &mut p);
    // H(s) = a0 / θ(s); find Ω where |H(jΩ)| = 1/√2 and normalise it to 1.
    let a0 = c[0];
    let mag = |w: f64| -> f64 {
        let s = C::new(0.0, w);
        p.as_slice().iter().fold(C::real(1.0), |acc, &r| acc.mul(s.sub(r))).abs().recip() * a0
    };
    let (mut lo, mut hi) = (1e-3f64, 100.0f64);
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if mag(mid) > std::f64::consts::FRAC_1_SQRT_2 { lo = mid } else { hi = mid }
    }
    let w3 = (lo * hi).sqrt();
    for r in &mut p.v[..p.n] {
        *r = r.scale(1.0 / w3);
    }
    let k = p.prod_neg().re;
    Zpk { z: Roots::new(), p, k }
}

// ---- Jacobi elliptic functions via Landen transformations (normalised argument u·K).

const LANDEN_STEPS: usize = 10;

/// Descending Landen sequence of moduli.
fn landen(k: f64) -> ([f64; LANDEN_STEPS], usize) {
    let mut v = [0.0; LANDEN_STEPS];
    let mut n = 0;
    let mut k = k;
    while n < LANDEN_STEPS && k > 1e-16 {
        k = (k / (1.0 + (1.0 - k * k).max(0.0).sqrt())).powi(2);
        v[n] = k;
        n += 1;
    }
    (v, n)
}

/// Complete elliptic integral K(k).
#[cfg(test)]
fn ellip_k(k: f64) -> f64 {
    if k >= 1.0 {
        return f64::INFINITY;
    }
    let (v, n) = landen(k);
    v[..n].iter().fold(PI / 2.0, |a, &vi| a * (1.0 + vi))
}

/// cd(u·K, k) for complex normalised u.
fn cde(u: C, k: f64) -> C {
    let (v, n) = landen(k);
    let mut w = u.scale(PI / 2.0).cos();
    for &vi in v[..n].iter().rev() {
        w = w.scale(1.0 + vi).div(C::real(1.0).add(w.mul(w).scale(vi)));
    }
    w
}

/// sn(u·K, k) for complex normalised u.
fn sne(u: C, k: f64) -> C {
    let (v, n) = landen(k);
    let mut w = u.scale(PI / 2.0).sin();
    for &vi in v[..n].iter().rev() {
        w = w.scale(1.0 + vi).div(C::real(1.0).add(w.mul(w).scale(vi)));
    }
    w
}

/// Inverse of [`cde`]: normalised u with cd(u·K, k) = w.
fn acde(w: C, k: f64) -> C {
    let (v, n) = landen(k);
    let mut w = w;
    for i in 0..n {
        let v1 = if i == 0 { k } else { v[i - 1] };
        let s = C::real(1.0).sub(w.mul(w).scale(v1 * v1)).sqrt();
        w = w.div(C::real(1.0).add(s)).scale(2.0 / (1.0 + v[i]));
    }
    w.acos().scale(2.0 / PI)
}

/// Inverse of [`sne`]: sn(uK) = cd((1 − u)K).
fn asne(w: C, k: f64) -> C {
    C::real(1.0).sub(acde(w, k))
}

fn elliptic(n: usize, rp: f64, rs: f64) -> Zpk {
    let ep = (10f64.powf(rp / 10.0) - 1.0).sqrt();
    let es = (10f64.powf(rs / 10.0) - 1.0).sqrt();
    let k1 = ep / es;
    let k1p = (1.0 - k1 * k1).sqrt();
    let l = n / 2;
    // Degree equation: k' = k1'^N · Π sn⁴(u_i K', k1').
    let mut kp = k1p.powi(n as i32);
    for i in 1..=l {
        let ui = (2 * i - 1) as f64 / n as f64;
        kp *= sne(C::real(ui), k1p).re.powi(4);
    }
    let k = (1.0 - kp * kp).max(0.0).sqrt();
    // v0 = −j·asn(j/εp, k1) / N   (normalised).
    let a = asne(C::new(0.0, 1.0 / ep), k1);
    let v0 = C::new(a.im, -a.re).scale(1.0 / n as f64); // −j·a/N
    let mut z = Roots::new();
    let mut p = Roots::new();
    for i in 1..=l {
        let ui = (2 * i - 1) as f64 / n as f64;
        let zeta = cde(C::real(ui), k);
        let zi = C::new(0.0, 1.0).div(zeta.scale(k));
        z.push(zi);
        z.push(zi.conj());
        // p_i = j·cd((u_i − j v0)K, k)
        let arg = C::real(ui).sub(C::new(0.0, 1.0).mul(v0));
        let c = cde(arg, k);
        let pi = C::new(-c.im, c.re);
        p.push(pi);
        p.push(pi.conj());
    }
    if n % 2 == 1 {
        // p0 = j·sn(j v0 K, k)
        let s = sne(C::new(0.0, 1.0).mul(v0), k);
        p.push(C::new(-s.im, s.re));
        // the result is real (negative); drop the numerical imaginary residue
        let last = p.n - 1;
        p.v[last].im = 0.0;
        p.v[last].re = -p.v[last].re.abs();
    }
    // Make sure all poles are in the left half plane.
    for r in &mut p.v[..p.n] {
        if r.re > 0.0 {
            r.re = -r.re;
        }
    }
    // Gain: H(0) = 1 (odd N) or 1/√(1+εp²) (even N).
    let h0 = if n % 2 == 1 { 1.0 } else { 1.0 / (1.0 + ep * ep).sqrt() };
    let num = z.prod_neg();
    let den = p.prod_neg();
    let k = h0 * den.div(num).re;
    Zpk { z, p, k }
}

// ---------------------------------------------------------------------------- transforms

fn lp_to_lp(f: &mut Zpk, wc: f64) {
    for r in &mut f.z.v[..f.z.n] {
        *r = r.scale(wc);
    }
    for r in &mut f.p.v[..f.p.n] {
        *r = r.scale(wc);
    }
    f.k *= wc.powi((f.p.n - f.z.n) as i32);
}

fn lp_to_hp(f: &mut Zpk, wc: f64) {
    let ratio = f.z.prod_neg().div(f.p.prod_neg()).re;
    let degree = f.p.n - f.z.n;
    for r in &mut f.z.v[..f.z.n] {
        *r = C::real(wc).div(*r);
    }
    for r in &mut f.p.v[..f.p.n] {
        *r = C::real(wc).div(*r);
    }
    for _ in 0..degree {
        f.z.push(C::real(0.0));
    }
    f.k *= ratio;
}

fn lp_to_bp(f: &mut Zpk, w0: f64, bw: f64) {
    let degree = f.p.n - f.z.n;
    let map = |src: &Roots| -> Roots {
        let mut out = Roots::new();
        for &r in src.as_slice() {
            let a = r.scale(bw / 2.0);
            let d = a.mul(a).sub(C::real(w0 * w0)).sqrt();
            out.push(a.add(d));
            out.push(a.sub(d));
        }
        out
    };
    f.z = map(&f.z);
    f.p = map(&f.p);
    for _ in 0..degree {
        f.z.push(C::real(0.0));
    }
    f.k *= bw.powi(degree as i32);
}

fn lp_to_bs(f: &mut Zpk, w0: f64, bw: f64) {
    let degree = f.p.n - f.z.n;
    let ratio = f.z.prod_neg().div(f.p.prod_neg()).re;
    let map = |src: &Roots| -> Roots {
        let mut out = Roots::new();
        for &r in src.as_slice() {
            let a = C::real(bw / 2.0).div(r);
            let d = a.mul(a).sub(C::real(w0 * w0)).sqrt();
            out.push(a.add(d));
            out.push(a.sub(d));
        }
        out
    };
    f.z = map(&f.z);
    f.p = map(&f.p);
    for _ in 0..degree {
        f.z.push(C::new(0.0, w0));
        f.z.push(C::new(0.0, -w0));
    }
    f.k *= ratio;
}

fn bilinear(f: &mut Zpk, fs: f64) {
    let fs2 = 2.0 * fs;
    let degree = f.p.n - f.z.n;
    let num = f.z.as_slice().iter().fold(C::real(1.0), |a, &r| a.mul(C::real(fs2).sub(r)));
    let den = f.p.as_slice().iter().fold(C::real(1.0), |a, &r| a.mul(C::real(fs2).sub(r)));
    for r in &mut f.z.v[..f.z.n] {
        *r = C::real(fs2).add(*r).div(C::real(fs2).sub(*r));
    }
    for r in &mut f.p.v[..f.p.n] {
        *r = C::real(fs2).add(*r).div(C::real(fs2).sub(*r));
    }
    for _ in 0..degree {
        f.z.push(C::real(-1.0));
    }
    f.k *= num.div(den).re;
}

// ------------------------------------------------------------------------ factorisation

const REAL_EPS: f64 = 1e-7;

/// Split roots into conjugate pairs (upper half plane members) and real roots.
fn split(r: &Roots) -> (Roots, Roots) {
    let mut pairs = Roots::new();
    let mut reals = Roots::new();
    for &x in r.as_slice() {
        if x.im.abs() <= REAL_EPS * (1.0 + x.abs()) {
            reals.push(C::real(x.re));
        } else if x.im > 0.0 {
            pairs.push(x);
        }
    }
    (pairs, reals)
}

fn take(r: &mut Roots, i: usize) -> C {
    let v = r.v[i];
    r.v[i] = r.v[r.n - 1];
    r.n -= 1;
    v
}

fn closest(r: &Roots, to: C) -> Option<usize> {
    (0..r.n).min_by(|&a, &b| r.v[a].sub(to).abs().total_cmp(&r.v[b].sub(to).abs()))
}

fn take_closest(r: &mut Roots, to: C) -> C {
    let i = closest(r, to).unwrap_or(0);
    take(r, i)
}

fn into_sections(f: &Zpk) -> Sections {
    let (mut pp, mut pr) = split(&f.p);
    let (mut zp, mut zr) = split(&f.z);
    let mut out = Sections { c: [Coeffs::IDENTITY; MAX_SECTIONS], n: 0 };
    let mut push = |b: [f64; 3], a: [f64; 2]| {
        if out.n < MAX_SECTIONS {
            out.c[out.n] = Coeffs { b0: b[0], b1: b[1], b2: b[2], a1: a[0], a2: a[1] };
            out.n += 1;
        }
    };
    // An odd real pole first (it needs a single real zero).
    if pr.n % 2 == 1 {
        let i = (0..pr.n).max_by(|&a, &b| pr.v[a].re.abs().total_cmp(&pr.v[b].re.abs())).unwrap_or(0);
        let p = take(&mut pr, i);
        let b = match closest(&zr, p) {
            Some(j) => {
                let z = take(&mut zr, j);
                [1.0, -z.re, 0.0]
            }
            None => [1.0, 0.0, 0.0],
        };
        push(b, [-p.re, 0.0]);
    }
    // Second-order sections, highest-Q (closest to the unit circle) first.
    loop {
        let pole_pair = if pp.n > 0 {
            let i = (0..pp.n).max_by(|&a, &b| pp.v[a].abs().total_cmp(&pp.v[b].abs())).unwrap_or(0);
            let p = take(&mut pp, i);
            Some([-2.0 * p.re, p.re * p.re + p.im * p.im, p.re, p.im])
        } else if pr.n >= 2 {
            let p1 = take(&mut pr, 0);
            let p2 = take(&mut pr, 0);
            Some([-(p1.re + p2.re), p1.re * p2.re, (p1.re + p2.re) / 2.0, 0.0])
        } else {
            None
        };
        let Some([a1, a2, cre, cim]) = pole_pair else { break };
        let centre = C::new(cre, cim);
        // Zeros: best complex pair, else two reals, else none.
        let b = if let Some(j) = closest(&zp, centre) {
            let use_pair = zr.n < 2 || zp.v[j].sub(centre).abs() <= closest(&zr, centre).map_or(f64::INFINITY, |k| zr.v[k].sub(centre).abs());
            if use_pair {
                let z = take(&mut zp, j);
                [1.0, -2.0 * z.re, z.re * z.re + z.im * z.im]
            } else {
                let z1 = take_closest(&mut zr, centre);
                let z2 = take_closest(&mut zr, centre);
                [1.0, -(z1.re + z2.re), z1.re * z2.re]
            }
        } else if zr.n >= 2 {
            let z1 = take_closest(&mut zr, centre);
            let z2 = take_closest(&mut zr, centre);
            [1.0, -(z1.re + z2.re), z1.re * z2.re]
        } else if zr.n == 1 {
            let z1 = take(&mut zr, 0);
            [1.0, -z1.re, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        push(b, [a1, a2]);
    }
    // Overall gain on the first section.
    if out.n > 0 {
        out.c[0].b0 *= f.k;
        out.c[0].b1 *= f.k;
        out.c[0].b2 *= f.k;
    }
    out
}

/// Design parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub family: Family,
    pub band: Band,
    /// Prototype order (1..=[`MAX_ORDER`]).
    pub order: usize,
    /// Low-pass / high-pass edge, or the lower edge for band-pass / band-stop (Hz).
    pub f1: f64,
    /// Upper edge for band-pass / band-stop (Hz).
    pub f2: f64,
    /// Pass-band ripple (dB, Chebyshev / elliptic).
    pub ripple_db: f64,
    /// Stop-band attenuation (dB, elliptic).
    pub stop_db: f64,
}

/// A normalised analog low-pass prototype (cache it: Bessel needs a root search).
#[derive(Clone, Copy, Debug)]
pub struct Prototype(Zpk);

/// The analog prototype for a family, order, pass-band ripple and stop-band attenuation.
pub fn prototype(family: Family, order: usize, ripple_db: f64, stop_db: f64) -> Prototype {
    let n = order.clamp(1, MAX_ORDER);
    let rp = if ripple_db.is_nan() { 1.0 } else { ripple_db.clamp(0.01, 10.0) };
    let rs = if stop_db.is_nan() { 60.0 } else { stop_db.clamp(rp + 1.0, 150.0) };
    Prototype(match family {
        Family::Butterworth => butterworth(n),
        Family::Bessel => bessel(n),
        Family::Chebyshev => chebyshev1(n, rp),
        Family::Elliptic => elliptic(n, rp, rs),
    })
}

/// Transform a prototype to `band` at `f1` (and `f2` for band-pass/stop) and digitise it.
pub fn realize(proto: &Prototype, band: Band, f1: f64, f2: f64, sample_rate: f64) -> Sections {
    let mut f = proto.0;
    // a non-positive / non-finite rate or NaN edge would cross the clamp bounds
    let sample_rate = if sample_rate.is_finite() && sample_rate > 2.1 { sample_rate } else { 48_000.0 };
    let (f1, f2) = (if f1.is_nan() { 1000.0 } else { f1 }, if f2.is_nan() { 1000.0 } else { f2 });
    let nyq = sample_rate * 0.5;
    let warp = |hz: f64| 2.0 * sample_rate * (PI * hz.clamp(1.0, nyq * 0.98) / sample_rate).tan();
    match band {
        Band::LowPass => lp_to_lp(&mut f, warp(f1)),
        Band::HighPass => lp_to_hp(&mut f, warp(f1)),
        Band::BandPass | Band::BandStop => {
            let (lo, hi) = if f1 <= f2 { (f1, f2) } else { (f2, f1) };
            let hi = hi.max(lo * 1.001);
            let (w1, w2) = (warp(lo), warp(hi));
            let (w0, bw) = ((w1 * w2).sqrt(), (w2 - w1).max(1e-6));
            if band == Band::BandPass { lp_to_bp(&mut f, w0, bw) } else { lp_to_bs(&mut f, w0, bw) }
        }
    }
    bilinear(&mut f, sample_rate);
    into_sections(&f)
}

/// Design a digital filter at `sample_rate`.
pub fn design(spec: &Spec, sample_rate: f64) -> Sections {
    realize(&prototype(spec.family, spec.order, spec.ripple_db, spec.stop_db), spec.band, spec.f1, spec.f2, sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48000.0;

    fn spec(family: Family, band: Band, order: usize, f1: f64, f2: f64) -> Spec {
        Spec { family, band, order, f1, f2, ripple_db: 1.0, stop_db: 60.0 }
    }

    #[test]
    fn butterworth_matches_closed_form() {
        // |H| = 1/√(1 + (Ωa/Ωc)^(2N)) with pre-warped analog frequencies.
        for n in 1..=8 {
            let s = design(&spec(Family::Butterworth, Band::LowPass, n, 1000.0, 0.0), SR);
            let wa = |f: f64| (PI * f / SR).tan();
            for f in [100.0, 500.0, 1000.0, 2000.0, 8000.0] {
                let want = -10.0 * (1.0 + (wa(f) / wa(1000.0)).powi(2 * n as i32)).log10();
                let got = s.magnitude_db(f, SR);
                assert!((got - want).abs() < 1e-6, "n={n} f={f}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn bessel_is_minus_3db_at_cutoff_and_monotonic() {
        for n in 1..=10 {
            let s = design(&spec(Family::Bessel, Band::LowPass, n, 2000.0, 0.0), SR);
            assert!((s.magnitude_db(2000.0, SR) + 3.0103).abs() < 0.02, "n={n}: {}", s.magnitude_db(2000.0, SR));
            assert!(s.magnitude_db(1.0, SR).abs() < 1e-5, "n={n}: {}", s.magnitude_db(1.0, SR));
            let mut last = 1.0;
            for i in 1..200 {
                let m = s.magnitude_db(i as f64 * 100.0, SR);
                assert!(m <= last + 1e-9, "Bessel must be monotonic");
                last = m;
            }
        }
    }

    #[test]
    fn chebyshev_ripple_bounds() {
        for n in 2..=8 {
            let s = design(&Spec { ripple_db: 0.5, ..spec(Family::Chebyshev, Band::LowPass, n, 1000.0, 0.0) }, SR);
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for i in 1..100 {
                let m = s.magnitude_db(i as f64 * 10.0, SR);
                min = min.min(m);
                max = max.max(m);
            }
            assert!(max < 1e-6 && min > -0.5 - 1e-6, "n={n} {min}..{max}");
            assert!(min < -0.45, "ripple reaches the bound (n={n}: {min})");
            assert!((s.magnitude_db(1000.0, SR) + 0.5).abs() < 1e-3);
        }
    }

    #[test]
    fn elliptic_meets_ripple_and_stopband() {
        for n in 2..=8 {
            for (rp, rs) in [(1.0, 60.0), (0.1, 40.0), (0.5, 80.0)] {
                let s = design(&Spec { ripple_db: rp, stop_db: rs, ..spec(Family::Elliptic, Band::LowPass, n, 1000.0, 0.0) }, SR);
                let mut max = f64::NEG_INFINITY;
                let mut min = f64::INFINITY;
                for i in 1..=100 {
                    let m = s.magnitude_db(i as f64 * 10.0, SR);
                    min = min.min(m);
                    max = max.max(m);
                }
                assert!(max < 1e-6 && min > -rp - 1e-3, "n={n} rp={rp}: {min}..{max}");
                // Once the response first drops below −As it never rises above it again.
                let mut below = false;
                for i in 1..2390 {
                    let f = i as f64 * 10.0;
                    let m = s.magnitude_db(f, SR);
                    if m < -rs {
                        below = true;
                    }
                    if below {
                        assert!(m < -rs + 0.05, "n={n} rs={rs} f={f}: {m}");
                    }
                }
                assert!(below, "n={n} rs={rs}: stop band reached");
            }
        }
        // Sharper than Butterworth of the same order.
        let e = design(&spec(Family::Elliptic, Band::LowPass, 4, 1000.0, 0.0), SR);
        let b = design(&spec(Family::Butterworth, Band::LowPass, 4, 1000.0, 0.0), SR);
        assert!(e.magnitude_db(1500.0, SR) < b.magnitude_db(1500.0, SR) - 10.0);
    }

    #[test]
    fn highpass_bandpass_bandstop_shapes() {
        for fam in [Family::Butterworth, Family::Chebyshev, Family::Elliptic, Family::Bessel] {
            let hp = design(&spec(fam, Band::HighPass, 4, 1000.0, 0.0), SR);
            assert!(hp.magnitude_db(10000.0, SR) > -1.1, "{fam:?} hp pass");
            assert!(hp.magnitude_db(100.0, SR) < -40.0, "{fam:?} hp stop");
            let bp = design(&spec(fam, Band::BandPass, 3, 500.0, 2000.0), SR);
            assert!(bp.magnitude_db(1000.0, SR) > -3.5, "{fam:?} bp centre {}", bp.magnitude_db(1000.0, SR));
            assert!(bp.magnitude_db(50.0, SR) < -30.0 && bp.magnitude_db(15000.0, SR) < -25.0, "{fam:?} bp stop");
            let bs = design(&spec(fam, Band::BandStop, 3, 500.0, 2000.0), SR);
            assert!(bs.magnitude_db(1000.0, SR) < -20.0, "{fam:?} bs centre {}", bs.magnitude_db(1000.0, SR));
            assert!(bs.magnitude_db(30.0, SR) > -1.1 && bs.magnitude_db(18000.0, SR) > -1.1, "{fam:?} bs pass");
        }
    }

    #[test]
    fn complex_helpers() {
        let w = C::new(0.3, -0.7);
        let back = w.acos().cos();
        assert!(back.sub(w).abs() < 1e-12);
        assert!((C::new(-4.0, 0.0).sqrt().im - 2.0).abs() < 1e-12);
        assert!((ellip_k(0.0) - PI / 2.0).abs() < 1e-12);
        // sn(K, k) = 1; cd(0, k) = 1
        assert!((sne(C::real(1.0), 0.7).re - 1.0).abs() < 1e-9);
        assert!((cde(C::real(0.0), 0.7).re - 1.0).abs() < 1e-9);
        let u = asne(C::real(0.5), 0.7);
        assert!((sne(u, 0.7).re - 0.5).abs() < 1e-9);
    }
}
