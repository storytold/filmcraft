//! DNG opcode lists (DNG 1.7.1.0 chapter 7, "Opcode List Processing"): OpcodeList1 on the raw
//! samples as stored, OpcodeList2 on the linear reference values (after linearisation, black and
//! white levels, before white balance), OpcodeList3 on the demosaiced picture.
//!
//! An opcode list is big-endian whatever the file's byte order: a count, then per opcode its ID,
//! the DNG version that defined it, flags (bit 0 optional, bit 1 skippable for preview quality)
//! and the size of its parameters. Implemented: WarpRectilinear (1), WarpFisheye (2),
//! FixVignetteRadial (3), FixBadPixelsConstant (4), FixBadPixelsList (5), TrimBounds (6),
//! MapTable (7), MapPolynomial (8), GainMap (9), DeltaPerRow / Column (10, 11), ScalePerRow /
//! Column (12, 13) and WarpRectilinear2 (14, with the skip rule for a following warp). An unknown
//! opcode, or one from a DNG version newer than 1.7.1.0, is skipped when it is optional and an
//! error when it is not. Values are clipped to the stage's logical range after every opcode
//! (0–65535 for OpcodeList1, 0–1 after).
//!
//! Interpretations where the specification leaves room (recorded in the crate README):
//! coordinates of OpcodeList1 are those of the stored raw image, of OpcodeList2 and 3 those of the
//! ActiveArea; GainMap positions are relative to the whole image of the stage (row / height,
//! column / width); Delta and MapPolynomial values are in the 0–1 range at every stage (scaled by
//! 65535 on OpcodeList1's integer samples), MapTable indexes 16-bit values (0–1 scaled by 65535
//! after OpcodeList1); warps resample with a Catmull–Rom cubic and are refused on CFA mosaics,
//! as is TrimBounds before OpcodeList3.
//!
//! Every count and size is checked against the bytes of the parameter area before anything is
//! allocated, and per-opcode work is bounded by the image size.

use crate::{Error, Result};

/// The newest DNG version whose opcodes this reader knows (1.7.1.0).
const KNOWN_VERSION: u32 = 0x0107_0100;

/// Most opcodes in one list (each may touch the whole picture).
pub const MAX_OPCODES: usize = 256;

const OPTIONAL: u32 = 1;
const PREVIEW_SKIPPABLE: u32 = 2;

/// One opcode of a list.
#[derive(Clone, Debug, PartialEq)]
pub struct Opcode {
    pub id: u32,
    /// The DNG version that defined it, as four bytes in a big-endian u32 (1.3.0.0 = 0x01030000).
    pub version: u32,
    pub flags: u32,
    pub params: Vec<u8>,
}

impl Opcode {
    pub fn optional(&self) -> bool {
        self.flags & OPTIONAL != 0
    }

    pub fn preview_skippable(&self) -> bool {
        self.flags & PREVIEW_SKIPPABLE != 0
    }
}

/// The opcode lists of a raw IFD.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lists {
    pub list1: Vec<Opcode>,
    pub list2: Vec<Opcode>,
    pub list3: Vec<Opcode>,
}

/// Parse an opcode list (big-endian).
pub fn parse(b: &[u8]) -> Result<Vec<Opcode>> {
    let bad = || Error::Malformed("opcode list");
    let mut r = Reader::new(b);
    let count = r.u32().map_err(|_| bad())? as usize;
    // every opcode takes at least 16 bytes
    if count > b.len() / 16 || count > MAX_OPCODES {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let id = r.u32().map_err(|_| bad())?;
        let version = r.u32().map_err(|_| bad())?;
        let flags = r.u32().map_err(|_| bad())?;
        let len = r.u32().map_err(|_| bad())? as usize;
        let params = r.bytes(len).map_err(|_| bad())?.to_vec();
        out.push(Opcode { id, version, flags, params });
    }
    Ok(out)
}

/// Big-endian parameter reader.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Malformed("opcode parameters"))?;
        let s = self.b.get(self.pos..end).ok_or(Error::Malformed("opcode parameters too short"))?;
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32> {
        let s = self.bytes(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn f32(&mut self) -> Result<f32> {
        let v = f32::from_bits(self.u32()?);
        if v.is_finite() { Ok(v) } else { Err(Error::Malformed("non-finite opcode parameter")) }
    }

    fn f64(&mut self) -> Result<f64> {
        let s = self.bytes(8)?;
        let v = f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
        if v.is_finite() { Ok(v) } else { Err(Error::Malformed("non-finite opcode parameter")) }
    }

    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }

    /// A count of items of `size` bytes each that must fit in what is left.
    fn count(&mut self, size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(size) > self.remaining() {
            return Err(Error::Malformed("opcode count larger than its parameters"));
        }
        Ok(n)
    }
}

/// The picture an opcode list works on: `planes` interleaved samples per pixel.
pub struct Image<'a> {
    pub data: &'a mut Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub planes: usize,
    /// The stage's logical maximum: 65535 for OpcodeList1 (integer samples, rounded after every
    /// opcode), 1 for OpcodeList2 and 3.
    pub max: f32,
    /// A CFA mosaic (one sample per pixel before the demosaic): warps are refused.
    pub mosaic: bool,
    /// TrimBounds is allowed (OpcodeList3 only: the picture may change size).
    pub allow_trim: bool,
    /// Origin of the picture after TrimBounds, relative to where it started.
    pub origin: (usize, usize),
}

