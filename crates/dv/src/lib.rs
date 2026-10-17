//! DV25 525/60 decoder, implemented from SMPTE 314M-2005 sections 4.7 and 5.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod vlc;
use std::{f64::consts::PI, sync::OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

/// Original 720x480 picture, with 180x480 chroma planes (4:1:1), no scaling or deinterlacing.
pub struct Frame {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
}

// Figure 27: one-based coefficient output order, expressed at raster positions.
const ORDER: [[u8; 64]; 2] = [
    [
        1, 2, 6, 7, 15, 16, 28, 29, 3, 5, 8, 14, 17, 27, 30, 43, 4, 9, 13, 18, 26, 31, 42, 44, 10, 12, 19, 25, 32, 41, 45, 54, 11, 20, 24, 33, 40, 46, 53, 55,
        21, 23, 34, 39, 47, 52, 56, 61, 22, 35, 38, 48, 51, 57, 60, 62, 36, 37, 49, 50, 58, 59, 63, 64,
    ],
    [
        1, 3, 7, 19, 21, 35, 37, 51, 5, 9, 17, 23, 33, 39, 49, 53, 11, 15, 25, 31, 41, 47, 55, 61, 13, 27, 29, 43, 45, 57, 59, 63, 2, 4, 8, 20, 22, 36, 38, 52,
        6, 10, 18, 24, 34, 40, 50, 54, 12, 16, 26, 32, 42, 48, 56, 62, 14, 28, 30, 44, 46, 58, 60, 64,
    ],
];
const AREA: [[usize; 64]; 2] = [
    [
        0, 0, 0, 1, 1, 1, 2, 2, 0, 0, 1, 1, 1, 2, 2, 2, 0, 1, 1, 1, 2, 2, 2, 3, 1, 1, 1, 2, 2, 2, 3, 3, 1, 1, 2, 2, 2, 3, 3, 3, 1, 2, 2, 2, 3, 3, 3, 3, 2, 2,
        2, 3, 3, 3, 3, 3, 2, 2, 3, 3, 3, 3, 3, 3,
    ],
    [
        0, 0, 1, 1, 1, 2, 2, 3, 0, 1, 1, 2, 2, 2, 3, 3, 1, 1, 2, 2, 2, 3, 3, 3, 1, 2, 2, 2, 3, 3, 3, 3, 0, 0, 1, 1, 2, 2, 2, 3, 0, 1, 1, 2, 2, 2, 3, 3, 1, 1,
        2, 2, 2, 3, 3, 3, 1, 2, 2, 3, 3, 3, 3, 3,
    ],
];

#[derive(Clone, Copy, Default)]
struct Token {
    run: u8,
    amp: i16,
    len: usize,
    eob: bool,
}
fn tokens() -> &'static [Token] {
    static TABLE: OnceLock<Vec<Token>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![Token::default(); 65536];
        let mut add = |code: usize, len: usize, run: u8, amp: i16, eob: bool| {
            let start = code << (16 - len);
            let end = (code + 1) << (16 - len);
            for t in &mut table[start..end] {
                *t = Token { run, amp, len, eob };
            }
        };
        for &(run, amp, code, len, sign) in vlc::VLC {
            if sign {
                for s in 0..2 {
                    add((code as usize) * 2 + s, len as usize + 1, run, if s == 0 { amp as i16 } else { -(amp as i16) }, false);
                }
            } else {
                add(code as usize, len as usize, run, amp as i16, false);
            }
        }
        for run in 0..64 {
            add((0b1111110 << 6) | run, 13, run as u8, 0, false);
        }
        for amp in 0..256 {
            for s in 0..2 {
                add((0b1111111 << 9) | (amp << 1) | s, 16, 0, if s == 0 { amp as i16 } else { -(amp as i16) }, false);
            }
        }
        add(0b0110, 4, 0, 0, true);
        table
    })
}

