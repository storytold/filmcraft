//! Reading compound files ([MS-CFB] §2.2–2.6).

use crate::{ENDOFCHAIN, Error, FREESECT, MAXREGSECT, MINI_SECTOR_SIZE, NOSTREAM, Result, SIGNATURE, compare_names};

/// Directory entry object type (§2.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Root,
    Storage,
    Stream,
    /// Unallocated (type 0) or an unknown type.
    Unknown,
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Class id of a storage (AAF: the class of the object stored in it).
    pub clsid: [u8; 16],
    pub state_bits: u32,
    /// FILETIME stamps (0 when not set).
    pub created: u64,
    pub modified: u64,
    /// First sector (regular or mini) of a stream; the mini stream's for the root.
    pub start: u32,
    pub size: u64,
    /// Child entries of a storage, in sibling-tree order (sorted by [`compare_names`]).
    pub children: Vec<usize>,
    left: u32,
    right: u32,
    child: u32,
}

/// A parsed compound file over borrowed bytes.
#[derive(Clone, Debug)]
pub struct CompoundFile<'a> {
    data: &'a [u8],
    /// Major version: 3 (512-byte sectors) or 4 (4096-byte sectors).
    pub version: u16,
    /// Header CLSID (normally zero).
    pub header_clsid: [u8; 16],
    sector_size: usize,
    mini_cutoff: u64,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    mini_stream: Vec<u8>,
    pub entries: Vec<Entry>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8]))
}

fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}

impl<'a> CompoundFile<'a> {
    /// Parse the header, FAT, mini FAT, directory and mini stream of `data`.
    pub fn open(data: &'a [u8]) -> Result<CompoundFile<'a>> {
        if data.len() < 512 || data[..8] != SIGNATURE || u16_at(data, 28) != 0xFFFE {
            return Err(Error::NotCfb);
        }
        let version = u16_at(data, 26);
        let shift = u16_at(data, 30);
        if !(7..=16).contains(&shift) {
            return Err(invalid(format!("sector shift {shift}")));
        }
        let mini_shift = u16_at(data, 32);
        if 1usize.checked_shl(mini_shift as u32) != Some(MINI_SECTOR_SIZE) {
            return Err(invalid(format!("mini sector shift {mini_shift}")));
        }
        let sector_size = 1usize << shift;
        let n_fat = u32_at(data, 44) as usize;
        let first_dir = u32_at(data, 48);
        let mini_cutoff = u32_at(data, 56) as u64;
        let first_minifat = u32_at(data, 60);
        let first_difat = u32_at(data, 68);
        let mut header_clsid = [0u8; 16];
        header_clsid.copy_from_slice(&data[8..24]);
        let max_sectors = data.len() / sector_size + 1;
        if n_fat > max_sectors {
            return Err(invalid(format!("{n_fat} FAT sectors in a {}-byte file", data.len())));
        }
        let mut cf = CompoundFile {
            data,
            version,
            header_clsid,
            sector_size,
            mini_cutoff,
            fat: Vec::new(),
            minifat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
        };
        // FAT sector numbers: 109 in the header, then the DIFAT chain.
        let mut fat_sectors: Vec<u32> = (0..109).map(|i| u32_at(data, 76 + i * 4)).filter(|&s| s <= MAXREGSECT).collect();
        let mut d = first_difat;
        let mut seen = 0;
        while d <= MAXREGSECT && fat_sectors.len() < n_fat {
            seen += 1;
            // a DIFAT chain cannot have more sectors than the file holds; the header's DIFAT sector
            // count (up to 2^32 - 1) is not trusted, or a self-pointing sector loops that often
            if seen > max_sectors {
                return Err(invalid(format!("DIFAT chain loops: more than {max_sectors} sectors in a {}-byte file", data.len())));
            }
            let s = cf.sector(d)?;
            let per = sector_size / 4 - 1;
            for i in 0..per {
                let v = u32_at(s, i * 4);
                if v <= MAXREGSECT {
                    fat_sectors.push(v);
                }
            }
            d = u32_at(s, per * 4);
        }
        fat_sectors.truncate(n_fat);
        let mut fat = Vec::with_capacity(fat_sectors.len() * sector_size / 4);
        for s in fat_sectors {
            let b = cf.sector(s)?;
            fat.extend((0..sector_size / 4).map(|i| u32_at(b, i * 4)));
        }
        cf.fat = fat;
        // Directory.
        let dir = cf.read_chain(first_dir, None)?;
        let n_entries = dir.len() / 128;
        if n_entries == 0 {
            return Err(invalid("empty directory"));
        }
        for i in 0..n_entries {
            cf.entries.push(parse_entry(&dir[i * 128..(i + 1) * 128], version));
        }
        if cf.entries[0].kind != EntryKind::Root {
            return Err(invalid("first directory entry is not the root"));
        }
        // Mini FAT and mini stream.
        if first_minifat <= MAXREGSECT {
            let mf = cf.read_chain(first_minifat, None)?;
            cf.minifat = mf.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        }
        let root = &cf.entries[0];
        if root.start <= MAXREGSECT && root.size > 0 {
            let size = root.size.min(data.len() as u64) as usize;
            cf.mini_stream = cf.read_chain(root.start, Some(size))?;
        }
        cf.build_tree()?;
        Ok(cf)
    }

    /// Sector `n` (a whole sector; a truncated last sector is an error).
    fn sector(&self, n: u32) -> Result<&'a [u8]> {
        let start = (n as usize + 1).checked_mul(self.sector_size).ok_or_else(|| invalid("sector number overflow"))?;
        let end = start.checked_add(self.sector_size).ok_or_else(|| invalid("sector number overflow"))?;
        if n > MAXREGSECT || end > self.data.len() {
            return Err(invalid(format!("sector {n} is outside the file")));
        }
        Ok(&self.data[start..end])
    }

