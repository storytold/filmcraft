//! Recognise R3D imports even in builds without the optional native SDK backend.
use filmcraft_media::{MediaError, Result, SharedReader, SharedSource};
use std::{path::Path, sync::Arc};

pub fn is_r3d(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|s| s.eq_ignore_ascii_case("r3d"))
}

fn unavailable() -> Result<SharedSource> {
    Err(MediaError::Unsupported(
        "R3D / Nikon ZR R3D NE requires a native FilmCraft build with the red-sdk feature, RED SDK 9.2.1, and a local file. See docs/r3d-ne.md.".into(),
    ))
}

pub fn opener(name: &str, _: Arc<[u8]>) -> Option<Result<SharedSource>> {
    is_r3d(name).then(unavailable)
}

pub fn reader_opener(name: &str, _: &[u8], _: &SharedReader) -> Option<Result<SharedSource>> {
    is_r3d(name).then(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_backend_does_not_read_a_large_raw_file_into_memory() {
        struct Large;
        impl filmcraft_media::ByteReader for Large {
            fn len(&self) -> u64 {
                10 * 1024 * 1024 * 1024
            }
            fn read_at(&self, _: u64, buf: &mut [u8]) -> std::io::Result<()> {
                assert!(buf.len() <= filmcraft_media::reader::HEAD_LEN);
                buf.fill(0);
                Ok(())
            }
        }
        let result = filmcraft_media::reader::open_reader("DSC_0001.R3D", Arc::new(Large), &[reader_opener], &[opener]);
        assert!(matches!(result, Err(MediaError::Unsupported(message)) if message.contains("red-sdk")));
    }
    #[test]
    fn recognises_case_without_claiming_other_files() {
        for name in ["DSC_0001.R3D", "DSC_0002.r3d", "a.R3d"] {
            assert!(is_r3d(name));
            assert!(opener(name, Arc::from([])).unwrap().is_err());
        }
        for name in ["a.mov", "a.nev", "a.r3d.mov", "r3d", ""] {
            assert!(!is_r3d(name));
        }
    }
}
