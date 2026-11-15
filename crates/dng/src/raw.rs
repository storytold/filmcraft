//! Bounded DNG raw layout and strip/tile decoding from the shared RAW library.

use crate::tiff::{Ifd, Tiff};
use crate::{Error, Result};
pub use craft_raw::dng_lines::{MAX_SAMPLES, PHOTOMETRIC_CFA, PHOTOMETRIC_LINEAR_RAW, RawImage, RawLayout};

fn error(e: craft_raw::dng_lines::Error) -> Error {
    match e {
        craft_raw::dng_lines::Error::NotTiff => Error::NotDng,
        craft_raw::dng_lines::Error::Truncated => Error::Truncated,
        craft_raw::dng_lines::Error::TooLarge => Error::TooLarge,
        craft_raw::dng_lines::Error::Malformed(s) => Error::Malformed(s),
        craft_raw::dng_lines::Error::Unsupported(s) => Error::Unsupported(s),
    }
}

/// Find the largest full-resolution raw IFD.
pub fn find_raw_ifd<'i>(t: &Tiff, ifds: &'i [Ifd]) -> Result<&'i Ifd> {
    craft_raw::dng_lines::find_raw_ifd(&t.inner(), ifds).map_err(error)
}
/// Read and validate a raw IFD's layout.
pub fn layout(t: &Tiff, ifd: &Ifd) -> Result<RawLayout> {
    craft_raw::dng_lines::layout(&t.inner(), ifd).map_err(error)
}
/// Decode the raw samples of an IFD.
pub fn read(t: &Tiff, ifd: &Ifd) -> Result<RawImage> {
    craft_raw::dng_lines::read(&t.inner(), ifd).map_err(error)
}
