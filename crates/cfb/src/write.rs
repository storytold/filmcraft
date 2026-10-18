//! Writing compound files ([MS-CFB] §2.2–2.6).
//!
//! Layout written by [`Writer::finish`] (sector numbers ascending): the sectors of every stream of
//! 4096 bytes or more, the mini stream, the mini FAT, the directory, the FAT, then DIFAT sectors
//! when the FAT needs more than the 109 header slots. Every chain is contiguous. Sibling trees are
//! balanced binary search trees coloured as valid red-black trees (§2.6.4).

use crate::{DIFSECT, ENDOFCHAIN, FATSECT, FREESECT, MINI_SECTOR_SIZE, MINI_STREAM_CUTOFF, NOSTREAM, Result, SIGNATURE, compare_names, validate_name};

/// File format version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Version {
    /// Version 3: 512-byte sectors.
    V3,
    /// Version 4: 4096-byte sectors (AAF's "4K" files).
    #[default]
    V4,
}

impl Version {
    fn sector_size(self) -> usize {
        match self {
            Version::V3 => 512,
            Version::V4 => 4096,
        }
    }
}

/// A storage or stream added to a [`Writer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Root,
    Storage,
    Stream,
}

#[derive(Clone, Debug)]
struct Node {
    name: String,
    kind: Kind,
    clsid: [u8; 16],
    data: Vec<u8>,
    children: Vec<usize>,
}

/// Builds a compound file in memory.
#[derive(Clone, Debug)]
pub struct Writer {
    version: Version,
    nodes: Vec<Node>,
}

impl Default for Writer {
    fn default() -> Self {
        Writer::new(Version::default())
    }
}

impl Writer {
    /// The root storage.
    pub const ROOT: NodeId = NodeId(0);

    pub fn new(version: Version) -> Self {
        Writer { version, nodes: vec![Node { name: "Root Entry".into(), kind: Kind::Root, clsid: [0; 16], data: Vec::new(), children: Vec::new() }] }
    }

    fn add(&mut self, parent: NodeId, name: &str, kind: Kind, clsid: [u8; 16], data: Vec<u8>) -> Result<NodeId> {
        validate_name(name)?;
        let p = self.nodes.get(parent.0).ok_or_else(|| crate::Error::NotFound(format!("node {}", parent.0)))?;
        if p.kind == Kind::Stream {
            return Err(crate::Error::Invalid(format!("{:?} is a stream, not a storage", p.name)));
        }
        if p.children.iter().any(|&c| compare_names(&self.nodes[c].name, name).is_eq()) {
            return Err(crate::Error::BadName(format!("duplicate name {name}")));
        }
        let id = self.nodes.len();
        self.nodes.push(Node { name: name.to_string(), kind, clsid, data, children: Vec::new() });
        self.nodes[parent.0].children.push(id);
        Ok(NodeId(id))
    }

    /// Add a storage below `parent`.
    pub fn storage(&mut self, parent: NodeId, name: &str, clsid: [u8; 16]) -> Result<NodeId> {
        self.add(parent, name, Kind::Storage, clsid, Vec::new())
    }

    /// Add a stream below `parent`.
    pub fn stream(&mut self, parent: NodeId, name: &str, data: Vec<u8>) -> Result<NodeId> {
        self.add(parent, name, Kind::Stream, [0; 16], data)
    }

    /// Set the class id of a storage (or the root).
    pub fn set_clsid(&mut self, node: NodeId, clsid: [u8; 16]) {
        if let Some(n) = self.nodes.get_mut(node.0) {
            n.clsid = clsid;
        }
    }