impl Image<'_> {
    fn integer(&self) -> bool {
        self.max > 1.0
    }

    fn clip(&mut self) {
        let (max, int) = (self.max, self.integer());
        for v in self.data.iter_mut() {
            let c = v.clamp(0.0, max);
            *v = if int {
                c.round()
            } else if c.is_nan() {
                0.0
            } else {
                c
            };
        }
    }
}

/// Apply `list`. `preview` skips the opcodes marked as only needed for full quality.
pub fn apply(list: &[Opcode], img: &mut Image, preview: bool) -> Result<()> {
    let mut skip_warp = false;
    for op in list {
        let skip_this = std::mem::take(&mut skip_warp) && matches!(op.id, 1 | 2);
        if skip_this || (preview && op.preview_skippable()) {
            continue;
        }
        let known = op.version <= KNOWN_VERSION && (1..=14).contains(&op.id);
        if !known {
            if op.optional() {
                continue;
            }
            return Err(Error::Unsupported(format!("DNG opcode {} (version {:08x})", op.id, op.version)));
        }
        match run(op, img) {
            Ok(()) => {}
            // an optional opcode this reader cannot apply here is left out
            Err(Error::Unsupported(_)) if op.optional() => continue,
            Err(e) => return Err(e),
        }
        img.clip();
        if op.id == 14 && op.optional() {
            skip_warp = true;
        }
    }
    Ok(())
}

fn run(op: &Opcode, img: &mut Image) -> Result<()> {
    let mut r = Reader::new(&op.params);
    match op.id {
        1 | 2 | 14 => warp(op.id, &mut r, img),
        3 => vignette(&mut r, img),
        4 => {
            let constant = r.u32()?;
            let phase = r.u32()?;
            let mut bad = Vec::new();
            let c = if img.integer() { constant as f32 } else { constant as f32 / 65_535.0 };
            for (i, px) in img.data.iter().enumerate() {
                if *px == c {
                    bad.push(i);
                }
            }
            fix_bad(img, phase, &bad)
        }
        5 => {
            let phase = r.u32()?;
            let points = r.u32()? as usize;
            let rects = r.u32()? as usize;
            if points.saturating_mul(8).saturating_add(rects.saturating_mul(16)) > r.remaining() {
                return Err(Error::Malformed("FixBadPixelsList counts larger than its parameters"));
            }
            let (w, h) = (img.width, img.height);
            let mut mask = vec![false; w * h];
            for _ in 0..points {
                let (y, x) = (r.u32()? as usize, r.u32()? as usize);
                if x < w && y < h {
                    mask[y * w + x] = true;
                }
            }
            for _ in 0..rects {
                let (t, l, b, rt) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
                for y in t.min(h)..b.min(h) {
                    for x in l.min(w)..rt.min(w) {
                        mask[y * w + x] = true;
                    }
                }
            }
            let bad: Vec<usize> = mask.iter().enumerate().filter_map(|(i, &b)| b.then_some(i)).collect();
            fix_bad(img, phase, &bad)
        }
        6 => trim(&mut r, img),
        _ => area_op(op.id, &mut r, img),
    }
}

/// The area fields of opcodes 7–13, clamped to the picture.
struct Area {
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
    plane: usize,
    planes: usize,
    row_pitch: usize,
    col_pitch: usize,
}

impl Area {
    fn read(r: &mut Reader, img: &Image) -> Result<Area> {
        let mut v = [0usize; 8];
        for x in &mut v {
            *x = r.u32()? as usize;
        }
        let [top, left, bottom, right, plane, planes, row_pitch, col_pitch] = v;
        if row_pitch == 0 || col_pitch == 0 {
            return Err(Error::Malformed("opcode area with a zero pitch"));
        }
        let plane = plane.min(img.planes);
        Ok(Area {
            top: top.min(img.height),
            left: left.min(img.width),
            bottom: bottom.min(img.height),
            right: right.min(img.width),
            plane,
            planes: planes.min(img.planes - plane),
            row_pitch,
            col_pitch,
        })
    }

    /// Rows of the area, in order.
    fn rows(&self) -> impl Iterator<Item = usize> + use<> {
        (self.top..self.bottom.max(self.top)).step_by(self.row_pitch)
    }

    fn cols(&self) -> impl Iterator<Item = usize> + use<> {
        (self.left..self.right.max(self.left)).step_by(self.col_pitch)
    }

    /// Visit every affected sample: f(row index in area, col index in area, x, y, plane index in
    /// area, value).
    fn each(&self, img: &mut Image, mut f: impl FnMut(usize, usize, usize, usize, usize, &mut f32)) {
        let (w, np) = (img.width, img.planes);
        for (ri, y) in self.rows().enumerate() {
            for (ci, x) in self.cols().enumerate() {
                for p in 0..self.planes {
                    if let Some(v) = img.data.get_mut((y * w + x) * np + self.plane + p) {
                        f(ri, ci, x, y, p, v);
                    }
                }
            }
        }
    }
}

