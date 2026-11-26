//! Clean-room reader for the Insta360 INSV trailer.
//!
//! An `.insv` file (Insta360 X-series and similar cameras) is an ordinary MP4 —
//! `ftyp`/`mdat`/`moov`, usually two fisheye video tracks and an audio track —
//! followed by a proprietary *trailer*: a 72-byte header at end-of-file, then a
//! chain of metadata frames packed back-to-back in front of it. The trailer's
//! INFO frame holds a protobuf with the camera model, firmware and, in the
//! `OffsetV3` field, the factory lens calibration (an MEI projection per
//! fisheye: mirror parameter, focal lengths, principal point, distortion and
//! the lens pose) that makes a seam-free stitch possible.
//!
//! This crate reads only the trailer; the MP4 part is `filmcraft-isobmff`'s
//! job. The layout is observed from camera files and the published
//! `insv-conversion` / `insvtools` descriptions of the header and frame chain;
//! no Insta360 binary was disassembled (clean-room, AGENTS.md §2).
//!
//! Every number comes from the file and is treated as hostile: sizes are
//! checked against the file length, the frame chain is walked with a step
//! limit, and the protobuf skips unknown fields within bounds. A file whose
//! trailer does not parse is `Ok(None)` — the caller opens the MP4 part alone.
//!
//! Layer L0: no dependencies beyond `std`, no `unsafe`, builds for
//! `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::io;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A read failed.
    Io(String),
    /// Structurally invalid trailer data.
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Invalid(e) => write!(f, "invalid INSV trailer: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Random-access, read-only bytes (the same shape the other container crates
/// use).
pub trait ByteSource {
    /// Total length in bytes.
    fn len(&self) -> u64;
    /// Fill `buf` entirely with bytes starting at `offset`; fail with
    /// `UnexpectedEof` if past the end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    /// True if the source holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of byte source")
}

impl ByteSource for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        let src = self.get(start..end).ok_or_else(eof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

impl ByteSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for &T {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ByteSource + ?Sized> ByteSource for Box<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

/// The 32-byte ASCII hex signature at the end of the 72-byte trailer header.
pub const SIGNATURE_BYTES: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";
/// Size of the trailer header at end-of-file.
const HEADER_SIZE: u64 = 72;
/// Trailer header version this parser reads.
const HEADER_VERSION: i32 = 3;
/// Most frames to walk backwards through the chain (a damaged file must not
/// loop).
const MAX_CHAIN_STEPS: usize = 64;
/// Most bytes to accept for one metadata frame.
const MAX_FRAME: u64 = 1 << 30;

/// Whether `src` ends with an INSV trailer header.
pub fn sniff(src: &dyn ByteSource) -> bool {
    trailer_head(src).is_some()
}

/// The 72 trailer header bytes, when the file ends with a valid one.
fn trailer_head(src: &dyn ByteSource) -> Option<[u8; HEADER_SIZE as usize]> {
    let len = src.len();
    if len < HEADER_SIZE {
        return None;
    }
    let mut h = [0u8; HEADER_SIZE as usize];
    src.read_at(len - HEADER_SIZE, &mut h).ok()?;
    (h.get(40..72)? == SIGNATURE_BYTES).then_some(h)
}

/// One fisheye lens of the factory calibration, in full-sensor pixels and
/// degrees.
///
/// The projection is the MEI (unified) model: a ray is rotated into the lens
/// frame, intersects the unit sphere shifted by the mirror parameter `xi`, and
/// the resulting normalised point is distorted with the Brown coefficients
/// (`k1..k3`, `p1`, `p2`) before the intrinsics map it to the sensor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    /// Mirror parameter of the unified projection (> 1 for these lenses).
    pub xi: f64,
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    /// Lens pose in camera space, degrees. The firmware de-rolls the image, so
    /// a stitch applies `yaw` and `pitch` only; `roll` describes the sensor
    /// mount.
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub k1: f64,
    pub k2: f64,
    pub k3: f64,
    pub p1: f64,
    pub p2: f64,
    /// The lens's position in camera space, metres (the `OffsetV3` translation
    /// tokens). The two lenses are physically separated by the rig's baseline
    /// (~3 cm on an X4); a stitch that ignores it treats them as co-located and
    /// so cannot reconcile a near object seen from both. Default zero (a lens
    /// with no known offset, e.g. a synthetic test).
    pub tx: f64,
    pub ty: f64,
    pub tz: f64,
    /// Full sensor size: the two lenses' circles sit side by side across it.
    pub sensor_w: u32,
    pub sensor_h: u32,
}

impl Lens {
    /// Re-express this lens for a `circle_px` square video circle (the sensor
    /// is scaled to the decoded picture, and each lens's principal point moves
    /// relative to its own circle's centre). `index` is the lens's position in
    /// the sensor strip: 0 left, 1 right.
    pub fn scale_to_circle(&self, index: usize, circle_px: u32) -> Option<Lens> {
        let (circle, sh) = (f64::from(circle_px), f64::from(self.sensor_h));
        if !sh.is_finite() || sh <= 0.0 || !circle.is_finite() {
            return None;
        }
        let s = circle / sh;
        let ox = f64::from(self.sensor_w) * (2.0 * index as f64 + 1.0) / 4.0;
        Some(Lens {
            xi: self.xi,
            fx: self.fx * s,
            fy: self.fy * s,
            cx: (self.cx - ox) * s + circle / 2.0,
            cy: (self.cy - sh / 2.0) * s + circle / 2.0,
            yaw: self.yaw,
            pitch: self.pitch,
            roll: self.roll,
            k1: self.k1,
            k2: self.k2,
            k3: self.k3,
            p1: self.p1,
            p2: self.p2,
            tx: self.tx,
            ty: self.ty,
            tz: self.tz,
            sensor_w: circle_px,
            sensor_h: circle_px,
        })
    }
}

/// What the INSV trailer says about a file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// Camera model, e.g. `Insta360 X4` (protobuf field 2).
    pub model: Option<String>,
    /// Firmware build (field 3).
    pub firmware: Option<String>,
    /// Device serial (field 1).
    pub serial: Option<String>,
    /// Colour profile name, e.g. `standard` (field 22).
    pub color_profile: Option<String>,
    /// For a spanned recording, the path of the first part as the camera
    /// stored it (field 26's submessage); the file itself plays standalone.
    pub first_part: Option<String>,
    /// The two lenses' factory calibration, in stream order (field 54,
    /// `OffsetV3`): lens 0 is the left circle (the front, facing +Z), lens 1 the
    /// right (the rear, facing −Z). `None` when the trailer carries no usable calibration.
    pub lenses: Option<[Lens; 2]>,
}

/// Read the INSV trailer of `src`. `Ok(None)` when the file has no trailer
/// this parser understands (it is then just an MP4).
pub fn parse(src: &dyn ByteSource) -> Result<Option<Meta>> {
    let Some(h) = trailer_head(src) else {
        return Ok(None);
    };
    let len = src.len();
    let (meta_size, version) = (i32_at(&h, 32), i32_at(&h, 36));
    if version != HEADER_VERSION {
        return Ok(None);
    }
    let meta_size = u64::try_from(meta_size).unwrap_or(0);
    if meta_size < HEADER_SIZE || meta_size > len {
        return Ok(None);
    }
    let meta_pos = len - meta_size;
    let Some(info) = info_payload(src, meta_pos, len)? else {
        return Ok(None);
    };
    Ok(Some(meta_from_protobuf(&info)))
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    let w: [u8; 4] = match b.get(at..at + 4) {
        Some(s) => s.try_into().unwrap_or([0; 4]),
        None => [0; 4],
    };
    i32::from_le_bytes(w)
}

/// The INFO frame's protobuf, found by walking the frame chain backwards from
/// the header, falling back to the INDEX frame's entry table.
fn info_payload(src: &dyn ByteSource, meta_pos: u64, len: u64) -> Result<Option<Vec<u8>>> {
    let mut cur = len.saturating_sub(HEADER_SIZE);
    for _ in 0..MAX_CHAIN_STEPS {
        if cur <= meta_pos + 6 {
            break;
        }
        let mut hdr = [0u8; 6];
        src.read_at(cur - 6, &mut hdr)?;
        let (ver, typ) = (hdr[0], hdr[1]);
        let size = i32_at(&hdr, 2);
        let size = u64::try_from(size).unwrap_or(0);
        if size == 0 || size > MAX_FRAME || cur < 6 + size + meta_pos {
            return index_payload(src, meta_pos);
        }
        let payload = cur - 6 - size;
        if typ == 1 && ver == 1 {
            return Ok(Some(read(src, payload, size)?));
        }
        if typ == 0 {
            return index_payload(src, meta_pos);
        }
        cur = payload;
    }
    index_payload(src, meta_pos)
}

/// The INFO frame through the INDEX frame (chain walking hit damage first).
/// INDEX entries are `[u8 type][u8 ver][i32 size][i32 offset]`, offsets
/// relative to the metadata region's start.
fn index_payload(src: &dyn ByteSource, meta_pos: u64) -> Result<Option<Vec<u8>>> {
    let mut cur = src.len().saturating_sub(HEADER_SIZE);
    for _ in 0..MAX_CHAIN_STEPS {
        if cur <= meta_pos + 6 {
            break;
        }
        let mut hdr = [0u8; 6];
        src.read_at(cur - 6, &mut hdr)?;
        let size = u64::try_from(i32_at(&hdr, 2)).unwrap_or(0);
        if size == 0 || size > MAX_FRAME || cur < 6 + size + meta_pos {
            return Ok(None);
        }
        let payload = cur - 6 - size;
        if hdr[1] == 0 {
            let idx = read(src, payload, size)?;
            for e in idx.as_chunks::<10>().0 {
                if e[0] == 1 {
                    let (sz, off) = (i32_at(e, 2), i32_at(e, 6));
                    let (Some(sz), Some(off)) = (usize::try_from(sz).ok(), u64::try_from(off).ok()) else {
                        continue;
                    };
                    if sz > 0 && sz as u64 <= MAX_FRAME && meta_pos + off + sz as u64 <= src.len() {
                        return Ok(Some(read(src, meta_pos + off, sz as u64)?));
                    }
                }
            }
            return Ok(None);
        }
        if hdr[1] == 1 && hdr[0] == 1 {
            return Ok(Some(read(src, payload, size)?));
        }
        cur = payload;
    }
    Ok(None)
}

fn read(src: &dyn ByteSource, at: u64, len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| Error::Invalid("frame too large".into()))?;
    let mut v = vec![0u8; len];
    src.read_at(at, &mut v)?;
    Ok(v)
}

