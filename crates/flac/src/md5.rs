//! MD5 (RFC 1321), for the STREAMINFO signature of the unencoded audio (RFC 9639 §8.2): every
//! sample, interleaved, little-endian, in the fewest whole bytes that hold the sample size.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11,
    16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// `floor(abs(sin(i + 1)) × 2³²)`, RFC 1321 §3.4.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122,
    0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6,
    0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60,
    0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    block: [u8; 64],
    filled: usize,
    len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Md5 { state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], block: [0; 64], filled: 0, len: 0 }
    }
}

impl Md5 {
    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            let (head, rest) = data.split_at(take);
            if let Some(dst) = self.block.get_mut(self.filled..self.filled + take) {
                dst.copy_from_slice(head);
            }
            self.filled += take;
            data = rest;
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (w, c) in m.iter_mut().zip(block.as_chunks::<4>().0) {
            *w = u32::from_le_bytes(*c);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }

    /// The digest so far (the state is not consumed: more data may follow).
    pub fn digest(&self) -> [u8; 16] {
        let mut h = self.clone();
        let bits = h.len.wrapping_mul(8);
        h.update(&[0x80]);
        while h.filled != 56 {
            h.update(&[0]);
        }
        h.update(&bits.to_le_bytes());
        let mut out = [0u8; 16];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(h.state) {
            o.copy_from_slice(&s.to_le_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc_1321_test_suite() {
        let md5 = |s: &[u8]| {
            let mut m = Md5::default();
            m.update(s);
            hex(m.digest())
        };
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5(b"message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"), "57edf4a22be3c955ac49da2e2107b67a");
        // fed in pieces across block boundaries
        let mut m = Md5::default();
        for c in b"12345678901234567890123456789012345678901234567890123456789012345678901234567890".chunks(7) {
            m.update(c);
        }
        assert_eq!(hex(m.digest()), "57edf4a22be3c955ac49da2e2107b67a");
    }
}