    /// Lay the file out and return its bytes.
    pub fn finish(self) -> Vec<u8> {
        let ss = self.version.sector_size();
        let per_sector = ss / 4;
        // Directory order: depth-first pre-order from the root.
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![0usize];
        while let Some(n) = stack.pop() {
            order.push(n);
            for &c in self.nodes[n].children.iter().rev() {
                stack.push(c);
            }
        }
        let mut dir_index = vec![0u32; self.nodes.len()];
        for (i, &n) in order.iter().enumerate() {
            dir_index[n] = i as u32;
        }

        // Stream placement.
        let mut start = vec![ENDOFCHAIN; self.nodes.len()];
        let mut next_sector = 0u32;
        let mut big_chains: Vec<(u32, u32)> = Vec::new(); // (first, count)
        for &n in &order {
            let node = &self.nodes[n];
            if node.kind == Kind::Stream && node.data.len() as u64 >= MINI_STREAM_CUTOFF as u64 {
                let count = node.data.len().div_ceil(ss) as u32;
                start[n] = next_sector;
                big_chains.push((next_sector, count));
                next_sector += count;
            }
        }
        let mut next_mini = 0u32;
        let mut mini_chains: Vec<(u32, u32)> = Vec::new();
        for &n in &order {
            let node = &self.nodes[n];
            if node.kind == Kind::Stream && !node.data.is_empty() && (node.data.len() as u64) < MINI_STREAM_CUTOFF as u64 {
                let count = node.data.len().div_ceil(MINI_SECTOR_SIZE) as u32;
                start[n] = next_mini;
                mini_chains.push((next_mini, count));
                next_mini += count;
            }
        }
        let mini_bytes = next_mini as usize * MINI_SECTOR_SIZE;
        let mini_stream_sectors = mini_bytes.div_ceil(ss) as u32;
        let mini_stream_start = if mini_stream_sectors > 0 { next_sector } else { ENDOFCHAIN };
        next_sector += mini_stream_sectors;
        let minifat_sectors = (next_mini as usize * 4).div_ceil(ss) as u32;
        let minifat_start = if minifat_sectors > 0 { next_sector } else { ENDOFCHAIN };
        next_sector += minifat_sectors;
        let dir_sectors = (order.len() * 128).div_ceil(ss) as u32;
        let dir_start = next_sector;
        next_sector += dir_sectors;
        let base = next_sector as usize;
        let (mut fat_n, mut difat_n) = (1usize, 0usize);
        loop {
            let total = base + fat_n + difat_n;
            let need_fat = total.div_ceil(per_sector);
            let need_difat = if need_fat > 109 { (need_fat - 109).div_ceil(per_sector - 1) } else { 0 };
            if need_fat == fat_n && need_difat == difat_n {
                break;
            }
            fat_n = need_fat.max(fat_n);
            difat_n = need_difat.max(difat_n);
        }
        let fat_start = next_sector;
        let difat_start = fat_start + fat_n as u32;
        let total = base + fat_n + difat_n;

        // FAT.
        let mut fat = vec![FREESECT; fat_n * per_sector];
        let chain = |first: u32, count: u32, fat: &mut [u32]| {
            for i in 0..count {
                fat[(first + i) as usize] = if i + 1 == count { ENDOFCHAIN } else { first + i + 1 };
            }
        };
        for &(f, c) in &big_chains {
            chain(f, c, &mut fat);
        }
        if mini_stream_sectors > 0 {
            chain(mini_stream_start, mini_stream_sectors, &mut fat);
        }
        if minifat_sectors > 0 {
            chain(minifat_start, minifat_sectors, &mut fat);
        }
        chain(dir_start, dir_sectors, &mut fat);
        for i in 0..fat_n {
            fat[fat_start as usize + i] = FATSECT;
        }
        for i in 0..difat_n {
            fat[difat_start as usize + i] = DIFSECT;
        }
        debug_assert!(total <= fat.len());

        // Mini FAT.
        let mut minifat = vec![FREESECT; minifat_sectors as usize * per_sector];
        for &(f, c) in &mini_chains {
            for i in 0..c {
                minifat[(f + i) as usize] = if i + 1 == c { ENDOFCHAIN } else { f + i + 1 };
            }
        }

        // Directory.
        let mut tree = vec![(NOSTREAM, NOSTREAM, NOSTREAM, 1u8); self.nodes.len()]; // left, right, child, colour
        for (p, node) in self.nodes.iter().enumerate() {
            if node.kind == Kind::Stream || node.children.is_empty() {
                continue;
            }
            let mut kids = node.children.clone();
            kids.sort_by(|&a, &b| compare_names(&self.nodes[a].name, &self.nodes[b].name));
            let height = usize::BITS - (kids.len() + 1).leading_zeros() - 1; // floor(log2(n + 1))
            let perfect = (1usize << height) - 1 == kids.len();
            let root = build_tree(&kids, 0, perfect, height as usize, &mut tree, &dir_index);
            tree[p].2 = root;
        }
        let mut dir = Vec::with_capacity(dir_sectors as usize * ss);
        for &n in &order {
            let node = &self.nodes[n];
            let mut e = [0u8; 128];
            let units: Vec<u16> = node.name.encode_utf16().collect();
            for (i, u) in units.iter().enumerate() {
                e[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            e[64..66].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
            e[66] = match node.kind {
                Kind::Root => 5,
                Kind::Storage => 1,
                Kind::Stream => 2,
            };
            let (l, r, c, colour) = tree[n];
            e[67] = if node.kind == Kind::Root { 1 } else { colour };
            e[68..72].copy_from_slice(&l.to_le_bytes());
            e[72..76].copy_from_slice(&r.to_le_bytes());
            e[76..80].copy_from_slice(&c.to_le_bytes());
            e[80..96].copy_from_slice(&node.clsid);
            let (st, size) = match node.kind {
                Kind::Root => (mini_stream_start, mini_bytes as u64),
                Kind::Storage => (0, 0),
                Kind::Stream => (if node.data.is_empty() { ENDOFCHAIN } else { start[n] }, node.data.len() as u64),
            };
            e[116..120].copy_from_slice(&st.to_le_bytes());
            e[120..128].copy_from_slice(&size.to_le_bytes());
            dir.extend_from_slice(&e);
        }
        while dir.len() < dir_sectors as usize * ss {
            let mut e = [0u8; 128];
            e[68..80].copy_from_slice(&[0xFF; 12]);
            dir.extend_from_slice(&e);
        }

        // Header.
        let mut out = Vec::with_capacity((total + 1) * ss);
        out.extend_from_slice(&SIGNATURE);
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&0x003Eu16.to_le_bytes());
        out.extend_from_slice(&(if self.version == Version::V3 { 3u16 } else { 4u16 }).to_le_bytes());
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&(if self.version == Version::V3 { 9u16 } else { 12u16 }).to_le_bytes());
        out.extend_from_slice(&6u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&(if self.version == Version::V3 { 0 } else { dir_sectors }).to_le_bytes());
        out.extend_from_slice(&(fat_n as u32).to_le_bytes());
        out.extend_from_slice(&dir_start.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&MINI_STREAM_CUTOFF.to_le_bytes());
        out.extend_from_slice(&minifat_start.to_le_bytes());
        out.extend_from_slice(&minifat_sectors.to_le_bytes());
        out.extend_from_slice(&(if difat_n > 0 { difat_start } else { ENDOFCHAIN }).to_le_bytes());
        out.extend_from_slice(&(difat_n as u32).to_le_bytes());
        for i in 0..109 {
            let v = if i < fat_n { fat_start + i as u32 } else { FREESECT };
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.resize(ss, 0);

        // Sectors.
        let pad = |out: &mut Vec<u8>| {
            let r = out.len() % ss;
            if r != 0 {
                out.resize(out.len() + ss - r, 0);
            }
        };
        for &n in &order {
            let node = &self.nodes[n];
            if node.kind == Kind::Stream && node.data.len() as u64 >= MINI_STREAM_CUTOFF as u64 {
                out.extend_from_slice(&node.data);
                pad(&mut out);
            }
        }
        for &n in &order {
            let node = &self.nodes[n];
            if node.kind == Kind::Stream && !node.data.is_empty() && (node.data.len() as u64) < MINI_STREAM_CUTOFF as u64 {
                out.extend_from_slice(&node.data);
                let r = node.data.len() % MINI_SECTOR_SIZE;
                if r != 0 {
                    out.resize(out.len() + MINI_SECTOR_SIZE - r, 0);
                }
            }
        }
        pad(&mut out);
        for v in &minifat {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&dir);
        for v in &fat {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let mut remaining: Vec<u32> = (109..fat_n).map(|i| fat_start + i as u32).collect();
        for i in 0..difat_n {
            let mut s = Vec::with_capacity(ss);
            let take = remaining.len().min(per_sector - 1);
            for v in remaining.drain(..take) {
                s.extend_from_slice(&v.to_le_bytes());
            }
            while s.len() < ss - 4 {
                s.extend_from_slice(&FREESECT.to_le_bytes());
            }
            let next = if i + 1 < difat_n { difat_start + i as u32 + 1 } else { ENDOFCHAIN };
            s.extend_from_slice(&next.to_le_bytes());
            out.extend_from_slice(&s);
        }
        debug_assert_eq!(out.len(), (total + 1) * ss);
        out
    }
}

/// Balanced BST of `kids[..]` (sorted). Nodes on the bottom level of an imperfect tree are red,
/// so every root-to-leaf path has the same number of black nodes.
fn build_tree(kids: &[usize], depth: usize, perfect: bool, height: usize, tree: &mut [(u32, u32, u32, u8)], dir_index: &[u32]) -> u32 {
    if kids.is_empty() {
        return NOSTREAM;
    }
    let mid = kids.len() / 2;
    let n = kids[mid];
    let left = build_tree(&kids[..mid], depth + 1, perfect, height, tree, dir_index);
    let right = build_tree(&kids[mid + 1..], depth + 1, perfect, height, tree, dir_index);
    tree[n].0 = left;
    tree[n].1 = right;
    tree[n].3 = if !perfect && depth == height { 0 } else { 1 };
    dir_index[n]
}