fn area_op(id: u32, r: &mut Reader, img: &mut Image) -> Result<()> {
    let a = Area::read(r, img)?;
    let scale = img.max;
    match id {
        7 => {
            let n = r.count(2)?;
            if n == 0 {
                return Err(Error::Malformed("MapTable without entries"));
            }
            let table: Vec<f32> = (0..n).map(|_| r.bytes(2).map(|b| f32::from(u16::from_be_bytes([b[0], b[1]])))).collect::<Result<_>>()?;
            let last = table.last().copied().unwrap_or(0.0);
            a.each(img, |_, _, _, _, _, v| {
                let code = (*v / scale * 65_535.0).round().clamp(0.0, 65_535.0) as usize;
                *v = table.get(code).copied().unwrap_or(last) / 65_535.0 * scale;
            });
        }
        8 => {
            let degree = r.u32()? as usize;
            if degree > 8 {
                return Err(Error::Malformed("MapPolynomial degree above 8"));
            }
            let c: Vec<f64> = (0..=degree).map(|_| r.f64()).collect::<Result<_>>()?;
            a.each(img, |_, _, _, _, _, v| {
                let x = f64::from(*v / scale);
                let y = c.iter().rev().fold(0.0, |acc, k| acc * x + k);
                *v = (y * f64::from(scale)) as f32;
            });
        }
        9 => gain_map(&a, r, img)?,
        10..=13 => {
            let n = r.count(4)?;
            let vals: Vec<f32> = (0..n).map(|_| r.f32()).collect::<Result<_>>()?;
            let per_row = matches!(id, 10 | 12);
            let need = if per_row { a.rows().count() } else { a.cols().count() };
            if vals.len() < need {
                return Err(Error::Malformed("opcode with fewer values than rows or columns"));
            }
            let delta = id <= 11;
            a.each(img, |ri, ci, _, _, _, v| {
                let k = vals.get(if per_row { ri } else { ci }).copied().unwrap_or(if delta { 0.0 } else { 1.0 });
                *v = if delta { *v + k * scale } else { *v * k };
            });
        }
        _ => return Err(Error::Unsupported(format!("DNG opcode {id}"))),
    }
    Ok(())
}

fn gain_map(a: &Area, r: &mut Reader, img: &mut Image) -> Result<()> {
    let points_v = r.u32()? as usize;
    let points_h = r.u32()? as usize;
    let spacing_v = r.f64()?;
    let spacing_h = r.f64()?;
    let origin_v = r.f64()?;
    let origin_h = r.f64()?;
    let map_planes = r.u32()? as usize;
    let n = points_v.saturating_mul(points_h).saturating_mul(map_planes);
    if points_v == 0 || points_h == 0 || map_planes == 0 || n.saturating_mul(4) > r.remaining() {
        return Err(Error::Malformed("GainMap size"));
    }
    if (points_v > 1 && spacing_v <= 0.0) || (points_h > 1 && spacing_h <= 0.0) {
        return Err(Error::Malformed("GainMap spacing"));
    }
    let gains: Vec<f32> = (0..n).map(|_| r.f32()).collect::<Result<_>>()?;
    let (w, h) = (img.width.max(1) as f64, img.height.max(1) as f64);
    let pos = |rel: f64, origin: f64, spacing: f64, points: usize| {
        if points <= 1 {
            return 0.0;
        }
        ((rel - origin) / spacing).clamp(0.0, (points - 1) as f64)
    };
    let gain = |p: usize, mv: f64, mh: f64| -> f32 {
        let mp = p.min(map_planes - 1);
        let (v0, h0) = (mv.floor() as usize, mh.floor() as usize);
        let (v1, h1) = ((v0 + 1).min(points_v - 1), (h0 + 1).min(points_h - 1));
        let (fv, fh) = (mv - v0 as f64, mh - h0 as f64);
        let g = |v: usize, hh: usize| f64::from(gains.get((v * points_h + hh) * map_planes + mp).copied().unwrap_or(1.0));
        let top = g(v0, h0) * (1.0 - fh) + g(v0, h1) * fh;
        let bottom = g(v1, h0) * (1.0 - fh) + g(v1, h1) * fh;
        (top * (1.0 - fv) + bottom * fv) as f32
    };
    a.each(img, |_, _, x, y, p, v| {
        let mv = pos(y as f64 / h, origin_v, spacing_v, points_v);
        let mh = pos(x as f64 / w, origin_h, spacing_h, points_h);
        *v *= gain(p, mv, mh);
    });
    Ok(())
}

/// The optical centre in pixels and the distance to the farthest corner (chapter 7).
fn centre(img: &Image, cx: f64, cy: f64) -> (f64, f64, f64) {
    let (x1, y1) = (img.width.saturating_sub(1) as f64, img.height.saturating_sub(1) as f64);
    let (cx, cy) = (cx * x1, cy * y1);
    let mx = cx.abs().max((x1 - cx).abs());
    let my = cy.abs().max((y1 - cy).abs());
    (cx, cy, (mx * mx + my * my).sqrt())
}

