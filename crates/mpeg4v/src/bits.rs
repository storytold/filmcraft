//! Big-endian bit reader over one VOP. Reads past the end return zero bits; callers check
//! [`Bits::overrun`] to detect truncated data.

#[derive(Clone)]
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The next 32 bits, MSB first (zeros past the end).
    #[inline(always)]
    pub fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let w = if let Some(&b) = self.data.get(byte..).and_then(|d| d.first_chunk::<8>()) {
            u64::from_be_bytes(b)
        } else {
            let mut b = [0u8; 8];
            if let Some(rest) = self.data.get(byte..) {
                for (o, v) in b.iter_mut().zip(rest) {
                    *o = *v;
                }
            }
            u64::from_be_bytes(b)
        };
        ((w << (self.pos & 7)) >> 32) as u32
    }

    /// The next `n` (1..=32) bits without consuming them.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        ((self.peek32() as u64) >> (32 - n)) as u32
    }

    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.pos = self.pos.saturating_add(n as usize);
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.read(1) == 1
    }

    /// Position in bits from the start of the data.
    #[inline(always)]
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn bits_left(&self) -> isize {
        (self.data.len().saturating_mul(8) as isize).saturating_sub(self.pos as isize)
    }

    /// Bits have been read past the end: the data is truncated or corrupt.
    #[inline(always)]
    pub fn overrun(&self) -> bool {
        self.bits_left() < 0
    }

    pub fn len_bytes(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_bytes_and_past_the_end() {
        let d = [0b1010_1100, 0xFF, 0x01];
        let mut b = Bits::new(&d);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(7), 0b011_0011);
        assert_eq!(b.peek(4), 0xF);
        assert_eq!(b.read(14), 0b11_1111_0000_0001);
        assert!(!b.overrun());
        assert_eq!(b.read(8), 0);
        assert!(b.overrun());
    }
}
