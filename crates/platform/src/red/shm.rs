//! The helper's frame slots, read in place (FCRD6). The one FFI module of this backend
//! (docs/adr/0001-platform-ffi.md): a read-only view of the memory the helper creates, through
//! the official `windows` bindings (a named, pagefile-backed section) or `libc` (a POSIX shared
//! memory object under `/dev/shm`). Everything else in `red` is safe code; the view is handed
//! out as byte slices with their bounds checked here, and both arms present the same API.

use filmcraft_media::{MediaError, Result};

fn failure(message: impl Into<String>) -> MediaError {
    MediaError::Decode(message.into())
}

/// The name the app chose for the helper (letters, digits and dashes) and a non-empty length.
fn valid(name: &str, len: usize) -> Result<()> {
    if len == 0 || name.is_empty() || name.len() > 128 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(failure("invalid RED shared frame memory"));
    }
    Ok(())
}

/// The helper's frame memory name while the app does not yet hold the memory: held across the
/// worker's start-up and dropped on every path out of it, it removes the name on Linux, where
/// a helper killed before the app mapped its object would otherwise leave an empty object under
/// `/dev/shm` (a name already gone, the usual case, is fine). A Windows section dies with its
/// handles, so there is nothing to remove.
pub(super) struct Name(String);
impl Name {
    pub(super) fn new(name: &str) -> Self {
        Self(name.to_owned())
    }
}
impl Drop for Name {
    fn drop(&mut self) {
        remove(&self.0);
    }
}

#[cfg(target_os = "linux")]
pub(super) use posix_object::*;
#[cfg(windows)]
pub(super) use windows_section::*;

#[cfg(windows)]
mod windows_section {
    use super::{failure, valid};
    use filmcraft_media::Result;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Memory::{FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile};
    use windows::core::PCWSTR;