fn vignette(r: &mut Reader, img: &mut Image) -> Result<()> {
    let k: Vec<f64> = (0..5).map(|_| r.f64()).collect::<Result<_>>()?;
    let (cx, cy) = (r.f64()?, r.f64()?);
    let (cx, cy, m) = centre(img, cx, cy);
    if m <= 0.0 {
        return Ok(());
    }
    let (w, np) = (img.width, img.planes);
    for (i, px) in img.data.chunks_exact_mut(np.max(1)).enumerate() {
        let (x, y) = ((i % w) as f64, (i / w) as f64);
        let r2 = ((x - cx).powi(2) + (y - cy).powi(2)) / (m * m);
        let g = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
        for v in px {
            *v = (f64::from(*v) * g) as f32;
        }
    }
    Ok(())
}

/// Interpolate over `bad` pixels of a Bayer mosaic: the mean of the nearest good pixels of the
/// same colour (within 2, else 4, … up to 8 pixels).
fn fix_bad(img: &mut Image, phase: u32, bad: &[usize]) -> Result<()> {
    if bad.is_empty() {
        return Ok(());
    }
    // a real sensor has few bad pixels; a list covering much of the picture is not one
    if bad.len() > img.data.len() / 16 + 64 {
        return Err(Error::Unsupported("bad pixel opcode covering much of the picture".into()));
    }
    if img.planes != 1 {
        return Err(Error::Unsupported("bad pixel opcodes on a picture that is not a mosaic".into()));
    }
    // BayerPhase: the colour of the top-left pixel (0 red, 1 green in a red row, 2 green in a
    // blue row, 3 blue)
    let quad: [u8; 4] = match phase {
        0 => [0, 1, 1, 2],
        1 => [1, 0, 2, 1],
        2 => [1, 2, 0, 1],
        3 => [2, 1, 1, 0],
        _ => return Err(Error::Malformed("BayerPhase")),
    };
    let (w, h) = (img.width, img.height);
    let mut is_bad = vec![false; w * h];
    for &i in bad {
        if let Some(b) = is_bad.get_mut(i) {
            *b = true;
        }
    }
    let color = |x: usize, y: usize| quad[(y & 1) * 2 + (x & 1)];
    let src = img.data.clone();
    for &i in bad {
        let (x, y) = (i % w, i / w);
        let me = color(x, y);
        for radius in [2isize, 4, 6, 8] {
            let (mut sum, mut n) = (0f64, 0u32);
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (sx, sy) = (x as isize + dx, y as isize + dy);
                    if sx < 0 || sy < 0 || sx as usize >= w || sy as usize >= h || (dx == 0 && dy == 0) {
                        continue;
                    }
                    let (sx, sy) = (sx as usize, sy as usize);
                    if color(sx, sy) == me && !is_bad[sy * w + sx] {
                        sum += f64::from(src[sy * w + sx]);
                        n += 1;
                    }
                }
            }
            if n > 0 {
                if let Some(v) = img.data.get_mut(i) {
                    *v = (sum / f64::from(n)) as f32;
                }
                break;
            }
        }
    }
    Ok(())
}

fn trim(r: &mut Reader, img: &mut Image) -> Result<()> {
    let (t, l, b, rt) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
    if !img.allow_trim {
        return Err(Error::Unsupported("TrimBounds before the demosaic".into()));
    }
    let (b, rt) = (b.min(img.height), rt.min(img.width));
    if t >= b || l >= rt {
        return Err(Error::Malformed("TrimBounds outside the picture"));
    }
    let (np, w) = (img.planes, img.width);
    let nw = rt - l;
    let mut out = Vec::with_capacity(nw * (b - t) * np);
    for y in t..b {
        out.extend_from_slice(img.data.get((y * w + l) * np..(y * w + rt) * np).ok_or(Error::Malformed("TrimBounds"))?);
    }
    *img.data = out;
    img.width = nw;
    img.height = b - t;
    img.origin = (img.origin.0 + l, img.origin.1 + t);
    Ok(())
}

/// Catmull–Rom weights for a fractional position `t` in 0–1.
fn cubic(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [(-t3 + 2.0 * t2 - t) / 2.0, (3.0 * t3 - 5.0 * t2 + 2.0) / 2.0, (-3.0 * t3 + 4.0 * t2 + t) / 2.0, (t3 - t2) / 2.0]
}

/// The radial model of a warp opcode for one plane.
enum Radial {
    /// WarpRectilinear: f = kr0 + kr1 r² + kr2 r⁴ + kr3 r⁶, tangential kt0, kt1.
    Rectilinear([f64; 4], [f64; 2]),
    /// WarpFisheye: f = (kr0 θ + kr1 θ³ + kr2 θ⁵ + kr3 θ⁷) / r, θ = atan r.
    Fisheye([f64; 4]),
    /// WarpRectilinear2: f(r) = Σ kr_p r^p (p ≤ 14) at r clamped to the valid range, multiplied
    /// (or divided, reciprocal) into r; tangential kt0, kt1.
    Rectilinear2 { kr: [f64; 15], kt: [f64; 2], min: f64, max: f64, reciprocal: bool },
}