struct Block {
    bits: Vec<u8>,
    cursor: usize,
    coeff: [i16; 64],
    scan: usize,
    mode: usize,
    class: usize,
    q: usize,
    done: bool,
}
impl Block {
    fn new(data: &[u8], q: usize) -> Result<Self> {
        if data.len() < 2 {
            return Err(Error("truncated DV DCT block"));
        }
        let header = u16::from_be_bytes([data[0], data[1]]);
        let dc = (header >> 7) as i16;
        let dc = if dc & 256 != 0 { dc - 512 } else { dc };
        let mut coeff = [0; 64];
        coeff[0] = dc;
        let bits = (12..data.len() * 8).map(|p| (data[p / 8] >> (7 - p % 8)) & 1).collect();
        Ok(Self { bits, cursor: 0, coeff, scan: 0, mode: ((header >> 6) & 1) as usize, class: ((header >> 4) & 3) as usize, q, done: false })
    }
    fn decode(&mut self) -> Result<()> {
        // Every complete token advances at least one bit; each block has 63 AC coefficients.
        while !self.done && self.cursor < self.bits.len() {
            let mut code = 0usize;
            for k in 0..16 {
                code = (code << 1) | self.bits.get(self.cursor + k).copied().unwrap_or(0) as usize;
            }
            let t = tokens()[code];
            if t.len == 0 {
                if self.bits.len() - self.cursor < 16 {
                    break;
                }
                return Err(Error("invalid DV VLC"));
            }
            if self.bits.len() - self.cursor < t.len {
                break;
            }
            self.cursor += t.len;
            if t.eob {
                self.done = true;
                break;
            }
            self.scan += t.run as usize + 1;
            if self.scan >= 64 {
                return Err(Error("DV coefficient run exceeds block"));
            }
            let raster = ORDER[self.mode].iter().position(|&n| n as usize == self.scan + 1).ok_or(Error("invalid DV scan"))?;
            self.coeff[raster] = t.amp;
        }
        Ok(())
    }
    fn continue_from(&mut self, pool: &[u8], cursor: &mut usize) -> Result<()> {
        if self.done {
            return Ok(());
        }
        let old_len = self.bits.len();
        let pending = old_len - self.cursor;
        self.bits.extend_from_slice(pool.get(*cursor..).ok_or(Error("invalid DV bit pool"))?);
        let before = self.cursor;
        self.decode()?;
        *cursor += self.cursor.saturating_sub(before).saturating_sub(pending);
        if !self.done {
            *cursor = pool.len();
        }
        if self.done {
            self.bits.truncate(self.cursor.max(old_len));
        }
        Ok(())
    }
}

struct Transform {
    inverse: [[f64; 64]; 2],
    cos8: [[f64; 8]; 8],
    cos4: [[f64; 4]; 4],
}
fn transform() -> &'static Transform {
    static T: OnceLock<Transform> = OnceLock::new();
    T.get_or_init(|| {
        let c = |n: usize| (n as f64 * PI / 16.0).cos();
        let w = [1.0, c(4) / (4.0 * c(7) * c(2)), c(4) / (2.0 * c(6)), 1.0 / (2.0 * c(5)), 7.0 / 8.0, c(4) / c(3), c(4) / c(2), c(4) / c(1)];
        let mut inverse = [[0.0; 64]; 2];
        for mode in 0..2 {
            for v in 0..8 {
                for h in 0..8 {
                    inverse[mode][v * 8 + h] = if v == 0 && h == 0 { 4.0 } else { 2.0 / (w[h] * w[if mode == 0 { v } else { 2 * (v % 4) }]) };
                }
            }
        }
        let cos8 = std::array::from_fn(|x| {
            std::array::from_fn(|h| ((2 * x + 1) as f64 * h as f64 * PI / 16.0).cos() * if h == 0 { 0.5 / std::f64::consts::SQRT_2 } else { 0.5 })
        });
        let cos4 = std::array::from_fn(|z| {
            std::array::from_fn(|u| ((2 * z + 1) as f64 * u as f64 * PI / 8.0).cos() * if u == 0 { 0.5 / std::f64::consts::SQRT_2 } else { 0.5 })
        });
        Transform { inverse, cos8, cos4 }
    })
}
fn pixels(b: &Block) -> [u8; 64] {
    let index = b.q as i32 - [0, 3, 6, 5][b.class];
    let steps = if index >= 9 {
        [1, 1, 1, 1]
    } else if index == 8 {
        [1, 1, 1, 2]
    } else if index >= 6 {
        [1, 1, 2, 2]
    } else if index >= 4 {
        [1, 2, 2, 4]
    } else if index >= 2 {
        [2, 2, 4, 4]
    } else if index >= 0 {
        [2, 4, 4, 8]
    } else if index >= -2 {
        [4, 4, 8, 8]
    } else if index >= -4 {
        [4, 8, 8, 16]
    } else {
        [8, 8, 16, 16]
    };
    let t = transform();
    let mut coeff = [0.0; 64];
    for i in 0..64 {
        coeff[i] = b.coeff[i] as f64 * t.inverse[b.mode][i] * if i == 0 { 1.0 } else { (steps[AREA[b.mode][i]] * (if b.class == 3 { 2 } else { 1 })) as f64 };
    }
    let mut tmp = [[0.0; 8]; 8];
    for v in 0..8 {
        for x in 0..8 {
            for h in 0..8 {
                tmp[v][x] += coeff[v * 8 + h] * t.cos8[x][h];
            }
        }
    }
    std::array::from_fn(|i| {
        let (x, y) = (i % 8, i / 8);
        let mut value = 0.0;
        if b.mode == 0 {
            for v in 0..8 {
                value += tmp[v][x] * t.cos8[y][v];
            }
        } else {
            for u in 0..4 {
                value += (tmp[u][x] + if y % 2 == 0 { tmp[u + 4][x] } else { -tmp[u + 4][x] }) * t.cos4[y / 2][u];
            }
        }
        (value + 128.0).round().clamp(0.0, 255.0) as u8
    })
}