    fn wide(name: &str) -> Vec<u16> {
        format!("Local\\{name}-frames").encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Nothing to do: a section lives exactly as long as a handle or view of it does.
    pub(in super::super) fn remove(_: &str) {}

    /// A read-only view of `len` bytes of a named section another process created.
    pub(in super::super) struct Mapping {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        len: usize,
    }

    // SAFETY: the view is plain memory owned by this process for the life of the value; the handle
    // and base address are used only through `&self` methods that read, and the section is never
    // written through this view (it is mapped read-only), so sharing it between threads is sound.
    unsafe impl Send for Mapping {}
    // SAFETY: as above; `slice` only reads, and the drop runs once.
    unsafe impl Sync for Mapping {}

    impl Mapping {
        /// Open the section `Local\<name>-frames` for reading and map its first `len` bytes.
        /// Fails if the section does not exist or is smaller than `len` (the view then covers
        /// memory that the section guarantees to be readable, whatever the other process does
        /// later).
        pub(in super::super) fn open(name: &str, len: usize) -> Result<Self> {
            valid(name, len)?;
            let wide = wide(name);
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; the handle
            // returned is owned by this value and closed in `drop`.
            let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide.as_ptr())) }
                .map_err(|e| failure(format!("RED shared frame memory unavailable: {e}")))?;
            // SAFETY: `handle` is an open file-mapping handle with read access; a view of `len`
            // bytes from offset 0 is requested and the call fails (null) rather than mapping less
            // when the section is smaller than `len`, so a non-null view is `len` readable bytes
            // until `UnmapViewOfFile`.
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, len) };
            if view.Value.is_null() {
                let error = windows::core::Error::from_thread();
                // SAFETY: `handle` is open and owned here; nothing else refers to it.
                let _ = unsafe { CloseHandle(handle) };
                return Err(failure(format!("RED shared frame memory could not be mapped: {error}")));
            }
            Ok(Self { handle, view, len })
        }
        /// Bytes `offset..offset + len` of the view.
        ///
        /// The helper writes a slot only while it decodes into it and hands the slot over before
        /// its reply, and the app hands it back (`F <slot>`) only once it has finished reading; a
        /// helper that exits leaves the pages as they are (a pagefile-backed section lives while
        /// this view does). So within the protocol the bytes do not change under the slice. A
        /// helper that broke the protocol could change them, as any process can change a file
        /// another has mapped; the readers tolerate any byte values (they are validated, never
        /// trusted), and no Rust invariant beyond the slice's own contents rests on them.
        pub(in super::super) fn slice(&self, offset: usize, len: usize) -> Result<&[u8]> {
            let end = offset.checked_add(len).ok_or_else(|| failure("RED frame slot out of range"))?;
            if end > self.len {
                return Err(failure("RED frame slot out of range"));
            }
            if len == 0 {
                return Ok(&[]);
            }
            // SAFETY: `offset + len <= self.len`, and the view is `self.len` readable bytes (see
            // `open`) for as long as `self` lives, which the returned lifetime does not exceed; the
            // memory is initialized (a section's pages read as zero until written) and `u8` has no
            // invalid values. Concurrent modification by the other process is discussed above.
            Ok(unsafe { std::slice::from_raw_parts((self.view.Value as *const u8).add(offset), len) })
        }
        #[cfg(test)]
        pub(in super::super) fn len(&self) -> usize {
            self.len
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: `view` came from a successful `MapViewOfFile` and `handle` from
            // `OpenFileMappingW`, both owned by this value and released exactly once here; no
            // slice borrowed from `self` can outlive it.
            unsafe {
                let _ = UnmapViewOfFile(self.view);
                let _ = CloseHandle(self.handle);
            }
        }
    }

    /// A section created in this process, for the tests: the helper's side of the protocol.
    #[cfg(test)]
    pub(in super::super) struct TestSection {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        len: usize,
    }
    #[cfg(test)]
    impl TestSection {
        pub(in super::super) fn create(name: &str, len: usize) -> Result<Self> {
            use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, PAGE_READWRITE};
            let wide = wide(name);
            // SAFETY: a pagefile-backed section (no file handle) of `len` bytes with a NUL-terminated
            // name that outlives the call; the handle is owned by the returned value.
            let handle =
                unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, (len as u64 >> 32) as u32, len as u32, PCWSTR(wide.as_ptr())) }
                    .map_err(|e| failure(format!("test section: {e}")))?;
            // SAFETY: `handle` is an open read-write mapping of at least `len` bytes.
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
            if view.Value.is_null() {
                // SAFETY: owned, open handle.
                let _ = unsafe { CloseHandle(handle) };
                return Err(failure("test section could not be mapped"));
            }
            Ok(Self { handle, view, len })
        }
        pub(in super::super) fn write(&self, offset: usize, bytes: &[u8]) -> Result<()> {
            let end = offset.checked_add(bytes.len()).filter(|end| *end <= self.len).ok_or_else(|| failure("test section write out of range"))?;
            let _ = end;
            // SAFETY: `offset + bytes.len() <= self.len` within a writable view this value owns, and
            // `bytes` cannot overlap the view (it is a Rust slice of this process's own memory).
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (self.view.Value as *mut u8).add(offset), bytes.len()) };
            Ok(())
        }
    }
    #[cfg(test)]
    impl Drop for TestSection {
        fn drop(&mut self) {
            // SAFETY: as for `Mapping`.
            unsafe {
                let _ = UnmapViewOfFile(self.view);
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Linux: the helper creates the POSIX shared memory object `/<name>-frames` (a file on the
/// tmpfs at `/dev/shm`) exclusively, sizes it once with `ftruncate` and never resizes it; the
/// app opens it read-only, checks its size against the geometry the helper declared, maps it
/// shared and then removes the name (the mapping keeps the object alive, as a view keeps a
/// Windows section alive, so a helper that dies mid-frame leaves the app reading valid memory).
/// A process of the same user could still shrink the object under the mapping (reading past
/// the new end would then fault), as it could ptrace the app; nothing short of that can.
#[cfg(target_os = "linux")]
mod posix_object {
    use super::{failure, valid};
    use filmcraft_media::Result;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// The object's POSIX name, `/<name>-frames`.
    fn object(name: &str) -> Result<CString> {
        CString::new(format!("/{name}-frames")).map_err(|_| failure("invalid RED shared frame memory"))
    }

    /// Remove the object's name, if it has one (its memory lives on in every mapping of it).
    pub(in super::super) fn remove(name: &str) {
        if valid(name, 1).is_err() {
            return;
        }
        if let Ok(object) = object(name) {
            // SAFETY: `object` is a NUL-terminated string that outlives the call; the call
            // touches no memory of this process and its result (a name already gone) is ignored.
            let _ = unsafe { libc::shm_unlink(object.as_ptr()) };
        }
    }

    /// A read-only mapping of `len` bytes of a shared memory object another process created.
    pub(in super::super) struct Mapping {
        base: *const u8,
        len: usize,
    }

    // SAFETY: the mapping is plain memory owned by this process for the life of the value; the
    // base address is used only through `&self` methods that read, and the object is never
    // written through this mapping (it is mapped read-only), so sharing it between threads is sound.
    unsafe impl Send for Mapping {}
    // SAFETY: as above; `slice` only reads, and the drop runs once.
    unsafe impl Sync for Mapping {}

    impl Mapping {
        /// Open `/<name>-frames` for reading and map its first `len` bytes. Fails if the object
        /// does not exist or is smaller than `len` (a mapping past the end of the object would
        /// fault when read), and removes the object's name once the mapping holds it.
        pub(in super::super) fn open(name: &str, len: usize) -> Result<Self> {
            valid(name, len)?;
            let object = object(name)?;
            // SAFETY: `object` is a NUL-terminated string that outlives the call; the descriptor
            // returned is owned by `file` below and closed with it.
            let fd = unsafe { libc::shm_open(object.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC, 0) };
            if fd < 0 {
                return Err(failure(format!("RED shared frame memory unavailable: {}", std::io::Error::last_os_error())));
            }
            // SAFETY: `fd` is an open descriptor that nothing else owns.
            let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            let size = file.metadata().map_err(|e| failure(format!("RED shared frame memory unavailable: {e}")))?.len();
            if size < len as u64 {
                return Err(failure("RED shared frame memory is smaller than the helper declared"));
            }
            // SAFETY: a read-only, shared mapping of `len` bytes from offset 0 of an open
            // descriptor whose object is at least `len` bytes (checked above); the address hint
            // is null, `MAP_FAILED` is checked, and the mapping outlives the descriptor.
            let base = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, file.as_raw_fd(), 0) };
            if base == libc::MAP_FAILED {
                return Err(failure(format!("RED shared frame memory could not be mapped: {}", std::io::Error::last_os_error())));
            }
            // The name is the app's to remove now that the mapping holds the object.
            // SAFETY: as in `remove`.
            let _ = unsafe { libc::shm_unlink(object.as_ptr()) };
            Ok(Self { base: base as *const u8, len })
        }
        /// Bytes `offset..offset + len` of the mapping.
        ///
        /// The helper writes a slot only while it decodes into it and hands the slot over before
        /// its reply, and the app hands it back (`F <slot>`) only once it has finished reading; a
        /// helper that exits leaves the pages as they are (the object lives while this mapping
        /// does). So within the protocol the bytes do not change under the slice. A helper that
        /// broke the protocol could change them, as any process can change a file another has
        /// mapped; the readers tolerate any byte values (they are validated, never trusted), and
        /// no Rust invariant beyond the slice's own contents rests on them.
        pub(in super::super) fn slice(&self, offset: usize, len: usize) -> Result<&[u8]> {
            let end = offset.checked_add(len).ok_or_else(|| failure("RED frame slot out of range"))?;
            if end > self.len {
                return Err(failure("RED frame slot out of range"));
            }
            if len == 0 {
                return Ok(&[]);
            }
            // SAFETY: `offset + len <= self.len`, and the mapping is `self.len` readable bytes
            // (see `open`) for as long as `self` lives, which the returned lifetime does not
            // exceed; the memory is initialized (a shared memory object's pages read as zero
            // until written) and `u8` has no invalid values. Concurrent modification by the
            // other process is discussed above.
            Ok(unsafe { std::slice::from_raw_parts(self.base.add(offset), len) })
        }
        #[cfg(test)]
        pub(in super::super) fn len(&self) -> usize {
            self.len
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: `base` and `len` came from a successful `mmap` owned by this value and are
            // unmapped exactly once here; no slice borrowed from `self` can outlive it.
            let _ = unsafe { libc::munmap(self.base as *mut libc::c_void, self.len) };
        }
    }

    /// An object created in this process, for the tests: the helper's side of the protocol.
    #[cfg(test)]
    pub(in super::super) struct TestSection {
        object: CString,
        base: *mut u8,
        len: usize,
    }
    #[cfg(test)]
    impl TestSection {
        pub(in super::super) fn create(name: &str, len: usize) -> Result<Self> {
            let object = object(name)?;
            // SAFETY: `object` is NUL-terminated and outlives the call; the descriptor returned
            // is owned by `file` below.
            let fd = unsafe { libc::shm_open(object.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC, 0o600) };
            if fd < 0 {
                return Err(failure(format!("test section: {}", std::io::Error::last_os_error())));
            }
            // SAFETY: `fd` is an open descriptor that nothing else owns.
            let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            if let Err(e) = file.set_len(len as u64) {
                super::remove(name);
                return Err(failure(format!("test section: {e}")));
            }
            // SAFETY: a read-write shared mapping of `len` bytes of an object just sized to `len`.
            let base = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0) };
            if base == libc::MAP_FAILED {
                super::remove(name);
                return Err(failure("test section could not be mapped"));
            }
            Ok(Self { object, base: base as *mut u8, len })
        }
        pub(in super::super) fn write(&self, offset: usize, bytes: &[u8]) -> Result<()> {
            let end = offset.checked_add(bytes.len()).filter(|end| *end <= self.len).ok_or_else(|| failure("test section write out of range"))?;
            let _ = end;
            // SAFETY: `offset + bytes.len() <= self.len` within a writable mapping this value owns,
            // and `bytes` cannot overlap it (it is a Rust slice of this process's own memory).
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len()) };
            Ok(())
        }
    }
    #[cfg(test)]
    impl Drop for TestSection {
        fn drop(&mut self) {
            // SAFETY: as for `Mapping`; the name may already be gone (the app removes it).
            unsafe {
                let _ = libc::munmap(self.base as *mut libc::c_void, self.len);
                let _ = libc::shm_unlink(self.object.as_ptr());
            }
        }
    }
}