impl Radial {
    /// Source offset (normalised) of destination offset (dx, dy).
    fn map(&self, dx: f64, dy: f64) -> (f64, f64) {
        let r2 = dx * dx + dy * dy;
        let tangential = |kt: &[f64; 2]| (kt[0] * (2.0 * dx * dy) + kt[1] * (r2 + 2.0 * dx * dx), kt[1] * (2.0 * dx * dy) + kt[0] * (r2 + 2.0 * dy * dy));
        match self {
            Radial::Rectilinear(kr, kt) => {
                let f = kr[0] + r2 * (kr[1] + r2 * (kr[2] + r2 * kr[3]));
                let (tx, ty) = tangential(kt);
                (f * dx + tx, f * dy + ty)
            }
            Radial::Fisheye(kr) => {
                let r = r2.sqrt();
                let f = if r < 1e-12 {
                    kr[0]
                } else {
                    let t = r.atan();
                    let t2 = t * t;
                    t * (kr[0] + t2 * (kr[1] + t2 * (kr[2] + t2 * kr[3]))) / r
                };
                (f * dx, f * dy)
            }
            Radial::Rectilinear2 { kr, kt, min, max, reciprocal } => {
                let r = r2.sqrt().clamp(*min, *max);
                let f = kr.iter().rev().fold(0.0, |acc, k| acc * r + k);
                let f = if *reciprocal {
                    if f.abs() < 1e-12 {
                        return (dx, dy);
                    }
                    1.0 / f
                } else {
                    f
                };
                let (tx, ty) = tangential(kt);
                (f * dx + tx, f * dy + ty)
            }
        }
    }
}

