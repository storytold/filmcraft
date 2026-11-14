//! Project thumbnails: a small picture of the project stored in the envelope beside it.
//!
//! The thumbnail is what the Program Monitor showed when the project was saved, downscaled to
//! [`THUMBNAIL_SIDE`] on its longest side and stored as base64 JPEG:
//!
//! ```json
//! "thumbnail": { "format": "jpeg", "width": 320, "height": 180, "data": "/9j/4AAQ…" }
//! ```
//!
//! Like the `view`, it is outside the schema version (older builds ignore it) and nothing in it is
//! trusted: one that cannot be read is dropped, never a load error. Writers put it before
//! `project`, so [`read_thumbnail`] finds it by reading only the start of the file.

use base64::Engine as _;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::io::Read as _;

/// Longest side of the thumbnails this build writes, in pixels.
pub const THUMBNAIL_SIDE: u32 = 320;
/// Longest side a stored thumbnail may have; larger ones are ignored.
pub const MAX_THUMBNAIL_SIDE: u32 = 1024;
/// Largest encoded (JPEG / PNG) thumbnail accepted, in bytes.
pub const MAX_THUMBNAIL_BYTES: usize = 512 * 1024;
/// Largest base64 `data` string accepted (the encoding of [`MAX_THUMBNAIL_BYTES`]).
const MAX_DATA_CHARS: usize = MAX_THUMBNAIL_BYTES.div_ceil(3) * 4;
/// [`read_thumbnail`] reads at most this much of a file: the envelope header plus the largest
/// thumbnail accepted, with room for indentation.
pub const READ_LIMIT: u64 = (MAX_DATA_CHARS + 64 * 1024) as u64;
/// JPEG quality of written thumbnails.
const JPEG_QUALITY: u8 = 80;
/// The largest picture [`Thumbnail::from_rgba8`] accepts (16K × 16K).
const MAX_INPUT_SIDE: u32 = 16384;

#[derive(Debug, thiserror::Error)]
#[error("project thumbnail: {0}")]
pub struct ThumbnailError(pub String);

/// How the thumbnail's bytes are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbnailFormat {
    Jpeg,
    Png,
}

impl ThumbnailFormat {
    fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::Png => image::ImageFormat::Png,
        }
    }

    fn has_signature(self, data: &[u8]) -> bool {
        match self {
            Self::Jpeg => data.starts_with(&[0xFF, 0xD8, 0xFF]),
            Self::Png => data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        }
    }
}

/// An encoded thumbnail, as stored in a project file. Values built by [`Thumbnail::from_rgba8`]
/// or read from a file are within the caps above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub format: ThumbnailFormat,
    pub width: u32,
    pub height: u32,
    /// The encoded image (JPEG or PNG file bytes).
    pub data: Vec<u8>,
}

/// A decoded thumbnail: `rgba` holds `width * height` straight-alpha sRGB pixels, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The JSON shape of a thumbnail.
#[derive(Serialize, Deserialize)]
pub(crate) struct ThumbnailJson {
    format: ThumbnailFormat,
    width: u32,
    height: u32,
    data: String,
}

