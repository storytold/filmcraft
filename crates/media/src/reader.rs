//! Random-access byte readers: open media without loading the whole file.
//!
//! A [`ByteReader`] serves reads at any offset (a native file, a web `Blob` read in chunks, an
//! in-memory buffer). Container openers that only need the index and then single samples (MP4/MOV,
//! Matroska) open from a reader ([`ReaderOpener`]); formats decoded in one go (stills, WAV,
//! standalone compressed audio) fall back to reading the whole file through it.

use std::io;
use std::sync::Arc;

use crate::{MediaError, Opener, Result, SharedSource};

/// Random-access, read-only bytes.
pub trait ByteReader: Send + Sync {
    /// Total length in bytes.
    fn len(&self) -> u64;
    /// Fill `buf` entirely from `offset`; `UnexpectedEof` past the end. Asynchronous readers fail
    /// with `WouldBlock` (and set [`crate::pending`]) while the range is being fetched.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub type SharedReader = Arc<dyn ByteReader>;

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "read past end of byte source")
}

/// An in-memory reader.
pub struct MemReader(pub Arc<[u8]>);

impl ByteReader for MemReader {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let a = usize::try_from(offset).map_err(|_| eof())?;
        let src = self.0.get(a..a.checked_add(buf.len()).ok_or_else(eof)?).ok_or_else(eof)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// A file on disk, read in place: the file stays open and only the requested ranges are read, so
/// opening a clip costs its index rather than its size.
#[cfg(any(unix, windows))]
#[derive(Debug)]
pub struct FileReader {
    file: std::fs::File,
    len: u64,
}

#[cfg(any(unix, windows))]
impl FileReader {
    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let m = file.metadata()?;
        if !m.is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a file", path.display())));
        }
        Ok(Self { file, len: m.len() })
    }
}

