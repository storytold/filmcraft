//! Lossless JPEG (T.81 process 14) decoding for DNG, using the shared line-restart decoder.

use crate::{Error, Result};
pub use craft_raw::ljpeg_lines::{Decoded, predict};

/// Decode a lossless JPEG stream, capping the total number of samples.
pub fn decode(data: &[u8], max_samples: usize) -> Result<Decoded> {
    craft_raw::ljpeg_lines::decode(data, max_samples).map_err(|e| match e {
        craft_raw::ljpeg_lines::Error::Truncated => Error::Truncated,
        craft_raw::ljpeg_lines::Error::TooLarge => Error::TooLarge,
        craft_raw::ljpeg_lines::Error::Malformed(s) => Error::Malformed(s),
        craft_raw::ljpeg_lines::Error::Unsupported(s) => Error::Unsupported(s),
    })
}