impl Thumbnail {
    /// Make a thumbnail of an RGBA8 picture (`rgba.len() == width * height * 4`; alpha is
    /// ignored, pass an opaque picture): downscaled by area averaging so its longest side is at most
    /// [`THUMBNAIL_SIDE`], then JPEG-encoded.
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Self, ThumbnailError> {
        if width == 0 || height == 0 || width > MAX_INPUT_SIDE || height > MAX_INPUT_SIDE {
            return Err(ThumbnailError(format!("picture size {width}×{height} is out of range")));
        }
        let (w, h) = (width as usize, height as usize);
        let needed = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or_else(|| ThumbnailError("picture too large".into()))?;
        if rgba.len() != needed {
            return Err(ThumbnailError(format!("{} bytes of pixels for a {width}×{height} picture", rgba.len())));
        }
        let (tw, th) = fit(width, height, THUMBNAIL_SIDE);
        let rgb = downscale_rgb(rgba, w, h, tw as usize, th as usize);
        let mut data = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, JPEG_QUALITY),
            &rgb,
            tw,
            th,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| ThumbnailError(e.to_string()))?;
        if data.len() > MAX_THUMBNAIL_BYTES {
            return Err(ThumbnailError("encoded thumbnail is too large".into()));
        }
        Ok(Self { format: ThumbnailFormat::Jpeg, width: tw, height: th, data })
    }

    /// Decode the image. Fails (never panics) on damaged data, on an image larger than
    /// [`MAX_THUMBNAIL_SIDE`], or one whose size is not the size recorded.
    pub fn decode_rgba(&self) -> Result<ThumbnailImage, ThumbnailError> {
        let err = |e: image::ImageError| ThumbnailError(e.to_string());
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&self.data), self.format.image_format());
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_THUMBNAIL_SIDE);
        limits.max_image_height = Some(MAX_THUMBNAIL_SIDE);
        // the RGBA8 picture, plus the decoder's own buffers
        limits.max_alloc = Some(u64::from(MAX_THUMBNAIL_SIDE) * u64::from(MAX_THUMBNAIL_SIDE) * 4 * 4);
        reader.limits(limits);
        let img = reader.decode().map_err(err)?;
        if (img.width(), img.height()) != (self.width, self.height) {
            return Err(ThumbnailError(format!("image is {}×{}, the file says {}×{}", img.width(), img.height(), self.width, self.height)));
        }
        Ok(ThumbnailImage { width: self.width, height: self.height, rgba: img.into_rgba8().into_raw() })
    }

    pub(crate) fn to_json(&self) -> ThumbnailJson {
        ThumbnailJson { format: self.format, width: self.width, height: self.height, data: base64::engine::general_purpose::STANDARD.encode(&self.data) }
    }

    /// Validate a thumbnail read from a file; `None` when it is not one this build accepts.
    fn from_json(j: ThumbnailJson) -> Option<Self> {
        let side_ok = |v: u32| (1..=MAX_THUMBNAIL_SIDE).contains(&v);
        if !side_ok(j.width) || !side_ok(j.height) || j.data.len() > MAX_DATA_CHARS {
            return None;
        }
        let data = base64::engine::general_purpose::STANDARD.decode(j.data.as_bytes()).ok()?;
        if data.is_empty() || data.len() > MAX_THUMBNAIL_BYTES || !j.format.has_signature(&data) {
            return None;
        }
        Some(Self { format: j.format, width: j.width, height: j.height, data })
    }

    /// The thumbnail held in a JSON value (an envelope's `thumbnail`), or `None` when it cannot be read.
    pub fn from_value(v: serde_json::Value) -> Option<Self> {
        serde_json::from_value::<ThumbnailJson>(v).ok().and_then(Self::from_json)
    }
}

/// `width × height` scaled to fit `side` on its longest side (never enlarged, at least 1 × 1).
fn fit(width: u32, height: u32, side: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= side || longest == 0 {
        return (width.max(1), height.max(1));
    }
    let scale = |v: u32| ((u64::from(v) * u64::from(side) + u64::from(longest) / 2) / u64::from(longest)).clamp(1, u64::from(side)) as u32;
    (scale(width), scale(height))
}

/// Area-average `rgba` (`w × h`, validated by the caller) down to `tw × th` RGB8.
fn downscale_rgb(rgba: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(tw.saturating_mul(th).saturating_mul(3));
    for ty in 0..th {
        let (y0, y1) = span(ty, th, h);
        for tx in 0..tw {
            let (x0, x1) = span(tx, tw, w);
            let mut sum = [0u64; 3];
            let mut n = 0u64;
            for y in y0..y1 {
                let row = y.saturating_mul(w);
                for x in x0..x1 {
                    let i = row.saturating_add(x).saturating_mul(4);
                    if let Some(p) = rgba.get(i..i.saturating_add(3)) {
                        for (s, v) in sum.iter_mut().zip(p) {
                            *s += u64::from(*v);
                        }
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            out.extend(sum.map(|s| ((s + n / 2) / n).min(255) as u8));
        }
    }
    out
}

/// The source pixels `[a, b)` that output pixel `i` of `out` covers in a line of `len`.
fn span(i: usize, out: usize, len: usize) -> (usize, usize) {
    let out = out.max(1);
    let a = i.saturating_mul(len) / out;
    let b = (i.saturating_add(1).saturating_mul(len) / out).max(a.saturating_add(1)).min(len);
    (a.min(len.saturating_sub(1)), b)
}

/// Find the thumbnail in the start of a project file without reading the project: the envelope's
/// keys are scanned up to `project` (writers put the thumbnail before it). `None` when the bytes
/// are not a FilmCraft project, have no thumbnail there, or it cannot be read; never an error.
/// `bytes` may be cut off anywhere after the thumbnail.
pub fn scan_thumbnail(bytes: &[u8]) -> Option<Thumbnail> {
    struct Finder<'a>(&'a mut Option<ThumbnailJson>);
    impl<'de> Visitor<'de> for Finder<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a project envelope")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "thumbnail" => {
                        *self.0 = Some(map.next_value()?);
                        return Ok(());
                    }
                    "format" => {
                        let format: String = map.next_value()?;
                        if format != crate::FORMAT_ID {
                            return Ok(());
                        }
                    }
                    // the project (megabytes) and what follows it are not read
                    "project" => return Ok(()),
                    _ => {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
            }
            Ok(())
        }
    }
    let mut found = None;
    let mut de = serde_json::Deserializer::from_slice(bytes);
    // Stopping early leaves the rest of the document unread, which serde_json reports as an
    // error: what was found before it is what counts.
    let _ = serde::Deserializer::deserialize_map(&mut de, Finder(&mut found));
    found.and_then(Thumbnail::from_json)
}