#[cfg(any(unix, windows))]
impl ByteReader for FileReader {
    fn len(&self) -> u64 {
        self.len
    }
    /// Positional reads: threads decoding different parts of the file do not share a cursor.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_exact_at(&self.file, buf, offset)
        }
        #[cfg(windows)]
        {
            let (mut done, mut at) = (0usize, offset);
            while let Some(rest) = buf.get_mut(done..).filter(|r| !r.is_empty()) {
                match std::os::windows::fs::FileExt::seek_read(&self.file, rest, at) {
                    Ok(0) => return Err(eof()),
                    Ok(n) => {
                        done = done.saturating_add(n);
                        at = at.saturating_add(n as u64);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }
}

/// Read `len` bytes at `offset` (fewer at the end of the reader).
pub fn read_range(r: &dyn ByteReader, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let n = (r.len().saturating_sub(offset)).min(len as u64) as usize;
    let mut v = vec![0u8; n];
    r.read_at(offset, &mut v)?;
    Ok(v)
}

/// Opens a container from a reader: `head` is the first bytes of the file (for sniffing).
/// Returns `None` when the format is not this opener's.
pub type ReaderOpener = fn(name: &str, head: &[u8], reader: &SharedReader) -> Option<Result<SharedSource>>;

/// Bytes sniffed by [`open_reader`].
pub const HEAD_LEN: usize = 64 * 1024;

/// Open media from a reader: reader openers first; otherwise read the whole file and use the
/// byte openers (`extra`, then stills / WAV).
pub fn open_reader(name: &str, reader: SharedReader, reader_openers: &[ReaderOpener], extra: &[Opener]) -> Result<SharedSource> {
    open_reader_within(name, reader, reader_openers, extra, u64::MAX)
}

/// Like [`open_reader`], but a file no reader opener takes is read whole only up to `max_whole`
/// bytes; a larger one is refused instead. For looking at files rather than importing them (the
/// Media Browser's properties and thumbnails, #157): reading a multi-gigabyte AVI or WAV into
/// memory to show its duration, or to find out it isn't supported, thrashes the disk.
pub fn open_reader_within(name: &str, reader: SharedReader, reader_openers: &[ReaderOpener], extra: &[Opener], max_whole: u64) -> Result<SharedSource> {
    let head = read_range(&*reader, 0, HEAD_LEN).map_err(|e| MediaError::Io(format!("{name}: {e}")))?;
    for o in reader_openers {
        if let Some(r) = o(name, &head, &reader) {
            return r;
        }
    }
    if reader.len() > max_whole {
        return Err(MediaError::Unsupported(format!("{name}: no streaming reader for this format, and it is too large to read whole here")));
    }
    let all = read_range(&*reader, 0, usize::try_from(reader.len()).unwrap_or(usize::MAX)).map_err(|e| MediaError::Io(format!("{name}: {e}")))?;
    crate::open_bytes(name, all.into(), extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_reader_reads_ranges_and_reports_eof() {
        let r = MemReader(Arc::from(&b"0123456789"[..]));
        let mut b = [0u8; 3];
        r.read_at(4, &mut b).unwrap();
        assert_eq!(&b, b"456");
        assert_eq!(r.read_at(8, &mut b).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_range(&r, 8, 100).unwrap(), b"89");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn file_reader_reads_ranges_in_place() {
        let dir = std::env::temp_dir().join(format!("filmcraft-file-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bytes.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let r = FileReader::open(&path).unwrap();
        assert_eq!(r.len(), 10);
        let mut b = [0u8; 3];
        r.read_at(4, &mut b).unwrap();
        assert_eq!(&b, b"456");
        // reads are positional: an earlier offset after a later one
        r.read_at(0, &mut b).unwrap();
        assert_eq!(&b, b"012");
        assert_eq!(r.read_at(8, &mut b).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_range(&r, 8, 100).unwrap(), b"89");
        assert!(FileReader::open(&dir).is_err(), "a directory is not a media file");
        assert_eq!(FileReader::open(&dir.join("missing")).unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A reader that counts the bytes read from it.
    struct Counting(MemReader, std::sync::atomic::AtomicU64);
    impl ByteReader for Counting {
        fn len(&self) -> u64 {
            self.0.len()
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            self.1.fetch_add(buf.len() as u64, std::sync::atomic::Ordering::SeqCst);
            self.0.read_at(offset, buf)
        }
    }

    /// #157: looking at a file no streaming reader takes (an AVI, a large WAV) read all of it.
    /// Within a limit it reads the head only and refuses; a small file still opens.
    #[test]
    fn open_within_reads_only_the_head_of_large_unstreamable_files() {
        let mut wav = crate::wav::write_wav16(&vec![0.25; 48_000 * 2], 2, 48_000);
        let small = wav.len() as u64;
        let r = Arc::new(Counting(MemReader(Arc::from(wav.clone())), Default::default()));
        let src = open_reader_within("a.wav", r.clone(), &[], &[], small).unwrap();
        assert_eq!(src.info().audio().unwrap().channels, 2);
        // the same file over the limit: refused after reading the sniffing head only
        let r = Arc::new(Counting(MemReader(Arc::from(wav.clone())), Default::default()));
        let e = open_reader_within("a.wav", r.clone(), &[], &[], small - 1).err().unwrap();
        assert!(matches!(e, MediaError::Unsupported(_)), "{e:?}");
        assert!(r.1.load(std::sync::atomic::Ordering::SeqCst) <= HEAD_LEN as u64);
        // an unsupported format is refused without being read whole either
        wav.resize(4 * HEAD_LEN, 0);
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"AVI ");
        let r = Arc::new(Counting(MemReader(Arc::from(wav)), Default::default()));
        assert!(open_reader_within("a.avi", r.clone(), &[], &[], HEAD_LEN as u64).is_err());
        assert!(r.1.load(std::sync::atomic::Ordering::SeqCst) <= HEAD_LEN as u64);
    }

    #[test]
    fn falls_back_to_byte_openers() {
        // a WAV is opened through the whole-file path
        let wav = crate::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        let r: SharedReader = Arc::new(MemReader(wav.into()));
        let s = open_reader("a.wav", r, &[], &[]).unwrap();
        assert!(s.info().has_audio());
    }
}
