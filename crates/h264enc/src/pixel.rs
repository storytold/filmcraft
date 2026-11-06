//! Sample types: `u8` for 8-bit pictures, `u16` for 9- and 10-bit (High 10) pictures.
//!
//! The encoder is generic over [`Pixel`], so the 8-bit path is monomorphised to the same code it had
//! before high bit depth existed. Intermediate precision is chosen per type: the 6-tap half-sample
//! filter needs 16 bits for 8-bit samples and 32 bits above that ([`Pixel::Tap`]). SATD lanes and SAD
//! accumulators stay 16-bit, which holds for up to 12-bit samples; the encoder accepts at most 10.

/// A picture sample.
pub trait Pixel: Copy + Default + PartialEq + Send + Sync + std::fmt::Debug + 'static {
    /// Unclipped horizontal 6-tap intermediate of the half-sample filter.
    type Tap: Copy + Default + Send + Sync;
    /// The sample value (always non-negative).
    fn get(self) -> i32;
    /// A sample from `v`, which the caller has already clipped to the bit depth.
    fn of(v: i32) -> Self;
    fn untap(t: Self::Tap) -> i32;
    /// `v` clipped to 0..=`d.max` (the constant 255 for `u8`, so the 8-bit path keeps its old code).
    fn clip(v: i32, d: Depth) -> Self;
    /// |a - b| (fits 16 bits up to 16-bit samples).
    fn ad(self, o: Self) -> u16;
    /// a - b in 16 bits (exact up to 15-bit samples).
    fn diff(self, o: Self) -> i16;
    /// The horizontal half-sample filter tap of six consecutive samples, unclipped and unshifted.
    fn tap6(e: &[Self; 6]) -> Self::Tap;
    /// (a + b + 1) >> 1.
    fn avg(self, o: Self) -> Self;
    /// Store 8-bit samples scaled to the bit depth (v << `shift`).
    fn from_u8(dst: &mut [Self], src: &[u8], shift: u32);
    /// Write the samples as bytes: one per sample for `u8`, two little-endian ones for `u16`
    /// (the `yuv420p10le` layout).
    fn extend_bytes(src: &[Self], out: &mut Vec<u8>);
}

impl Pixel for u8 {
    type Tap = i16;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn of(v: i32) -> Self {
        v as u8
    }
    #[inline(always)]
    fn untap(t: i16) -> i32 {
        t as i32
    }
    #[inline(always)]
    fn clip(v: i32, _: Depth) -> u8 {
        v.clamp(0, 255) as u8
    }
    #[inline(always)]
    fn ad(self, o: u8) -> u16 {
        self.abs_diff(o) as u16
    }
    #[inline(always)]
    fn diff(self, o: u8) -> i16 {
        self as i16 - o as i16
    }
    #[inline(always)]
    fn tap6(e: &[u8; 6]) -> i16 {
        (e[0] as i16 + e[5] as i16) - 5 * (e[1] as i16 + e[4] as i16) + 20 * (e[2] as i16 + e[3] as i16)
    }
    #[inline(always)]
    fn avg(self, o: u8) -> u8 {
        ((self as u16 + o as u16 + 1) >> 1) as u8
    }
    fn from_u8(dst: &mut [u8], src: &[u8], _: u32) {
        let n = dst.len().min(src.len());
        dst[..n].copy_from_slice(&src[..n]);
    }
    fn extend_bytes(src: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(src);
    }
}

impl Pixel for u16 {
    type Tap = i32;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn of(v: i32) -> Self {
        v as u16
    }
    #[inline(always)]
    fn untap(t: i32) -> i32 {
        t
    }
    #[inline(always)]
    fn clip(v: i32, d: Depth) -> u16 {
        v.clamp(0, d.max) as u16
    }
    #[inline(always)]
    fn ad(self, o: u16) -> u16 {
        self.abs_diff(o)
    }
    #[inline(always)]
    fn diff(self, o: u16) -> i16 {
        self as i16 - o as i16
    }
    #[inline(always)]
    fn tap6(e: &[u16; 6]) -> i32 {
        let e = e.map(i32::from);
        (e[0] + e[5]) - 5 * (e[1] + e[4]) + 20 * (e[2] + e[3])
    }
    #[inline(always)]
    fn avg(self, o: u16) -> u16 {
        ((self as u32 + o as u32 + 1) >> 1) as u16
    }
    fn from_u8(dst: &mut [u16], src: &[u8], shift: u32) {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d = (s as u16) << shift;
        }
    }
    fn extend_bytes(src: &[u16], out: &mut Vec<u8>) {
        out.reserve(src.len() * 2);
        for s in src {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
}

/// Sample-range parameters of one stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Depth {
    /// BitDepth (8..=10).
    pub bits: u8,
    /// Largest sample value, (1 << bits) - 1.
    pub max: i32,
    /// Neutral value used for unavailable intra neighbours, 1 << (bits - 1).
    pub mid: i32,
    /// QpBdOffset = 6 * (bits - 8): QP' = QP + offset.
    pub qp_offset: i32,
    /// bits - 8: how much larger distortions are than at 8 bits (SAD/SATD scale by 2^shift).
    pub shift: u32,
}

impl Depth {
    pub const fn new(bits: u8) -> Self {
        let shift = bits.saturating_sub(8) as u32;
        Depth { bits, max: (1 << bits) - 1, mid: 1 << bits.saturating_sub(1), qp_offset: 6 * shift as i32, shift }
    }
}

impl Default for Depth {
    fn default() -> Self {
        Depth::new(8)
    }
}

/// Largest QP' the tables cover (QP 51 at 10-bit: 51 + 12).
pub const QP_PRIME_MAX: usize = 63;
