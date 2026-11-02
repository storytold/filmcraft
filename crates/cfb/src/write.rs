//! Writing compound files ([MS-CFB] §2.2–2.6).
//!
//! Layout written by [`Writer::finish`] (sector numbers ascending): the sectors of every stream of
//! 4096 bytes or more, the mini stream, the mini FAT, the directory, the FAT, then DIFAT sectors
//! when the FAT needs more than the 109 header slots. Every chain is contiguous. Sibling trees are
//! balanced binary search trees coloured as valid red-black trees (§2.6.4).

use std::collections::HashSet;

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
    /// The children's names, upper-cased as sibling order compares them: an O(1) duplicate check.
    /// (Scanning every sibling with `compare_names`, which allocates, made adding n children O(n^2):
    /// 20 000 AAF markers took a minute.)
    child_keys: HashSet<Vec<u16>>,
}

/// A name as sibling comparison sees it: its UTF-16 code units, upper-cased.
fn name_key(name: &str) -> Vec<u16> {
    name.encode_utf16().map(crate::upper).collect()
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
        Writer {
            version,
            nodes: vec![Node {
                name: "Root Entry".into(),
                kind: Kind::Root,
                clsid: [0; 16],
                data: Vec::new(),
                children: Vec::new(),
                child_keys: HashSet::new(),
            }],
        }
    }

    fn add(&mut self, parent: NodeId, name: &str, kind: Kind, clsid: [u8; 16], data: Vec<u8>) -> Result<NodeId> {
        validate_name(name)?;
        let p = self.nodes.get(parent.0).ok_or_else(|| crate::Error::NotFound(format!("node {}", parent.0)))?;
        if p.kind == Kind::Stream {
            return Err(crate::Error::Invalid(format!("{:?} is a stream, not a storage", p.name)));
        }
        let key = name_key(name);
        if p.child_keys.contains(&key) {
            return Err(crate::Error::BadName(format!("duplicate name {name}")));
        }
        let id = self.nodes.len();
        self.nodes.push(Node { name: name.to_string(), kind, clsid, data, children: Vec::new(), child_keys: HashSet::new() });
        let p = &mut self.nodes[parent.0];
        p.children.push(id);
        p.child_keys.insert(key);
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
        let layout = self.layout();
        let fat = layout.fat(ss / 4);
        let minifat = layout.mini_fat(ss / 4);
        let dir = self.directory(&layout);
        let mut out = self.header(&layout);

        // Sectors.
        let pad = |out: &mut Vec<u8>| {
            let r = out.len() % ss;
            if r != 0 {
                out.resize(out.len() + ss - r, 0);
            }
        };
        for &n in &layout.order {
            let node = &self.nodes[n];
            if node.kind == Kind::Stream && node.data.len() as u64 >= MINI_STREAM_CUTOFF as u64 {
                out.extend_from_slice(&node.data);
                pad(&mut out);
            }
        }
        for &n in &layout.order {
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
        layout.write_difat(&mut out, ss);
        debug_assert_eq!(out.len(), (layout.total + 1) * ss);
        out
    }

    /// Directory order (depth-first pre-order from the root) and sector placement of every stream.
    fn layout(&self) -> Layout {
        let ss = self.version.sector_size();
        let per_sector = ss / 4;
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
        let (fat_n, difat_n) = fat_sector_counts(next_sector as usize, per_sector);
        let fat_start = next_sector;
        let difat_start = fat_start + fat_n as u32;
        Layout {
            order,
            dir_index,
            start,
            big_chains,
            mini_chains,
            mini_bytes,
            mini_stream_sectors,
            mini_stream_start,
            minifat_sectors,
            minifat_start,
            dir_sectors,
            dir_start,
            fat_n,
            difat_n,
            fat_start,
            difat_start,
            total: next_sector as usize + fat_n + difat_n,
        }
    }

    /// The directory sectors: one 128-byte entry per node in directory order, then unused entries.
    fn directory(&self, l: &Layout) -> Vec<u8> {
        let ss = self.version.sector_size();
        let mut tree = vec![(NOSTREAM, NOSTREAM, NOSTREAM, 1u8); self.nodes.len()]; // left, right, child, colour
        for (p, node) in self.nodes.iter().enumerate() {
            if node.kind == Kind::Stream || node.children.is_empty() {
                continue;
            }
            let mut kids = node.children.clone();
            kids.sort_by(|&a, &b| compare_names(&self.nodes[a].name, &self.nodes[b].name));
            let height = usize::BITS - (kids.len() + 1).leading_zeros() - 1; // floor(log2(n + 1))
            let perfect = (1usize << height) - 1 == kids.len();
            let root = build_tree(&kids, 0, perfect, height as usize, &mut tree, &l.dir_index);
            tree[p].2 = root;
        }
        let mut dir = Vec::with_capacity(l.dir_sectors as usize * ss);
        for &n in &l.order {
            dir.extend_from_slice(&self.dir_entry(l, n, tree[n]));
        }
        while dir.len() < l.dir_sectors as usize * ss {
            let mut e = [0u8; 128];
            e[68..80].copy_from_slice(&[0xFF; 12]);
            dir.extend_from_slice(&e);
        }
        dir
    }

    /// The directory entry of node `n`; `links` are its (left, right, child, colour).
    fn dir_entry(&self, l: &Layout, n: usize, links: (u32, u32, u32, u8)) -> [u8; 128] {
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
        let (left, right, child, colour) = links;
        e[67] = if node.kind == Kind::Root { 1 } else { colour };
        e[68..72].copy_from_slice(&left.to_le_bytes());
        e[72..76].copy_from_slice(&right.to_le_bytes());
        e[76..80].copy_from_slice(&child.to_le_bytes());
        e[80..96].copy_from_slice(&node.clsid);
        let (st, size) = match node.kind {
            Kind::Root => (l.mini_stream_start, l.mini_bytes as u64),
            Kind::Storage => (0, 0),
            Kind::Stream => (if node.data.is_empty() { ENDOFCHAIN } else { l.start[n] }, node.data.len() as u64),
        };
        e[116..120].copy_from_slice(&st.to_le_bytes());
        e[120..128].copy_from_slice(&size.to_le_bytes());
        e
    }

    /// The header sector.
    fn header(&self, l: &Layout) -> Vec<u8> {
        let ss = self.version.sector_size();
        let v3 = self.version == Version::V3;
        let mut out = Vec::with_capacity((l.total + 1) * ss);
        out.extend_from_slice(&SIGNATURE);
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&0x003Eu16.to_le_bytes());
        out.extend_from_slice(&(if v3 { 3u16 } else { 4u16 }).to_le_bytes());
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&(if v3 { 9u16 } else { 12u16 }).to_le_bytes());
        out.extend_from_slice(&6u16.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&(if v3 { 0 } else { l.dir_sectors }).to_le_bytes());
        out.extend_from_slice(&(l.fat_n as u32).to_le_bytes());
        out.extend_from_slice(&l.dir_start.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&MINI_STREAM_CUTOFF.to_le_bytes());
        out.extend_from_slice(&l.minifat_start.to_le_bytes());
        out.extend_from_slice(&l.minifat_sectors.to_le_bytes());
        out.extend_from_slice(&(if l.difat_n > 0 { l.difat_start } else { ENDOFCHAIN }).to_le_bytes());
        out.extend_from_slice(&(l.difat_n as u32).to_le_bytes());
        for i in 0..109 {
            let v = if i < l.fat_n { l.fat_start + i as u32 } else { FREESECT };
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.resize(ss, 0);
        out
    }
}