fn warp(id: u32, r: &mut Reader, img: &mut Image) -> Result<()> {
    if img.mosaic {
        return Err(Error::Unsupported("a warp opcode on a CFA mosaic".into()));
    }
    let per_set = match id {
        1 => 6,
        2 => 4,
        _ => 19,
    };
    let n = r.count(per_set * 8)?;
    if n != 1 && n != img.planes {
        return Err(Error::Malformed("warp opcode coefficient sets"));
    }
    let mut sets = Vec::with_capacity(n);
    for _ in 0..n {
        let v: Vec<f64> = (0..per_set).map(|_| r.f64()).collect::<Result<_>>()?;
        sets.push(match id {
            1 => Radial::Rectilinear([v[0], v[1], v[2], v[3]], [v[4], v[5]]),
            2 => Radial::Fisheye([v[0], v[1], v[2], v[3]]),
            _ => {
                let mut kr = [0f64; 15];
                kr.copy_from_slice(&v[..15]);
                let (min, max) = (v[17].clamp(0.0, 1.0), v[18].clamp(0.0, 1.0));
                if min >= max {
                    return Err(Error::Malformed("WarpRectilinear2 valid radius range"));
                }
                Radial::Rectilinear2 { kr, kt: [v[15], v[16]], min, max, reciprocal: false }
            }
        });
    }
    let (cx, cy) = (r.f64()?, r.f64()?);
    if id == 14 {
        let reciprocal = r.u32()? != 0;
        for s in &mut sets {
            if let Radial::Rectilinear2 { reciprocal: rr, .. } = s {
                *rr = reciprocal;
            }
        }
    }
    let (cx, cy, m) = centre(img, cx, cy);
    if m <= 0.0 {
        return Ok(());
    }
    let (w, h, np) = (img.width, img.height, img.planes);
    let src = img.data.clone();
    let sample = |p: usize, x: isize, y: isize| {
        let (x, y) = (x.clamp(0, w as isize - 1) as usize, y.clamp(0, h as isize - 1) as usize);
        f64::from(src.get((y * w + x) * np + p).copied().unwrap_or(0.0))
    };
    for (i, px) in img.data.chunks_exact_mut(np.max(1)).enumerate() {
        let (x, y) = ((i % w) as f64, (i / w) as f64);
        let (dx, dy) = ((x - cx) / m, (y - cy) / m);
        for (p, v) in px.iter_mut().enumerate() {
            let model = sets.get(p).or(sets.first());
            let Some(model) = model else { continue };
            let (ox, oy) = model.map(dx, dy);
            let (sx, sy) = (cx + m * ox, cy + m * oy);
            if !sx.is_finite() || !sy.is_finite() {
                continue;
            }
            let (fx, fy) = (sx.floor(), sy.floor());
            let (wx, wy) = (cubic(sx - fx), cubic(sy - fy));
            let (ix, iy) = (fx.clamp(-4.0, w as f64 + 4.0) as isize, fy.clamp(-4.0, h as f64 + 4.0) as isize);
            let mut acc = 0.0;
            for (j, wyj) in wy.iter().enumerate() {
                let mut row = 0.0;
                for (k, wxk) in wx.iter().enumerate() {
                    row += wxk * sample(p, ix + k as isize - 1, iy + j as isize - 1);
                }
                acc += wyj * row;
            }
            *v = acc as f32;
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "writer"))]
/// Encode an opcode list (big-endian), for tests: (id, flags, parameters).
pub fn encode(list: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = (list.len() as u32).to_be_bytes().to_vec();
    for (id, flags, params) in list {
        let version: u32 = if *id == 14 { 0x0106_0000 } else { 0x0103_0000 };
        for v in [*id, version, *flags, params.len() as u32] {
            out.extend(v.to_be_bytes());
        }
        out.extend(params);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    fn d(v: &[f64]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    fn f(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    fn area(t: u32, l: u32, b: u32, r: u32, plane: u32, planes: u32, rp: u32, cp: u32) -> Vec<u8> {
        u(&[t, l, b, r, plane, planes, rp, cp])
    }

    /// Run one opcode on a float picture (range 0–1).
    fn run1(id: u32, params: Vec<u8>, data: &mut Vec<f32>, w: usize, h: usize, planes: usize) -> Result<(usize, usize)> {
        let list = parse(&encode(&[(id, 0, params)]))?;
        let mut img = Image { data, width: w, height: h, planes, max: 1.0, mosaic: false, allow_trim: true, origin: (0, 0) };
        apply(&list, &mut img, false)?;
        Ok((img.width, img.height))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn list_parsing_and_hostile_lists() {
        let l = encode(&[(9, 1, vec![1, 2, 3]), (3, 2, vec![])]);
        let p = parse(&l).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].id, p[0].version, p[0].params.as_slice()), (9, 0x0103_0000, &[1u8, 2, 3][..]));
        assert!(p[0].optional() && !p[0].preview_skippable());
        assert!(!p[1].optional() && p[1].preview_skippable());
        // every truncation is an error, never a panic
        for n in 0..l.len() {
            assert!(parse(&l[..n]).is_err(), "{n}");
        }
        // counts and sizes beyond the data
        assert!(parse(&u(&[u32::MAX])).is_err());
        assert!(parse(&u(&[1, 9, 0, 0, u32::MAX])).is_err());
        assert!(parse(&u(&[(MAX_OPCODES + 1) as u32]).into_iter().chain(vec![0; 16 * (MAX_OPCODES + 1)]).collect::<Vec<_>>()).is_err());
        assert_eq!(parse(&u(&[0])).unwrap(), vec![]);
    }

    #[test]
    fn unknown_and_newer_opcodes() {
        let mut data = vec![0.5f32; 4];
        let mut img = Image { data: &mut data, width: 2, height: 2, planes: 1, max: 1.0, mosaic: false, allow_trim: false, origin: (0, 0) };
        // unknown id: skipped when optional, an error when not
        assert!(apply(&parse(&encode(&[(99, 1, vec![])])).unwrap(), &mut img, false).is_ok());
        assert!(matches!(apply(&parse(&encode(&[(99, 0, vec![])])).unwrap(), &mut img, false), Err(Error::Unsupported(_))));
        // a known id from a newer DNG version is unknown
        let newer = [Opcode { id: 3, version: 0x0108_0000, flags: 0, params: vec![] }];
        assert!(matches!(apply(&newer, &mut img, false), Err(Error::Unsupported(_))));
        // an opcode this stage cannot run: skipped when optional
        let trim = Opcode { id: 6, version: 0x0103_0000, flags: 1, params: u(&[0, 0, 1, 1]) };
        assert!(apply(std::slice::from_ref(&trim), &mut img, false).is_ok());
        assert!(apply(&[Opcode { flags: 0, ..trim }], &mut img, false).is_err());
        // preview quality skips flagged opcodes
        let scale = Opcode { id: 12, version: 0x0103_0000, flags: 2, params: [area(0, 0, 2, 2, 0, 1, 1, 1), u(&[2]), f(&[0.5, 0.5])].concat() };
        apply(std::slice::from_ref(&scale), &mut img, true).unwrap();
        assert_eq!(img.data[0], 0.5);
        apply(&[scale], &mut img, false).unwrap();
        assert_eq!(img.data[0], 0.25);
        // malformed parameters are errors
        assert!(apply(&[Opcode { id: 10, version: 0x0103_0000, flags: 0, params: vec![0; 5] }], &mut img, false).is_err());
    }

    #[test]
    fn map_table_polynomial_delta_and_scale() {
        // 4 × 2, two planes; affect plane 1 only, every second column
        let mut data: Vec<f32> = (0..16).map(|i| i as f32 / 20.0).collect();
        let orig = data.clone();
        // MapTable: 16-bit identity reversed (v → 1 − v)
        let table: Vec<u8> = (0..=65_535u32).rev().flat_map(|v| (v as u16).to_be_bytes()).collect();
        run1(7, [area(0, 0, 2, 4, 1, 1, 1, 2), u(&[65_536]), table].concat(), &mut data, 4, 2, 2).unwrap();
        for (i, (a, b)) in data.iter().zip(&orig).enumerate() {
            let (x, p) = ((i / 2) % 4, i % 2);
            let want = if p == 1 && x % 2 == 0 { 1.0 - (b * 65_535.0).round() / 65_535.0 } else { *b };
            assert!(close(*a, want), "{i}: {a} {want}");
        }
        // MapPolynomial 0.1 + 0.5 x on everything
        let mut data = orig.clone();
        run1(8, [area(0, 0, 2, 4, 0, 2, 1, 1), u(&[1]), d(&[0.1, 0.5])].concat(), &mut data, 4, 2, 2).unwrap();
        for (a, b) in data.iter().zip(&orig) {
            assert!(close(*a, 0.1 + 0.5 * b));
        }
        // DeltaPerRow (rows 0, 1), ScalePerColumn (columns 1..3)
        let mut data = orig.clone();
        run1(10, [area(0, 0, 2, 4, 0, 2, 1, 1), u(&[2]), f(&[0.01, -0.02])].concat(), &mut data, 4, 2, 2).unwrap();
        for (i, (a, b)) in data.iter().zip(&orig).enumerate() {
            let want = (b + if i < 8 { 0.01 } else { -0.02 }).clamp(0.0, 1.0);
            assert!(close(*a, want), "{i}");
        }
        let mut data = orig.clone();
        run1(13, [area(0, 1, 2, 3, 0, 2, 1, 1), u(&[2]), f(&[2.0, 0.5])].concat(), &mut data, 4, 2, 2).unwrap();
        for (i, (a, b)) in data.iter().zip(&orig).enumerate() {
            let x = (i / 2) % 4;
            let want = match x {
                1 => (b * 2.0).min(1.0),
                2 => b * 0.5,
                _ => *b,
            };
            assert!(close(*a, want), "{i}");
        }
        // too few deltas for the rows is an error
        let mut data = orig.clone();
        assert!(run1(10, [area(0, 0, 2, 4, 0, 2, 1, 1), u(&[1]), f(&[0.1])].concat(), &mut data, 4, 2, 2).is_err());
        // zero pitch is an error, areas outside the picture are clamped
        assert!(run1(12, [area(0, 0, 2, 4, 0, 1, 0, 1), u(&[0])].concat(), &mut data, 4, 2, 2).is_err());
        let mut data = orig.clone();
        run1(12, [area(1, 3, 99, 99, 5, 9, 1, 1), u(&[1]), f(&[0.0])].concat(), &mut data, 4, 2, 2).unwrap();
        assert_eq!(data, orig, "plane 5 does not exist");
    }

    #[test]
    fn gain_map_interpolates_and_replicates() {
        // 2 × 2 map at the corners of the picture's relative coordinates 0 and 0.5
        let (w, h) = (8, 8);
        let mut data = vec![0.25f32; w * h];
        let gains = f(&[1.0, 2.0, 3.0, 4.0]);
        run1(9, [area(0, 0, 8, 8, 0, 1, 1, 1), u(&[2, 2]), d(&[0.5, 0.5, 0.0, 0.0]), u(&[1]), gains].concat(), &mut data, w, h, 1).unwrap();
        let g = |x: usize, y: usize| data[y * w + x] / 0.25;
        assert!(close(g(0, 0), 1.0));
        assert!(close(g(4, 0), 2.0), "{}", g(4, 0));
        assert!(close(g(0, 4), 3.0));
        assert!(close(g(2, 2), 2.5), "bilinear in the middle: {}", g(2, 2));
        assert!(close(g(7, 7), 4.0), "replicated past the map");
        // a map larger than its parameters is an error
        let mut data = vec![0.25f32; 4];
        assert!(run1(9, [area(0, 0, 2, 2, 0, 1, 1, 1), u(&[1000, 1000]), d(&[0.1, 0.1, 0.0, 0.0]), u(&[1]), f(&[1.0])].concat(), &mut data, 2, 2, 1).is_err());
    }

    #[test]
    fn vignette_and_warps() {
        // FixVignetteRadial k0 = 1: g = 1 + r², the centre unchanged, the corners doubled
        let (w, h) = (5, 5);
        let mut data = vec![0.25f32; w * h];
        run1(3, d(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]), &mut data, w, h, 1).unwrap();
        assert!(close(data[2 * w + 2], 0.25));
        assert!(close(data[0], 0.5) && close(data[w * h - 1], 0.5));
        assert!(close(data[2 * w], 0.25 * 1.5), "r² = 0.5 at the edge middle: {}", data[2 * w]);
        // identity warps leave the picture alone; kr0 = 0.5 magnifies a ramp about the centre
        let (w, h) = (9, 7);
        let ramp: Vec<f32> = (0..w * h).flat_map(|i| [(i % w) as f32 / 10.0, (i / w) as f32 / 10.0]).collect();
        let mut data = ramp.clone();
        run1(1, [u(&[1]), d(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat(), &mut data, w, h, 2).unwrap();
        assert!(data.iter().zip(&ramp).all(|(a, b)| close(*a, *b)));
        let mut data = ramp.clone();
        run1(1, [u(&[1]), d(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat(), &mut data, w, h, 2).unwrap();
        // x' = 4 + 0.5 (x − 4): the cubic reproduces a linear ramp exactly
        for x in 1..w - 1 {
            let want = (4.0 + 0.5 * (x as f32 - 4.0)) / 10.0;
            assert!(close(data[(3 * w + x) * 2], want), "x {x}: {} vs {want}", data[(3 * w + x) * 2]);
        }
        // one coefficient set per plane: plane 1 untouched
        let mut data = ramp.clone();
        run1(1, [u(&[2]), d(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat(), &mut data, w, h, 2).unwrap();
        assert!(data.iter().skip(1).step_by(2).zip(ramp.iter().skip(1).step_by(2)).all(|(a, b)| close(*a, *b)));
        // three sets for two planes is malformed
        let mut data = ramp.clone();
        assert!(run1(1, [u(&[3]), vec![0; 3 * 48 + 16]].concat(), &mut data, w, h, 2).is_err());
        // WarpFisheye kr0 = 1: f = atan(r) / r pulls sources towards the centre
        let mut data = ramp.clone();
        run1(2, [u(&[1]), d(&[1.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat(), &mut data, w, h, 2).unwrap();
        let m = (16.0f64 + 9.0).sqrt();
        let dx = 3.0 / m;
        let want = (4.0 + m * dx.atan()) / 10.0;
        assert!((f64::from(data[(3 * w + 7) * 2]) - want).abs() < 1e-3, "{} vs {want}", data[(3 * w + 7) * 2]);
        // WarpRectilinear2 identity (kr0 = 1), and the skip rule for a following warp
        let wr2 = |flags| (14, flags, [u(&[1]), d(&[1.0]), d(&[0.0; 14]), d(&[0.0, 0.0, 0.0, 1.0, 0.5, 0.5]), u(&[0])].concat());
        let halve = (1, 0, [u(&[1]), d(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5])].concat());
        for (flags, skipped) in [(1, true), (0, false)] {
            let list = parse(&encode(&[wr2(flags), halve.clone()])).unwrap();
            let mut data = ramp.clone();
            let mut img = Image { data: &mut data, width: w, height: h, planes: 2, max: 1.0, mosaic: false, allow_trim: false, origin: (0, 0) };
            apply(&list, &mut img, false).unwrap();
            let unchanged = data.iter().zip(&ramp).all(|(a, b)| close(*a, *b));
            assert_eq!(unchanged, skipped, "optional WarpRectilinear2 {flags}");
        }
        // warps are refused on a mosaic
        let mut data = vec![0.5f32; 16];
        let mut img = Image { data: &mut data, width: 4, height: 4, planes: 1, max: 1.0, mosaic: true, allow_trim: false, origin: (0, 0) };
        assert!(apply(&parse(&encode(&[halve])).unwrap(), &mut img, false).is_err());
    }

    #[test]
    fn bad_pixels_and_trim() {
        // RGGB 6 × 6 with each colour flat; two bad pixels and a bad 2 × 2 rectangle
        let (w, h) = (6, 6);
        let level = [0.2f32, 0.5, 0.5, 0.8];
        let flat: Vec<f32> = (0..w * h).map(|i| level[(i / w % 2) * 2 + i % 2]).collect();
        let mut data = flat.clone();
        data[2 * w + 2] = 0.0;
        data[3 * w + 4] = 0.0;
        // FixBadPixelsConstant 0, phase 0 (red top left), integer stage
        let mut ints: Vec<f32> = data.iter().map(|v| (v * 65_535.0).round()).collect();
        let list = parse(&encode(&[(4, 0, u(&[0, 0]))])).unwrap();
        let mut img = Image { data: &mut ints, width: w, height: h, planes: 1, max: 65_535.0, mosaic: true, allow_trim: false, origin: (0, 0) };
        apply(&list, &mut img, false).unwrap();
        for (a, b) in ints.iter().zip(&flat) {
            assert_eq!(*a, (b * 65_535.0).round());
        }
        // FixBadPixelsList: a point and a rectangle
        let mut data = flat.clone();
        for i in [w + 1, 4 * w + 3, 4 * w + 4, 5 * w + 3, 5 * w + 4] {
            data[i] = 0.9;
        }
        run1(5, u(&[0, 1, 1, 1, 1, 4, 3, 6, 5]), &mut data, w, h, 1).unwrap();
        assert!(data.iter().zip(&flat).all(|(a, b)| close(*a, *b)), "{data:?}");
        // a bad phase is malformed; huge lists are refused before allocating
        let mut data = flat.clone();
        assert!(run1(5, u(&[7, 1, 0, 1, 1]), &mut data, w, h, 1).is_err());
        assert!(run1(5, u(&[0, u32::MAX, u32::MAX]), &mut data, w, h, 1).is_err());
        // TrimBounds
        let mut data: Vec<f32> = (0..w * h * 3).map(|i| i as f32 / 200.0).collect();
        let (tw, th) = run1(6, u(&[1, 2, 4, 5]), &mut data, w, h, 3).unwrap();
        assert_eq!((tw, th, data.len()), (3, 3, 27));
        assert!(close(data[0], ((w + 2) * 3) as f32 / 200.0));
        let mut data = vec![0f32; 4];
        assert!(run1(6, u(&[1, 1, 1, 2]), &mut data, 2, 2, 1).is_err());
    }
}
