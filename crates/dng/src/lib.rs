//! Clean-room DNG and CinemaDNG decoder, implemented from the public specifications only (see the
//! crate README for editions): the TIFF 6.0 / TIFF-EP container, uncompressed and lossless JPEG
//! (ITU-T T.81 process 14) raw data, and the DNG processing model (linearisation, black and white
//! levels, white balance, demosaic, default crop, colour matrices) to a scene-linear Rec. 709
//! picture.
//!
//! - [`sniff`]: is this a DNG (a TIFF with a DNGVersion tag)?
//! - [`Dng::parse`] → [`Dng::info`] (size, bit depth, CinemaDNG frame rate and time code),
//!   [`Dng::raw`] (the raw samples) and [`Dng::develop`] (linear RGBA floats).
//! - [`decode`]: parse and develop in one call.
//!
//! Every number in the file is hostile: offsets, counts and sizes are checked, allocations are
//! capped by [`raw::MAX_SAMPLES`] and by the amount of data present, and the IFD walk is bounded.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod color;
pub mod demosaic;
pub mod ljpeg;
pub mod opcode;
pub mod options;
pub mod process;
pub mod raw;
#[cfg(any(test, feature = "writer"))]
pub mod reference;
pub mod tiff;
#[cfg(any(test, feature = "writer"))]
pub mod writer;

use color::Calibration;
use demosaic::Pattern;
pub use options::{DemosaicQuality, Highlights, OutputSpace, ProcessOptions, WhiteBalance};
use process::Params;
use raw::RawImage;
use tiff::{Entry, Ifd, Tiff, tags};

/// DNG decoding errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a DNG file")]
    NotDng,
    #[error("truncated DNG data")]
    Truncated,
    #[error("raw image too large")]
    TooLarge,
    #[error("malformed DNG: {0}")]
    Malformed(&'static str),
    #[error("unsupported DNG: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Whether `b` is a DNG: a TIFF file whose first IFD has a DNGVersion tag.
pub fn sniff(b: &[u8]) -> bool {
    let Ok((t, first)) = Tiff::new(b) else { return false };
    t.read_ifd(first, 0).is_ok_and(|(ifd, _)| ifd.get(tags::DNG_VERSION).is_some())
}

/// SMPTE ST 12-1 time code of a CinemaDNG frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl Timecode {
    /// The first time code of a CinemaDNG TimeCodes tag (SMPTE ST 331 / 12-1 packing: BCD frames,
    /// seconds, minutes, hours in bytes 0–3, the drop-frame flag in bit 6 of byte 0).
    pub fn from_bytes(b: &[u8]) -> Option<Timecode> {
        let bcd = |v: u8, tens_mask: u8| {
            let (u, t) = (v & 0x0F, (v >> 4) & tens_mask);
            (u <= 9).then_some(t * 10 + u)
        };
        let [f, s, m, h, ..] = *b else { return None };
        let tc = Timecode { frames: bcd(f, 0x3)?, seconds: bcd(s, 0x7)?, minutes: bcd(m, 0x7)?, hours: bcd(h, 0x3)?, drop_frame: f & 0x40 != 0 };
        (tc.seconds < 60 && tc.minutes < 60 && tc.hours < 24).then_some(tc)
    }
}

/// What a DNG holds, without decoding the pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// Size of the developed picture (the default crop).
    pub width: usize,
    pub height: usize,
    /// Size of the stored raw image.
    pub raw_width: usize,
    pub raw_height: usize,
    pub bits: u32,
    pub samples_per_pixel: usize,
    /// Colour filter array data (else LinearRaw).
    pub cfa: bool,
    /// "uncompressed", "lossless JPEG" or "lossy JPEG".
    pub compression: &'static str,
    pub dng_version: [u8; 4],
    /// CinemaDNG FrameRate (numerator, denominator).
    pub frame_rate: Option<(i64, i64)>,
    /// CinemaDNG time code of this frame.
    pub timecode: Option<Timecode>,
    /// UniqueCameraModel, else Make and Model.
    pub camera: Option<String>,
    /// CinemaDNG ReelName.
    pub reel: Option<String>,
}

/// A developed DNG: premultiplied, scene-linear Rec. 709 RGBA floats (alpha 1), top row first.
/// A white surface the sensor saw at its clipping level is 1.0 times 2^BaselineExposure.
pub struct Image {
    pub info: Info,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<f32>,
}

/// A parsed DNG file.
pub struct Dng<'a> {
    tiff: Tiff<'a>,
    ifds: Vec<Ifd>,
    /// Index of the main raw IFD in `ifds` (the first is IFD 0).
    raw: usize,
}