/// Top-level length-delimited fields of a proto3 message whose number is in
/// `wanted`, as (number, body) pairs: unwanted fields are stepped over without
/// copying their bytes, so a message stuffed with large fields this parser does
/// not use (a firmware that embeds gyro or exif blobs in INFO) costs its real
/// content, not its whole payload, per parse. Unknown wire types stop the walk
/// (a damaged message yields what was readable before it, never a panic).
fn pb_fields(buf: &[u8], wanted: &[u32]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let Some((tag, n)) = varint(buf, i) else { break };
        i = n;
        let (Ok(num), wt) = (u32::try_from(tag >> 3), (tag & 7) as u8) else { break };
        match wt {
            0 => match varint(buf, i) {
                Some((_, n)) => i = n,
                None => break,
            },
            1 => i = i.saturating_add(8),
            5 => i = i.saturating_add(4),
            2 => match varint(buf, i) {
                Some((len, n)) => {
                    i = n;
                    let Ok(len) = usize::try_from(len) else { break };
                    let Some(end) = i.checked_add(len) else { break };
                    if end > buf.len() {
                        break;
                    }
                    if num > 0 && wanted.contains(&num) && out.len() < 64 {
                        out.push((num, buf[i..end].to_vec()));
                    }
                    i = end;
                }
                None => break,
            },
            3 | 4 => break,
            _ => break,
        }
    }
    out
}

