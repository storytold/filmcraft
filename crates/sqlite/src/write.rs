//! A minimal writer: rowid tables (no indexes), for tests and fixtures. It lays out the same
//! structures the reader parses: a header, table b-trees of any depth, overflow chains and
//! records with the shortest serial types.

use crate::{MAGIC, Value, varint_len, write_varint};

/// Builds a database file.
pub struct Writer {
    page_size: usize,
    tables: Vec<(String, String, Vec<Vec<Value>>)>,
}

impl Writer {
    /// `page_size` is rounded to a power of two between 512 and 65536.
    pub fn new(page_size: usize) -> Writer {
        Writer { page_size: page_size.clamp(512, 65536).next_power_of_two().min(65536), tables: Vec::new() }
    }

    /// Add a table: `sql` is its `CREATE TABLE` statement and `rows` its rows (row ids 1, 2, …).
    pub fn table(&mut self, name: &str, sql: &str, rows: Vec<Vec<Value>>) -> &mut Self {
        self.tables.push((name.to_string(), sql.to_string(), rows));
        self
    }

    /// The database file.
    pub fn finish(self) -> Vec<u8> {
        let mut pages: Vec<Vec<u8>> = vec![Vec::new()]; // page 1 is filled last
        let mut schema = Vec::new();
        for (name, sql, rows) in &self.tables {
            let cells = rows.iter().enumerate().map(|(i, r)| (i as i64 + 1, record(r))).collect::<Vec<_>>();
            let root = self.tree(&mut pages, &cells, false);
            schema.push(vec![
                Value::Text("table".into()),
                Value::Text(name.clone()),
                Value::Text(name.clone()),
                Value::Integer(i64::from(root)),
                Value::Text(sql.clone()),
            ]);
        }
        let cells = schema.iter().enumerate().map(|(i, r)| (i as i64 + 1, record(r))).collect::<Vec<_>>();
        self.tree(&mut pages, &cells, true);
        let mut out = Vec::with_capacity(pages.len() * self.page_size);
        for p in &pages {
            out.extend_from_slice(p);
        }
        let count = pages.len() as u32;
        header(&mut out[..100], self.page_size, count);
        out
    }

    /// Bytes of each page that hold data (the writer reserves none).
    fn usable(&self) -> usize {
        self.page_size
    }

    /// Write a table b-tree; returns its root page (1 when `on_page_one`).
    fn tree(&self, pages: &mut Vec<Vec<u8>>, rows: &[(i64, Vec<u8>)], on_page_one: bool) -> u32 {
        let cells: Vec<(i64, Vec<u8>)> = rows.iter().map(|(rowid, rec)| (*rowid, self.leaf_cell(pages, *rowid, rec))).collect();
        let cap = |first: bool, header: usize| self.usable() - header - if first { 100 } else { 0 };
        // A tree that fits on its root page.
        let fits = |cells: &[Vec<u8>], header: usize, first: bool| cells.iter().map(|c| c.len() + 2).sum::<usize>() <= cap(first, header);
        let leaf_cells: Vec<Vec<u8>> = cells.iter().map(|(_, c)| c.clone()).collect();
        if fits(&leaf_cells, 8, on_page_one) {
            return self.put(pages, page(&leaf_cells, 0x0d, None, on_page_one, self.page_size), on_page_one);
        }
        // Leaves, then interior levels until one page holds the rest.
        let mut level: Vec<(u32, i64)> = Vec::new();
        let mut group: Vec<Vec<u8>> = Vec::new();
        let mut used = 0;
        let mut last_key = 0;
        for (rowid, c) in cells {
            if !group.is_empty() && used + c.len() + 2 > cap(false, 8) {
                let n = self.put(pages, page(&group, 0x0d, None, false, self.page_size), false);
                level.push((n, last_key));
                group.clear();
                used = 0;
            }
            used += c.len() + 2;
            group.push(c);
            last_key = rowid;
        }
        if !group.is_empty() {
            let n = self.put(pages, page(&group, 0x0d, None, false, self.page_size), false);
            level.push((n, last_key));
        }
        loop {
            let interior = |children: &[(u32, i64)]| -> (Vec<Vec<u8>>, u32) {
                let cells = children[..children.len() - 1]
                    .iter()
                    .map(|(n, key)| {
                        let mut c = n.to_be_bytes().to_vec();
                        write_varint(*key as u64, &mut c);
                        c
                    })
                    .collect();
                (cells, children[children.len() - 1].0)
            };
            if level.len() == 1 && !on_page_one {
                return level[0].0;
            }
            let (all, right) = interior(&level);
            if fits(&all, 12, on_page_one) {
                return self.put(pages, page(&all, 0x05, Some(right), on_page_one, self.page_size), on_page_one);
            }
            let mut next = Vec::new();
            let mut start = 0;
            while start < level.len() {
                let mut end = start + 1;
                let mut used = 0;
                while end < level.len() && used + 4 + varint_len(level[end - 1].1 as u64) + 2 <= cap(false, 12) {
                    used += 4 + varint_len(level[end - 1].1 as u64) + 2;
                    end += 1;
                }
                // never leave one child over: alone on a page it would have no key (SQLite reads
                // a non-root interior page without cells as corrupt)
                if level.len() - end == 1 && end - start > 2 {
                    end -= 1;
                }
                let (cells, right) = interior(&level[start..end]);
                let n = self.put(pages, page(&cells, 0x05, Some(right), false, self.page_size), false);
                next.push((n, level[end - 1].1));
                start = end;
            }
            level = next;
        }
    }