impl<'a> Dng<'a> {
    /// Parse the container and find the main raw image.
    pub fn parse(bytes: &'a [u8]) -> Result<Dng<'a>> {
        let (tiff, first) = Tiff::new(bytes)?;
        let ifds = tiff.all_ifds(first)?;
        if ifds.first().and_then(|i| i.get(tags::DNG_VERSION)).is_none() {
            return Err(Error::NotDng);
        }
        let main = raw::find_raw_ifd(&tiff, &ifds)?;
        let raw = ifds.iter().position(|i| i.offset == main.offset).ok_or(Error::Malformed("raw IFD"))?;
        Ok(Dng { tiff, ifds, raw })
    }

    fn ifd0(&self) -> &Ifd {
        static EMPTY: Ifd = Ifd { offset: 0, entries: Vec::new(), depth: 0 };
        self.ifds.first().unwrap_or(&EMPTY)
    }

    fn raw_ifd(&self) -> &Ifd {
        self.ifds.get(self.raw).unwrap_or(self.ifd0())
    }

    /// A camera-wide tag: from IFD 0, else the raw IFD.
    fn main_tag(&self, tag: u16) -> Option<&Entry> {
        self.ifd0().get(tag).or_else(|| self.raw_ifd().get(tag))
    }

    fn metadata(&self) -> craft_raw::dng_metadata::Metadata<'_, '_> {
        craft_raw::dng_metadata::Metadata { tiff: self.tiff.inner(), main: self.ifd0(), raw: self.raw_ifd() }
    }

    fn text(&self, tag: u16) -> Option<String> {
        self.main_tag(tag).and_then(|e| self.tiff.ascii(e).ok()).filter(|s| !s.is_empty())
    }

    fn cfa(&self) -> Result<Option<Pattern>> {
        Ok(self.metadata().cfa().map_err(raw::error)?.map(|p| Pattern { rows: p.rows, cols: p.cols, colors: p.colors }))
    }

    /// The processing tags of the raw image.
    pub fn params(&self) -> Result<Params> {
        self.params_with(true)
    }

    fn params_with(&self, opcodes: bool) -> Result<Params> {
        let p = self.metadata().params().map_err(raw::error)?;
        Ok(Params {
            active_area: p.active_area,
            linearization: p.linearization,
            black_repeat: p.black_repeat,
            black: p.black,
            black_delta_h: p.black_delta_h,
            black_delta_v: p.black_delta_v,
            white: p.white,
            crop_origin: p.crop_origin,
            crop_size: p.crop_size,
            cfa: p.cfa.map(|p| Pattern { rows: p.rows, cols: p.cols, colors: p.colors }),
            opcodes: opcode::Lists {
                list1: if opcodes { self.opcode_list(tags::OPCODE_LIST_1)? } else { Vec::new() },
                list2: if opcodes { self.opcode_list(tags::OPCODE_LIST_2)? } else { Vec::new() },
                list3: if opcodes { self.opcode_list(tags::OPCODE_LIST_3)? } else { Vec::new() },
            },
        })
    }

    fn opcode_list(&self, tag: u16) -> Result<Vec<opcode::Opcode>> {
        match self.raw_ifd().get(tag) {
            Some(e) => opcode::parse(self.tiff.value_bytes(e)?),
            None => Ok(Vec::new()),
        }
    }

    /// The colour tags.
    pub fn calibration(&self) -> Result<Calibration> {
        let c = self.metadata().calibration().map_err(raw::error)?;
        Ok(Calibration {
            color_matrix: c.color_matrix,
            camera_calibration: c.camera_calibration,
            forward_matrix: c.forward_matrix,
            illuminant: c.illuminant,
            analog_balance: c.analog_balance,
            as_shot_neutral: c.as_shot_neutral,
            as_shot_white_xy: c.as_shot_white_xy,
            baseline_exposure: c.baseline_exposure,
        })
    }

    /// Size, depth and CinemaDNG metadata.
    pub fn info(&self) -> Result<Info> {
        let l = raw::layout(&self.tiff, self.raw_ifd())?;
        let p = self.params_with(false)?;
        let (_, _, aw, ah) = process::active_rect(&p, l.width, l.height)?;
        let (_, _, width, height) = process::crop_rect(&p, aw, ah);
        let dng_version = self
            .main_tag(tags::DNG_VERSION)
            .and_then(|e| self.tiff.uints(e).ok())
            .and_then(|v| v.get(..4).map(|v| [0, 1, 2, 3].map(|i| v[i] as u8)))
            .unwrap_or_default();
        let frame_rate = self.main_tag(tags::FRAME_RATE).and_then(|e| {
            let b = self.tiff.value_bytes(e).ok()?;
            if !matches!(e.ty, tiff::Type::SRational | tiff::Type::Rational) {
                return None;
            }
            let n = i64::from(self.tiff.u32_of(b.get(0..4)?.try_into().ok()?) as i32);
            let d = i64::from(self.tiff.u32_of(b.get(4..8)?.try_into().ok()?) as i32);
            (n > 0 && d > 0 && n <= 1000 * d && n * 1000 >= d).then_some((n, d))
        });
        let timecode = self.main_tag(tags::TIME_CODES).and_then(|e| self.tiff.value_bytes(e).ok()).and_then(Timecode::from_bytes);
        let camera = self.text(tags::UNIQUE_CAMERA_MODEL).or_else(|| {
            let mm = [self.text(tags::MAKE), self.text(tags::MODEL)].into_iter().flatten().collect::<Vec<_>>().join(" ");
            (!mm.is_empty()).then_some(mm)
        });
        Ok(Info {
            width,
            height,
            raw_width: l.width,
            raw_height: l.height,
            bits: l.bits,
            samples_per_pixel: l.samples_per_pixel,
            cfa: p.cfa.is_some(),
            compression: match l.compression {
                7 => "lossless JPEG",
                34892 => "lossy JPEG",
                _ => "uncompressed",
            },
            dng_version,
            frame_rate,
            timecode,
            camera,
            reel: self.text(tags::REEL_NAME),
        })
    }

    /// The raw samples of the main image, as stored (before any processing).
    pub fn raw(&self) -> Result<RawImage> {
        raw::read(&self.tiff, self.raw_ifd())
    }

    /// The as-shot white balance as (temperature in kelvin, tint), for a settings panel to start
    /// from ([`WhiteBalance::Custom`] with these values develops like [`WhiteBalance::AsShot`]).
    /// `None` for monochrome files and files without a ColorMatrix.
    pub fn as_shot_temperature_tint(&self) -> Result<Option<(f64, f64)>> {
        let calibration = self.calibration()?;
        if calibration.color_matrix.iter().all(Option::is_none) || self.cfa()?.is_none() && raw::layout(&self.tiff, self.raw_ifd())?.samples_per_pixel == 1 {
            return Ok(None);
        }
        Ok(color::xy_to_temperature_tint(calibration.solve()?.white_xy))
    }

    /// Develop the main image to scene-linear Rec. 709 as the file says
    /// ([`ProcessOptions::default`]).
    pub fn develop(&self) -> Result<Image> {
        self.develop_with(&ProcessOptions::default())
    }

    /// Develop the main image with raw settings.
    pub fn develop_with(&self, o: &ProcessOptions) -> Result<Image> {
        let info = self.info()?;
        let p = self.params_with(o.apply_opcodes)?;
        let mut raw = self.raw()?;
        let preview = process::preview(&p, o, &info);
        process::apply_list1(&mut raw, &p, preview)?;
        let mut calibration = self.calibration()?;
        calibration.baseline_exposure += o.exposure_stops();
        if let WhiteBalance::Custom { temperature, tint } = o.white_balance
            && calibration.color_matrix.iter().any(Option::is_some)
        {
            let xy = color::temperature_tint_to_xy(temperature, tint).ok_or(Error::Unsupported("a non-finite white balance".into()))?;
            calibration.as_shot_neutral = None;
            calibration.as_shot_white_xy = Some(xy);
        }
        let solved = if p.cfa.is_none() && raw.samples_per_pixel == 1 {
            // monochrome: no colour, only the exposure
            let e = 2f64.powf(calibration.baseline_exposure.clamp(-16.0, 16.0));
            color::Solved { neutral: [1.0; 3], matrix: color::scale(&color::IDENTITY, e), white_xy: color::D65 }
        } else {
            calibration.solve()?
        };
        let d = process::develop_with(&raw, &p, &solved.neutral, &solved.matrix, o)?;
        Ok(Image { info, width: d.width, height: d.height, rgba: d.rgba })
    }
}

/// Parse and develop a DNG file.
pub fn decode(bytes: &[u8]) -> Result<Image> {
    Dng::parse(bytes)?.develop()
}

/// Parse and develop a DNG file with raw settings.
pub fn decode_with(bytes: &[u8], o: &ProcessOptions) -> Result<Image> {
    Dng::parse(bytes)?.develop_with(o)
}

/// Parse a DNG file's metadata.
pub fn probe(bytes: &[u8]) -> Result<Info> {
    Dng::parse(bytes)?.info()
}