    /// Concatenate a regular sector chain; `limit` stops once that many bytes are read.
    fn read_chain(&self, start: u32, limit: Option<usize>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut s = start;
        let mut steps = 0usize;
        // A chain cannot visit more sectors than the file holds. Not `self.fat.len()`: the FAT can
        // be far longer (one FAT sector named by many header / DIFAT slots), and a looping chain
        // would then append a whole sector per FAT entry (hundreds of MB, or an abort).
        let max_steps = self.data.len() / self.sector_size + 1;
        while s != ENDOFCHAIN {
            if s > MAXREGSECT {
                return Err(invalid(format!("bad sector {s:#x} in a chain")));
            }
            steps += 1;
            if steps > max_steps {
                return Err(invalid(format!("sector chain loops: more than {max_steps} sectors in a {}-byte file", self.data.len())));
            }
            let b = self.sector(s)?;
            out.extend_from_slice(b);
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
            s = *self.fat.get(s as usize).ok_or_else(|| invalid(format!("sector {s} has no FAT entry")))?;
            if s == FREESECT {
                return Err(invalid("chain runs into a free sector"));
            }
        }
        if let Some(l) = limit {
            if out.len() < l {
                return Err(invalid("stream is truncated"));
            }
            out.truncate(l);
        }
        Ok(out)
    }