    /// A table-leaf cell, spilling the payload to overflow pages when it does not fit.
    fn leaf_cell(&self, pages: &mut Vec<Vec<u8>>, rowid: i64, rec: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        write_varint(rec.len() as u64, &mut c);
        write_varint(rowid as u64, &mut c);
        let u = self.usable() as u64;
        let p = rec.len() as u64;
        let x = u - 35;
        if p <= x {
            c.extend_from_slice(rec);
            return c;
        }
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + ((p - m) % (u - 4));
        let local = if k <= x { k } else { m } as usize;
        c.extend_from_slice(&rec[..local]);
        // overflow pages, each pointing at the next (0 ends the chain)
        let chunks: Vec<&[u8]> = rec[local..].chunks(self.usable() - 4).collect();
        let first = pages.len() as u32 + 1;
        for (i, chunk) in chunks.iter().enumerate() {
            let mut pg = vec![0u8; self.page_size];
            let next = if i + 1 < chunks.len() { first + i as u32 + 1 } else { 0 };
            pg[..4].copy_from_slice(&next.to_be_bytes());
            pg[4..4 + chunk.len()].copy_from_slice(chunk);
            pages.push(pg);
        }
        c.extend_from_slice(&first.to_be_bytes());
        c
    }

    /// Store a finished page: as page 1 when `on_page_one`, else after the others. Returns its
    /// number.
    fn put(&self, pages: &mut Vec<Vec<u8>>, page: Vec<u8>, on_page_one: bool) -> u32 {
        if on_page_one {
            pages[0] = page;
            return 1;
        }
        pages.push(page);
        pages.len() as u32
    }
}

/// A b-tree page: header, cell pointers, cells packed at the end.
fn page(cells: &[Vec<u8>], kind: u8, right: Option<u32>, first: bool, size: usize) -> Vec<u8> {
    let mut p = vec![0u8; size];
    let h = if first { 100 } else { 0 };
    let header_len = if right.is_some() { 12 } else { 8 };
    let mut end = size;
    for (i, c) in cells.iter().enumerate() {
        end -= c.len();
        p[end..end + c.len()].copy_from_slice(c);
        let at = h + header_len + i * 2;
        p[at..at + 2].copy_from_slice(&(end as u16).to_be_bytes());
    }
    p[h] = kind;
    p[h + 3..h + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    // 0 stands for 65536
    p[h + 5..h + 7].copy_from_slice(&((end % 65536) as u16).to_be_bytes());
    if let Some(r) = right {
        p[h + 8..h + 12].copy_from_slice(&r.to_be_bytes());
    }
    p
}

/// The 100-byte database header, for `pages` pages of `page_size` bytes.
fn header(h: &mut [u8], page_size: usize, pages: u32) {
    h[..16].copy_from_slice(MAGIC);
    let ps = if page_size == 65536 { 1 } else { page_size as u16 };
    h[16..18].copy_from_slice(&ps.to_be_bytes());
    h[18] = 1; // legacy (rollback journal) write and read versions
    h[19] = 1;
    h[20] = 0;
    h[21] = 64; // payload fractions, fixed by the format
    h[22] = 32;
    h[23] = 32;
    h[24..28].copy_from_slice(&1u32.to_be_bytes()); // change counter
    h[28..32].copy_from_slice(&pages.to_be_bytes());
    h[40..44].copy_from_slice(&1u32.to_be_bytes()); // schema cookie
    h[44..48].copy_from_slice(&4u32.to_be_bytes()); // schema format 4
    h[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
    h[92..96].copy_from_slice(&1u32.to_be_bytes()); // version-valid-for = change counter
    h[96..100].copy_from_slice(&3_045_000u32.to_be_bytes());
}

/// Encode a record with the shortest serial type for each value.
pub(crate) fn record(values: &[Value]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut body = Vec::new();
    for v in values {
        let t: u64 = match v {
            Value::Null => 0,
            Value::Integer(0) => 8,
            Value::Integer(1) => 9,
            Value::Integer(i) => {
                let (t, n) = match *i {
                    -0x80..=0x7f => (1, 1),
                    -0x8000..=0x7fff => (2, 2),
                    -0x80_0000..=0x7f_ffff => (3, 3),
                    -0x8000_0000..=0x7fff_ffff => (4, 4),
                    -0x8000_0000_0000..=0x7fff_ffff_ffff => (5, 6),
                    _ => (6, 8),
                };
                body.extend_from_slice(&i.to_be_bytes()[8 - n..]);
                t
            }
            Value::Real(f) => {
                body.extend_from_slice(&f.to_be_bytes());
                7
            }
            Value::Text(s) => {
                body.extend_from_slice(s.as_bytes());
                13 + 2 * s.len() as u64
            }
            Value::Blob(b) => {
                body.extend_from_slice(b);
                12 + 2 * b.len() as u64
            }
        };
        write_varint(t, &mut types);
    }
    // the header size counts its own varint
    let mut hlen = types.len() + 1;
    while varint_len(hlen as u64) + types.len() != hlen {
        hlen = varint_len(hlen as u64) + types.len();
    }
    let mut out = Vec::with_capacity(hlen + body.len());
    write_varint(hlen as u64, &mut out);
    out.extend_from_slice(&types);
    out.extend_from_slice(&body);
    out
}