fn varint(buf: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let &b = buf.get(i)?;
        i += 1;
        v |= u64::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
        if b & 0x80 == 0 {
            return Some((v, i));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn pb_string(buf: &[u8]) -> Option<String> {
    std::str::from_utf8(buf).ok().map(|s| s.to_string())
}

/// The first-part path lives in a submessage (field 3 of field 26).
fn pb_sub_string(buf: &[u8], want: u32) -> Option<String> {
    pb_fields(buf, &[want]).into_iter().find(|(n, _)| *n == want).and_then(|(_, b)| pb_string(&b))
}

/// `OffsetV3` (field 54): `2_` then 19 tokens per lens — xi, fx, fy, cx, cy,
/// yaw, pitch, roll, three ignored, k1, k2, k3, p1, p2, sensor_w, sensor_h,
/// two ignored.
fn parse_offset_v3(s: &str) -> Option<[Lens; 2]> {
    const TOKENS: usize = 19;
    let parts: Vec<&str> = s.split('_').collect();
    if parts.first()?.parse::<usize>().ok()? != 2 {
        return None;
    }
    let toks: Vec<f64> = parts.get(1..)?.iter().map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
    if toks.len() < 2 * TOKENS {
        return None;
    }
    let lens = |t: &[f64]| -> Option<Lens> {
        let to_u32 = |v: f64| (v.is_finite() && v >= 0.0 && v <= f64::from(u32::MAX)).then_some(v as u32);
        let (sensor_w, sensor_h) = (to_u32(t[16])?, to_u32(t[17])?);
        let l = Lens {
            xi: t[0],
            fx: t[1],
            fy: t[2],
            cx: t[3],
            cy: t[4],
            yaw: t[5],
            pitch: t[6],
            roll: t[7],
            k1: t[11],
            k2: t[12],
            k3: t[13],
            p1: t[14],
            p2: t[15],
            tx: t[8],
            ty: t[9],
            tz: t[10],
            sensor_w,
            sensor_h,
        };
        // A usable MEI lens: a mirror beyond 1, positive finite focal lengths
        // and a principal point inside the sensor. Anything else is damaged
        // metadata and must not drive a stitch.
        let ok = l.xi > 1.0
            && [l.fx, l.fy, l.cx, l.cy, l.yaw, l.pitch].iter().all(|v| v.is_finite())
            && [l.k1, l.k2, l.k3, l.p1, l.p2].iter().all(|v| v.is_finite())
            && l.fx > 0.0
            && l.fy > 0.0
            && l.sensor_w >= l.sensor_h
            && l.sensor_h > 0
            && l.cx > 0.0
            && l.cx < f64::from(l.sensor_w)
            && l.cy > 0.0
            && l.cy < f64::from(l.sensor_h);
        ok.then_some(l)
    };
    let a = lens(toks.get(..TOKENS)?)?;
    let b = lens(toks.get(TOKENS..2 * TOKENS)?)?;
    Some([a, b])
}

fn meta_from_protobuf(buf: &[u8]) -> Meta {
    let mut m = Meta::default();
    // The only fields Insta360 readers have been seen to define here; the rest
    // of a real INFO message (motion data, thumbnails…) is skipped, not copied.
    for (num, body) in pb_fields(buf, &[1, 2, 3, 22, 26, 54]) {
        match num {
            1 => m.serial = pb_string(&body),
            2 => m.model = pb_string(&body),
            3 => m.firmware = pb_string(&body),
            22 => m.color_profile = pb_string(&body),
            26 => m.first_part = pb_sub_string(&body, 3),
            54 => m.lenses = pb_string(&body).as_deref().and_then(parse_offset_v3),
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trailer header with `meta_size` and `version`.
    fn head(meta_size: i32, version: i32) -> Vec<u8> {
        let mut h = vec![0u8; 72];
        h[32..36].copy_from_slice(&meta_size.to_le_bytes());
        h[36..40].copy_from_slice(&version.to_le_bytes());
        h[40..72].copy_from_slice(SIGNATURE_BYTES);
        h
    }

    /// A frame: payload followed by its 6-byte header (headers trail payloads).
    fn frame(ver: u8, typ: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = payload.to_vec();
        f.push(ver);
        f.push(typ);
        f.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        f
    }

    fn pb_str(field: u32, s: &str) -> Vec<u8> {
        let mut v = varint_bytes((u64::from(field) << 3) | 2);
        v.extend(varint_bytes(s.len() as u64));
        v.extend_from_slice(s.as_bytes());
        v
    }

    fn varint_bytes(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    /// The X4 calibration string, as a camera writes it.
    const X4_OFFSET: &str = "2_1.948170_4623.310_4622.280_4005.910_3011.090_0.360_0.315_90.224_0.000000_0.000000_0.000000_0.39570114_1.25954950_-3.94179916_-0.00181744_0.00048328_16000_6000_71_1.948170_4615.100_4615.490_12009.110_2982.290_-0.492_0.369_89.702_0.000327_-0.000021_-0.031486_0.38433224_1.30846024_-4.00595903_0.00127593_0.00029782_16000_6000_71_197632";

    /// A whole file: some MP4 bytes, an INFO frame, an INDEX frame, the header.
    fn file_with(info: &[u8]) -> Vec<u8> {
        let mut b = b"ftypisom....mdat".to_vec();
        let meta_start = b.len() as u64;
        let info_f = frame(1, 1, info);
        b.extend_from_slice(&info_f);
        // INDEX: one entry pointing at the INFO payload
        let mut idx = Vec::new();
        idx.extend_from_slice(&[1, 1]);
        idx.extend_from_slice(&(info.len() as i32).to_le_bytes());
        idx.extend_from_slice(&0i32.to_le_bytes()); // payload offset, relative to the region
        let idx_f = frame(0, 0, &idx);
        b.extend_from_slice(&idx_f);
        let meta_size = (b.len() - meta_start as usize + 72) as i32;
        b.extend_from_slice(&head(meta_size, 3));
        b
    }

    #[test]
    fn reads_model_firmware_and_calibration() {
        let mut info = pb_str(1, "SERIAL123");
        info.extend(pb_str(2, "Insta360 X4"));
        info.extend(pb_str(3, "v1.9.21_build5"));
        info.extend(pb_str(22, "standard"));
        info.extend(pb_str(54, X4_OFFSET));
        let b = file_with(&info);
        assert!(sniff(&b));
        let m = parse(&b).expect("parse").expect("trailer");
        assert_eq!(m.model.as_deref(), Some("Insta360 X4"));
        assert_eq!(m.firmware.as_deref(), Some("v1.9.21_build5"));
        assert_eq!(m.serial.as_deref(), Some("SERIAL123"));
        assert_eq!(m.color_profile.as_deref(), Some("standard"));
        let [a, c] = m.lenses.expect("calibration");
        assert!((a.xi - 1.948170).abs() < 1e-4);
        assert!((a.fx - 4623.310).abs() < 1e-3);
        assert_eq!((a.sensor_w, a.sensor_h), (16000, 6000));
        assert!((a.cx - 4005.910).abs() < 1e-3);
        assert!((c.cx - 12009.110).abs() < 1e-3);
        assert!((a.k1 - 0.39570114).abs() < 1e-6);
    }

    #[test]
    fn no_trailer_is_none() {
        let b = vec![0u8; 200];
        assert!(!sniff(&b));
        assert_eq!(parse(&b).expect("parse"), None);
    }

    #[test]
    fn unknown_header_version_is_none() {
        let mut b = file_with(&pb_str(2, "Insta360 X4"));
        let n = b.len();
        b[n - 36..n - 32].copy_from_slice(&9i32.to_le_bytes());
        assert_eq!(parse(&b).expect("parse"), None);
    }

    /// Hostile sizes must never panic or allocate wildly: every single-byte
    /// mutation of a good file parses or errors, and never unwinds.
    #[test]
    fn mutated_trailers_never_panic() {
        let mut info = pb_str(2, "Insta360 X4");
        info.extend(pb_str(54, X4_OFFSET));
        let good = file_with(&info);
        for i in 0..good.len() {
            for delta in [1u8, 0x80, 0xff] {
                let mut b = good.clone();
                b[i] = b[i].wrapping_add(delta);
                let r = std::panic::catch_unwind(|| {
                    let m = parse(&b);
                    if let Ok(Some(m)) = &m {
                        // whatever parsed, the calibration must be usable
                        if let Some([a, _]) = m.lenses {
                            assert!(a.xi > 1.0 && a.fx > 0.0 && a.fx.is_finite());
                        }
                    }
                });
                assert!(r.is_ok(), "panic on mutation at byte {i} +{delta}");
            }
        }
    }

    #[test]
    fn truncated_files_never_panic() {
        let mut info = pb_str(2, "Insta360 X4");
        info.extend(pb_str(54, X4_OFFSET));
        let good = file_with(&info);
        for n in 0..good.len() {
            let b = good[..n].to_vec();
            assert!(
                std::panic::catch_unwind(|| {
                    let _ = sniff(&b);
                    let _ = parse(&b);
                })
                .is_ok(),
                "panic at truncation {n}"
            );
        }
    }

    #[test]
    fn offset_v3_rejects_damaged_calibration() {
        assert!(parse_offset_v3(X4_OFFSET).is_some());
        // wrong lens count
        assert!(parse_offset_v3("3_1.9_1_1_0.5_0.5_0_0_90_0_0_0_0_0_0_0_16_6_0").is_none());
        // xi <= 1 cannot drive the unified projection
        let mut v3: Vec<String> = X4_OFFSET.split('_').map(str::to_string).collect();
        v3[1] = "0.5".into();
        assert!(parse_offset_v3(&v3.join("_")).is_none());
        // non-numeric tokens are dropped, leaving too few
        assert!(parse_offset_v3("2_1.9_abc").is_none());
        // principal point outside the sensor
        let mut v3: Vec<String> = X4_OFFSET.split('_').map(str::to_string).collect();
        v3[4] = "99999".into();
        assert!(parse_offset_v3(&v3.join("_")).is_none());
    }

    #[test]
    fn scale_to_circle_moves_principal_points() {
        let [a, b] = parse_offset_v3(X4_OFFSET).expect("calib");
        let a = a.scale_to_circle(0, 2880).expect("scale");
        let b = b.scale_to_circle(1, 2880).expect("scale");
        // lens 0's centre sits just right of its circle's centre (cx 4005.9 of
        // a 4000-wide half), lens 1's just left of its own
        assert!((a.cx - 1442.8).abs() < 1.0, "{}", a.cx);
        assert!((b.cx - 1444.4).abs() < 1.0, "{}", b.cx);
        assert!((a.fx - 4623.31 * 2880.0 / 6000.0).abs() < 1.0);
        assert_eq!((a.sensor_w, a.sensor_h), (2880, 2880));
    }

    #[test]
    fn protobuf_skips_unknown_fields() {
        let mut info = vec![0x08, 0x96, 0x01]; // field 1 varint 150
        info.extend(pb_str(2, "Insta360 ONE RS")); // field 2 string
        info.extend([0x1d, 1, 2, 3, 4]); // field 3 fixed32
        info.extend(pb_str(54, X4_OFFSET));
        let b = file_with(&info);
        let m = parse(&b).expect("parse").expect("trailer");
        assert_eq!(m.model.as_deref(), Some("Insta360 ONE RS"));
        assert!(m.lenses.is_some());
    }

    /// INFO fields this parser has no use for - a firmware embedding blobs of
    /// motion data or thumbnails as length-delimited fields - must not be copied
    /// once per parse: the wanted fields still decode, and `pb_fields` returns
    /// nothing for numbers it was not asked for.
    #[test]
    fn info_junk_fields_are_skipped_without_copying() {
        let mut info = pb_str(2, "Insta360 X4");
        for _ in 0..300 {
            // field 30, wire type 2, 200 bytes of payload: unknown, and large
            info.extend(varint_bytes((30 << 3) | 2));
            info.extend(varint_bytes(200));
            info.extend_from_slice(&[7u8; 200]);
        }
        let first = "/DCIM/Camera01/VID_20261010_205908_00_054.insv";
        let sub = pb_str(3, first);
        info.extend(varint_bytes((26 << 3) | 2));
        info.extend(varint_bytes(sub.len() as u64));
        info.extend(sub);
        let b = file_with(&info);
        let m = parse(&b).expect("parse").expect("trailer");
        assert_eq!(m.model.as_deref(), Some("Insta360 X4"));
        assert_eq!(m.first_part.as_deref(), Some(first), "junk around it must not hide the fields that matter");
        assert_eq!(pb_fields(&info, &[26, 54]).len(), 1, "unkicked field numbers are never even copied");
    }
}