/// Where everything lands in the file: directory order, stream starts and the sector counts.
struct Layout {
    /// Node indices in directory order.
    order: Vec<usize>,
    /// Directory index of each node.
    dir_index: Vec<u32>,
    /// First (mini) sector of each stream.
    start: Vec<u32>,
    /// (first, count) of each stream held in regular sectors.
    big_chains: Vec<(u32, u32)>,
    /// (first, count) of each stream held in mini sectors.
    mini_chains: Vec<(u32, u32)>,
    mini_bytes: usize,
    mini_stream_sectors: u32,
    mini_stream_start: u32,
    minifat_sectors: u32,
    minifat_start: u32,
    dir_sectors: u32,
    dir_start: u32,
    fat_n: usize,
    difat_n: usize,
    fat_start: u32,
    difat_start: u32,
    /// Sectors after the header.
    total: usize,
}

/// FAT and DIFAT sector counts for `base` other sectors (the FAT must also map itself).
fn fat_sector_counts(base: usize, per_sector: usize) -> (usize, usize) {
    let (mut fat_n, mut difat_n) = (1usize, 0usize);
    loop {
        let total = base + fat_n + difat_n;
        let need_fat = total.div_ceil(per_sector);
        let need_difat = if need_fat > 109 { (need_fat - 109).div_ceil(per_sector - 1) } else { 0 };
        if need_fat == fat_n && need_difat == difat_n {
            return (fat_n, difat_n);
        }
        fat_n = need_fat.max(fat_n);
        difat_n = need_difat.max(difat_n);
    }
}

impl Layout {
    fn fat(&self, per_sector: usize) -> Vec<u32> {
        let mut fat = vec![FREESECT; self.fat_n * per_sector];
        let chain = |first: u32, count: u32, fat: &mut [u32]| {
            for i in 0..count {
                fat[(first + i) as usize] = if i + 1 == count { ENDOFCHAIN } else { first + i + 1 };
            }
        };
        for &(f, c) in &self.big_chains {
            chain(f, c, &mut fat);
        }
        if self.mini_stream_sectors > 0 {
            chain(self.mini_stream_start, self.mini_stream_sectors, &mut fat);
        }
        if self.minifat_sectors > 0 {
            chain(self.minifat_start, self.minifat_sectors, &mut fat);
        }
        chain(self.dir_start, self.dir_sectors, &mut fat);
        for i in 0..self.fat_n {
            fat[self.fat_start as usize + i] = FATSECT;
        }
        for i in 0..self.difat_n {
            fat[self.difat_start as usize + i] = DIFSECT;
        }
        debug_assert!(self.total <= fat.len());
        fat
    }

    fn mini_fat(&self, per_sector: usize) -> Vec<u32> {
        let mut minifat = vec![FREESECT; self.minifat_sectors as usize * per_sector];
        for &(f, c) in &self.mini_chains {
            for i in 0..c {
                minifat[(f + i) as usize] = if i + 1 == c { ENDOFCHAIN } else { f + i + 1 };
            }
        }
        minifat
    }

    /// Append the DIFAT sectors: the FAT sector numbers beyond the header's 109 slots.
    fn write_difat(&self, out: &mut Vec<u8>, ss: usize) {
        let per_sector = ss / 4;
        let mut remaining: Vec<u32> = (109..self.fat_n).map(|i| self.fat_start + i as u32).collect();
        for i in 0..self.difat_n {
            let mut s = Vec::with_capacity(ss);
            let take = remaining.len().min(per_sector - 1);
            for v in remaining.drain(..take) {
                s.extend_from_slice(&v.to_le_bytes());
            }
            while s.len() < ss - 4 {
                s.extend_from_slice(&FREESECT.to_le_bytes());
            }
            let next = if i + 1 < self.difat_n { self.difat_start + i as u32 + 1 } else { ENDOFCHAIN };
            s.extend_from_slice(&next.to_le_bytes());
            out.extend_from_slice(&s);
        }
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
