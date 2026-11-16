//! Bounded classic TIFF access for DNG, implemented by the shared TIFF library.

use crate::{Error, Result};
pub use craft_tiff::classic::{Entry, Ifd, MAX_DEPTH, MAX_IFDS, Type, tags};

fn error(e: craft_tiff::classic::Error) -> Error {
    match e {
        craft_tiff::classic::Error::NotTiff => Error::NotDng,
        craft_tiff::classic::Error::Malformed(s) => Error::Malformed(s),
    }
}

/// A classic TIFF file in memory.
#[derive(Clone, Copy)]
pub struct Tiff<'a> {
    pub data: &'a [u8],
    pub big_endian: bool,
}
impl<'a> Tiff<'a> {
    fn inner(&self) -> craft_tiff::classic::Tiff<'a> {
        craft_tiff::classic::Tiff { data: self.data, big_endian: self.big_endian }
    }
    /// Read the classic TIFF header and first IFD offset.
    pub fn new(data: &'a [u8]) -> Result<(Self, u64)> {
        let (t, first) = craft_tiff::classic::Tiff::new(data).map_err(error)?;
        Ok((Self { data: t.data, big_endian: t.big_endian }, first))
    }
    /// Read an endian-aware 16-bit integer.
    pub fn u16_of(&self, bytes: [u8; 2]) -> u16 {
        self.inner().u16_of(bytes)
    }
    /// Read an endian-aware 32-bit integer.
    pub fn u32_of(&self, bytes: [u8; 4]) -> u32 {
        self.inner().u32_of(bytes)
    }
    /// Read one IFD and its next offset.
    pub fn read_ifd(&self, off: u64, depth: usize) -> Result<(Ifd, u64)> {
        self.inner().read_ifd(off, depth).map_err(error)
    }
    /// Walk the bounded main and SubIFD chains.
    pub fn all_ifds(&self, first: u64) -> Result<Vec<Ifd>> {
        self.inner().all_ifds(first).map_err(error)
    }
    /// Borrow the bytes of an entry's value.
    pub fn value_bytes(&self, entry: &Entry) -> Result<&'a [u8]> {
        self.inner().value_bytes(entry).map_err(error)
    }
    /// Read numeric values as floats.
    pub fn floats(&self, entry: &Entry) -> Result<Vec<f64>> {
        self.inner().floats(entry).map_err(error)
    }
    /// Read numeric values as unsigned integers.
    pub fn uints(&self, entry: &Entry) -> Result<Vec<u64>> {
        self.inner().uints(entry).map_err(error)
    }
    /// Read the first unsigned integer.
    pub fn uint(&self, entry: &Entry) -> Result<u64> {
        self.inner().uint(entry).map_err(error)
    }
    /// Read an ASCII value.
    pub fn ascii(&self, entry: &Entry) -> Result<String> {
        self.inner().ascii(entry).map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_header_preserves_filmcraft_error() {
        assert!(matches!(Tiff::new(b"bad"), Err(Error::NotDng)));
    }
}