/// Read the thumbnail of the project file at `path`, decoded, for a Recent Projects list. Reads at
/// most [`READ_LIMIT`] bytes (about 0.75 MB) from the start of the file whatever the project's
/// size, and never builds the project. `Ok(None)` when the file has no readable thumbnail (older
/// builds, saved without an open sequence, damaged, not a project); `Err` only when the file
/// cannot be opened or read. Blocking file I/O plus a JPEG decode (a few ms): call it off the UI
/// thread and cache the result.
pub fn read_thumbnail(path: &std::path::Path) -> std::io::Result<Option<ThumbnailImage>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(READ_LIMIT).read_to_end(&mut bytes)?;
    Ok(scan_thumbnail(&bytes).and_then(|t| t.decode_rgba().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32) -> Vec<u8> {
        (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 128, 255]).collect()
    }

    #[test]
    fn a_picture_is_downscaled_to_fit_and_decodes_back() {
        let t = Thumbnail::from_rgba8(1920, 1080, &picture(1920, 1080)).unwrap();
        assert_eq!((t.format, t.width, t.height), (ThumbnailFormat::Jpeg, 320, 180));
        let img = t.decode_rgba().unwrap();
        assert_eq!((img.width, img.height, img.rgba.len()), (320, 180, 320 * 180 * 4));
        // area averaging keeps the gradient: left is dark red, right bright
        assert!(img.rgba[0] < 20 && img.rgba[(319 * 4) as usize] > 230, "{} {}", img.rgba[0], img.rgba[319 * 4]);
        // portrait and tiny pictures: fit, never enlarged
        let t = Thumbnail::from_rgba8(1080, 1920, &picture(1080, 1920)).unwrap();
        assert_eq!((t.width, t.height), (180, 320));
        let t = Thumbnail::from_rgba8(3, 2, &picture(3, 2)).unwrap();
        assert_eq!((t.width, t.height), (3, 2));
        let t = Thumbnail::from_rgba8(16384, 1, &picture(16384, 1)).unwrap();
        assert_eq!((t.width, t.height), (320, 1));
    }

    #[test]
    fn bad_pictures_are_errors() {
        assert!(Thumbnail::from_rgba8(0, 10, &[]).is_err());
        assert!(Thumbnail::from_rgba8(10, 10, &[0; 399]).is_err());
        assert!(Thumbnail::from_rgba8(u32::MAX, u32::MAX, &[0; 4]).is_err());
        assert!(Thumbnail::from_rgba8(16385, 1, &[0; 16385 * 4]).is_err());
    }

    #[test]
    fn json_round_trip_and_hostile_values_are_dropped() {
        let t = Thumbnail::from_rgba8(64, 36, &picture(64, 36)).unwrap();
        let v = serde_json::to_value(t.to_json()).unwrap();
        assert_eq!(Thumbnail::from_value(v.clone()), Some(t.clone()));
        let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
        let mut big = vec![0xFF, 0xD8, 0xFF];
        big.resize(MAX_THUMBNAIL_BYTES + 1, 0);
        for junk in [
            serde_json::json!(null),
            serde_json::json!(7),
            serde_json::json!("x"),
            serde_json::json!({"format": "gif", "width": 1, "height": 1, "data": ""}),
            serde_json::json!({"format": "jpeg", "width": 0, "height": 1, "data": v["data"]}),
            serde_json::json!({"format": "jpeg", "width": 64, "height": 4_000_000_000u64, "data": v["data"]}),
            serde_json::json!({"format": "jpeg", "width": 2048, "height": 36, "data": v["data"]}),
            serde_json::json!({"format": "jpeg", "width": 64, "height": 36, "data": "not base64!"}),
            serde_json::json!({"format": "jpeg", "width": 64, "height": 36, "data": ""}),
            serde_json::json!({"format": "png", "width": 64, "height": 36, "data": v["data"]}),
            serde_json::json!({"format": "jpeg", "width": 64, "height": 36, "data": b64(&big)}),
            serde_json::json!({"format": "jpeg", "width": 64, "height": 36}),
        ] {
            assert_eq!(Thumbnail::from_value(junk.clone()), None, "{junk}");
        }
    }

    /// Damaged image bytes behind a valid header never panic and never decode to the wrong size.
    #[test]
    fn mutated_thumbnails_fail_cleanly() {
        let t = Thumbnail::from_rgba8(64, 36, &picture(64, 36)).unwrap();
        // a stored size that does not match the image
        let wrong = Thumbnail { width: 63, ..t.clone() };
        assert!(wrong.decode_rgba().is_err());
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..400 {
            let mut data = t.data.clone();
            match round % 3 {
                0 => data.truncate((next() as usize) % data.len()),
                1 => {
                    for _ in 0..4 {
                        let i = (next() as usize) % data.len();
                        data[i] ^= 1 << (next() % 8);
                    }
                }
                _ => {
                    let i = (next() as usize) % data.len().saturating_sub(4).max(1);
                    data[i..i + 2].copy_from_slice(&[0xFF, 0xFF]);
                }
            }
            let m = Thumbnail { data, ..t.clone() };
            let r = std::panic::catch_unwind(|| m.decode_rgba());
            match r {
                Ok(Ok(img)) => assert_eq!(img.rgba.len(), 64 * 36 * 4),
                Ok(Err(_)) => {}
                Err(_) => panic!("round {round}: decoding a damaged thumbnail panicked"),
            }
        }
        // a PNG claiming to be huge is refused by the limits before it allocates
        let mut png = Vec::new();
        image::ImageEncoder::write_image(image::codecs::png::PngEncoder::new(&mut png), &[0u8; 4], 1, 1, image::ExtendedColorType::Rgba8).unwrap();
        png[16..20].copy_from_slice(&60000u32.to_be_bytes());
        png[20..24].copy_from_slice(&60000u32.to_be_bytes());
        assert!(Thumbnail { format: ThumbnailFormat::Png, width: 1, height: 1, data: png }.decode_rgba().is_err());
    }

    #[test]
    fn scanning_stops_at_the_project_and_ignores_junk() {
        let t = Thumbnail::from_rgba8(64, 36, &picture(64, 36)).unwrap();
        let thumb = serde_json::to_string(&t.to_json()).unwrap();
        let doc = format!(r#"{{"format":"filmcraft.project","schema_version":15,"generator":"x","thumbnail":{thumb},"project":{{"#);
        // cut off in the project: still found
        assert_eq!(scan_thumbnail(doc.as_bytes()), Some(t.clone()));
        // after the project: not looked for
        let late = format!(r#"{{"format":"filmcraft.project","project":{{}},"thumbnail":{thumb}}}"#);
        assert_eq!(scan_thumbnail(late.as_bytes()), None);
        // another format's file
        let foreign = format!(r#"{{"format":"other","thumbnail":{thumb}}}"#);
        assert_eq!(scan_thumbnail(foreign.as_bytes()), None);
        for junk in [
            &b""[..],
            b"[]",
            b"7",
            b"{",
            b"{\"thumbnail\":",
            b"{\"thumbnail\":[1,2]}",
            b"\xff\xfe",
            b"{\"a\":{\"b\":[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]}}",
        ] {
            assert_eq!(scan_thumbnail(junk), None);
        }
        // every truncation of a document is handled
        for n in 0..doc.len() {
            let _ = scan_thumbnail(&doc.as_bytes()[..n]);
        }
        // very deep nesting before the thumbnail is refused by serde_json's recursion limit, not a stack overflow
        let deep = format!("{{\"x\":{}{}}}", "[".repeat(100_000), "]".repeat(100_000));
        assert_eq!(scan_thumbnail(deep.as_bytes()), None);
    }

    #[test]
    fn read_thumbnail_reads_only_the_start_of_a_file() {
        let dir = std::env::temp_dir().join(format!("fc-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Thumbnail::from_rgba8(64, 36, &picture(64, 36)).unwrap();
        let thumb = serde_json::to_string(&t.to_json()).unwrap();
        let path = dir.join("big.fcproj");
        // a "project" far larger than the read limit, and not even valid JSON past it
        let mut doc = format!(r#"{{"format":"filmcraft.project","schema_version":15,"thumbnail":{thumb},"project":{{"pad":""#).into_bytes();
        doc.resize(READ_LIMIT as usize * 3, b'a');
        std::fs::write(&path, &doc).unwrap();
        let img = read_thumbnail(&path).unwrap().unwrap();
        assert_eq!((img.width, img.height), (64, 36));
        std::fs::write(&path, b"garbage").unwrap();
        assert_eq!(read_thumbnail(&path).unwrap(), None);
        assert!(read_thumbnail(&dir.join("missing.fcproj")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