// Figures 22 and 26: alternating 6-high serpentine superblocks; rightmost is clipped.
fn location(sequence: usize, segment: usize, mb: usize) -> (usize, usize) {
    let j = [2, 1, 3, 0, 4][mb];
    let i = (sequence + [2, 6, 8, 0, 4][mb]) % 10;
    let (col, row) = if j == 1 || j == 3 {
        if segment < 3 {
            (0, segment + 3)
        } else {
            let c = (segment + 3) / 6;
            let r = (segment + 3) % 6;
            (c, if c % 2 == 1 { 5 - r } else { r })
        }
    } else {
        let c = segment / 6;
        let r = segment % 6;
        (c, if c % 2 == 1 { 5 - r } else { r })
    };
    let x = [0, 128, 288, 416, 576][j] + col * 32;
    let y = i * 48 + if x == 704 { row * 16 } else { row * 8 };
    (x, y)
}
fn put(plane: &mut [u8], stride: usize, x: usize, y: usize, data: &[u8; 64], right: bool) -> Result<()> {
    for row in 0..8 {
        for col in 0..8 {
            let (dx, dy) = if right { (col % 4, row + if col >= 4 { 8 } else { 0 }) } else { (col, row) };
            let index = (y + dy) * stride + x + dx;
            let target = plane.get_mut(index).ok_or(Error("DV block outside picture"))?;
            *target = data[row * 8 + col];
        }
    }
    Ok(())
}

/// Decode one complete DV25 525/60 DIF frame. All bit pools and dimensions are bounded.
pub fn decode_frame(data: &[u8]) -> Result<Frame> {
    if data.len() != 120000 {
        return Err(Error("unsupported DV frame size (requires 525/60 DV25)"));
    }
    if data[0] >> 5 != 0 || data[3] & 0x80 != 0 {
        return Err(Error("invalid or unsupported DV DIF header"));
    }
    let mut frame = Frame { y: vec![16; 720 * 480], cb: vec![128; 180 * 480], cr: vec![128; 180 * 480] };
    for sequence in 0..10 {
        for segment in 0..27 {
            let mut blocks = Vec::with_capacity(30);
            for mb in 0..5 {
                let video = segment * 5 + mb;
                let dif = sequence * 150 + 7 + video + video / 15;
                let d = data.get(dif * 80..dif * 80 + 80).ok_or(Error("truncated DV DIF block"))?;
                if d[0] >> 5 != 4 || d[1] >> 4 != sequence as u8 || d[2] as usize != video {
                    return Err(Error("invalid DV video DIF ID"));
                }
                if d[3] & 0xf == 15 && d[3] >> 4 == 15 {
                    return Err(Error("damaged DV macroblock"));
                }
                let mut offset = 4;
                for b in 0..6 {
                    let n = if b < 4 { 14 } else { 10 };
                    let mut block = Block::new(&d[offset..offset + n], (d[3] & 15) as usize)?;
                    block.decode()?;
                    blocks.push(block);
                    offset += n;
                }
            }
            let mut video_pool = Vec::new();
            for mb in 0..5 {
                let chunk = &mut blocks[mb * 6..mb * 6 + 6];
                let mut pool = Vec::new();
                for b in chunk.iter() {
                    if b.done {
                        pool.extend_from_slice(&b.bits[b.cursor..]);
                    }
                }
                let mut cursor = 0;
                for b in chunk.iter_mut() {
                    b.continue_from(&pool, &mut cursor)?;
                }
                video_pool.extend_from_slice(&pool[cursor..]);
            }
            let mut cursor = 0;
            for b in &mut blocks {
                b.continue_from(&video_pool, &mut cursor)?;
                if !b.done {
                    return Err(Error("DV AC data lacks end of block"));
                }
            }
            for mb in 0..5 {
                let (x, y) = location(sequence, segment, mb);
                for b in 0..6 {
                    let p = pixels(&blocks[mb * 6 + b]);
                    if b < 4 {
                        let (dx, dy) = if x == 704 { ((b % 2) * 8, (b / 2) * 8) } else { (b * 8, 0) };
                        put(&mut frame.y, 720, x + dx, y + dy, &p, false)?;
                    } else {
                        put(if b == 4 { &mut frame.cr } else { &mut frame.cb }, 180, x / 4, y, &p, x == 704)?;
                    }
                }
            }
        }
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_never_panics() {
        for n in [0, 1, 80, 119999, 120000, 144000] {
            let d = vec![0; n];
            assert!(std::panic::catch_unwind(|| decode_frame(&d)).is_ok());
            assert!(decode_frame(&d).is_err());
        }
    }
    #[test]
    fn layout_covers_picture_once() {
        let mut coverage = vec![0; 720 * 480];
        for s in 0..10 {
            for k in 0..27 {
                for m in 0..5 {
                    let (x, y) = location(s, k, m);
                    let (w, h) = if x == 704 { (16, 16) } else { (32, 8) };
                    for dy in 0..h {
                        for dx in 0..w {
                            coverage[(y + dy) * 720 + x + dx] += 1;
                        }
                    }
                }
            }
        }
        assert!(coverage.iter().all(|&n| n == 1));
    }
}
