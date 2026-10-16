//! MSB-first bit reader for OBU headers and the uncompressed frame header (spec 4.10 descriptors).

use crate::{Error, Result};

#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }

    #[inline]
    pub fn bit(&mut self) -> Result<u32> {
        let byte = *self.data.get(self.pos >> 3).ok_or(Error::Truncated)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    /// `f(n)`, n ≤ 32.
    pub fn f(&mut self, n: u32) -> Result<u32> {
        let mut x: u64 = 0;
        for _ in 0..n {
            x = (x << 1) | self.bit()? as u64;
        }
        Ok(x as u32)
    }

    pub fn flag(&mut self) -> Result<bool> {
        Ok(self.bit()? == 1)
    }

    /// `uvlc()`
    pub fn uvlc(&mut self) -> Result<u32> {
        let mut leading = 0u32;
        while self.bit()? == 0 {
            leading += 1;
            if leading > 40 {
                return Err(Error::Invalid("uvlc"));
            }
        }
        if leading >= 32 {
            return Ok(u32::MAX);
        }
        let v = self.f(leading)?;
        Ok(v + ((1u64 << leading) - 1) as u32)
    }

    /// `su(n)`
    pub fn su(&mut self, n: u32) -> Result<i32> {
        let v = self.f(n)? as i64;
        let sign = 1i64 << (n - 1);
        Ok(if v & sign != 0 { (v - 2 * sign) as i32 } else { v as i32 })
    }

    /// `ns(n)`
    pub fn ns(&mut self, n: u32) -> Result<u32> {
        if n <= 1 {
            return Ok(0);
        }
        let w = 32 - n.leading_zeros(); // FloorLog2(n) + 1
        let m = (1u32 << w) - n;
        let v = self.f(w - 1)?;
        if v < m {
            return Ok(v);
        }
        let extra = self.bit()?;
        Ok((v << 1) - m + extra)
    }

    /// `byte_alignment()`
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// Read a leb128 from a byte slice; returns (value, bytes used).
pub(crate) fn leb128(data: &[u8]) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(i).ok_or(Error::Truncated)? as u64;
        v |= (b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Ok((v, 8))
}