    fn read_mini_chain(&self, start: u32, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size);
        let mut s = start;
        let mut steps = 0usize;
        while out.len() < size {
            if s > MAXREGSECT {
                return Err(invalid("mini stream chain ends early"));
            }
            steps += 1;
            if steps > self.minifat.len().max(1) {
                return Err(invalid("mini sector chain loops"));
            }
            let at = s as usize * MINI_SECTOR_SIZE;
            let b = self.mini_stream.get(at..at + MINI_SECTOR_SIZE).ok_or_else(|| invalid("mini sector outside the mini stream"))?;
            out.extend_from_slice(b);
            s = *self.minifat.get(s as usize).ok_or_else(|| invalid("mini sector has no mini FAT entry"))?;
        }
        out.truncate(size);
        Ok(out)
    }

    fn build_tree(&mut self) -> Result<()> {
        let n = self.entries.len();
        let mut owner = vec![false; n];
        owner[0] = true;
        for parent in 0..n {
            if !matches!(self.entries[parent].kind, EntryKind::Root | EntryKind::Storage) {
                continue;
            }
            let mut kids = Vec::new();
            // in-order walk of the sibling tree, iterative, cycle-safe
            let mut stack: Vec<u32> = Vec::new();
            let mut cur = self.entries[parent].child;
            let mut steps = 0;
            while cur != NOSTREAM || !stack.is_empty() {
                while cur != NOSTREAM {
                    let i = cur as usize;
                    if i >= n || owner[i] {
                        return Err(invalid(format!("directory entry {cur} is referenced twice or does not exist")));
                    }
                    owner[i] = true;
                    stack.push(cur);
                    cur = self.entries[i].left;
                    steps += 1;
                    if steps > n {
                        return Err(invalid("directory tree loops"));
                    }
                }
                if let Some(top) = stack.pop() {
                    kids.push(top as usize);
                    cur = self.entries[top as usize].right;
                }
            }
            self.entries[parent].children = kids;
        }
        Ok(())
    }

    pub fn root(&self) -> &Entry {
        &self.entries[0]
    }

    pub fn entry(&self, id: usize) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Child of storage `parent` named `name` (case-insensitive as in §2.6.4).
    pub fn child(&self, parent: usize, name: &str) -> Option<usize> {
        self.entries.get(parent)?.children.iter().copied().find(|&c| compare_names(&self.entries[c].name, name).is_eq())
    }

    /// Entry at a `/`-separated path below the root (`""` is the root).
    pub fn find(&self, path: &str) -> Result<usize> {
        let mut cur = 0;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            cur = self.child(cur, part).ok_or_else(|| Error::NotFound(path.to_string()))?;
        }
        Ok(cur)
    }

    /// Contents of stream `id`.
    pub fn read(&self, id: usize) -> Result<Vec<u8>> {
        let e = self.entries.get(id).ok_or_else(|| Error::NotFound(format!("entry {id}")))?;
        if e.kind != EntryKind::Stream {
            return Err(invalid(format!("{:?} is not a stream", e.name)));
        }
        let size = usize::try_from(e.size).map_err(|_| invalid("stream too large"))?;
        if size == 0 {
            return Ok(Vec::new());
        }
        if size as u64 > self.data.len() as u64 {
            return Err(invalid(format!("stream {:?} is larger than the file", e.name)));
        }
        if e.size < self.mini_cutoff { self.read_mini_chain(e.start, size) } else { self.read_chain(e.start, Some(size)) }
    }

    /// Contents of the stream at `path`.
    pub fn read_path(&self, path: &str) -> Result<Vec<u8>> {
        self.read(self.find(path)?)
    }

    /// Sector size in bytes (512 or 4096).
    pub fn sector_size(&self) -> usize {
        self.sector_size
    }
}

fn parse_entry(b: &[u8], version: u16) -> Entry {
    let name_len = (u16_at(b, 64) as usize).min(64);
    let units: Vec<u16> = (0..name_len / 2).map(|i| u16_at(b, i * 2)).take_while(|&u| u != 0).collect();
    let kind = match b[66] {
        5 => EntryKind::Root,
        1 => EntryKind::Storage,
        2 => EntryKind::Stream,
        _ => EntryKind::Unknown,
    };
    let mut clsid = [0u8; 16];
    clsid.copy_from_slice(&b[80..96]);
    let size = u64_at(b, 120);
    Entry {
        name: String::from_utf16_lossy(&units),
        kind,
        clsid,
        state_bits: u32_at(b, 96),
        created: u64_at(b, 100),
        modified: u64_at(b, 108),
        start: u32_at(b, 116),
        // version 3 files may leave garbage in the high 32 bits (§2.6.3)
        size: if version == 3 { size & 0xFFFF_FFFF } else { size },
        children: Vec::new(),
        left: u32_at(b, 68),
        right: u32_at(b, 72),
        child: u32_at(b, 76),
    }
}
