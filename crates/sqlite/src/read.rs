//! Reading: the database header, table b-trees, overflow chains and records.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::schema::{Columns, parse_create_table};
use crate::{Error, MAGIC, Result, Value, corrupt, read_varint};

/// Deeper table b-trees cannot occur in a file below the 2^32-page limit; anything deeper is a
/// cycle or damage.
const MAX_DEPTH: usize = 40;

/// Page-type bytes of the b-tree page header.
const INTERIOR_INDEX: u8 = 0x02;
const INTERIOR_TABLE: u8 = 0x05;
const LEAF_INDEX: u8 = 0x0a;
const LEAF_TABLE: u8 = 0x0d;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// One row of the schema table (`sqlite_schema`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaEntry {
    /// `table`, `index`, `view` or `trigger`.
    pub kind: String,
    pub name: String,
    /// The table an index or trigger belongs to (its own name for a table).
    pub table: String,
    /// Root page of its b-tree (0 for views, triggers and virtual tables).
    pub root: u32,
    /// The `CREATE` statement (empty for automatic indexes).
    pub sql: String,
}

/// A table and its column names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<String>,
    /// The `INTEGER PRIMARY KEY` column, if any: it holds the row id.
    pub rowid_alias: Option<usize>,
    real: Vec<bool>,
    root: u32,
}

impl Table {
    /// Index of a column (ASCII case-insensitive, as SQL identifiers are).
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.eq_ignore_ascii_case(name))
    }
}

/// One row of a table.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub rowid: i64,
    pub values: Vec<Value>,
}

static NULL: Value = Value::Null;

impl Row {
    /// Column `i`, NULL when the record is shorter (a column added after the row was written).
    pub fn get(&self, i: usize) -> &Value {
        self.values.get(i).unwrap_or(&NULL)
    }
}

/// A database file held in memory.
pub struct Database<'a> {
    data: &'a [u8],
    page_size: usize,
    usable: usize,
    page_count: u32,
    encoding: Encoding,
    wal: bool,
    schema: Vec<SchemaEntry>,
}

/// The big-endian u16 at `at`.
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at.checked_add(1)?)?]))
}

/// The big-endian u32 at `at`.
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

impl<'a> Database<'a> {
    /// Parse the header and the schema table.
    pub fn open(data: &'a [u8]) -> Result<Self> {
        if data.len() < 100 || !data.starts_with(MAGIC) {
            return Err(Error::NotSqlite);
        }
        let raw = u16_at(data, 16).ok_or_else(|| corrupt("short header"))?;
        let page_size = if raw == 1 { 65536 } else { usize::from(raw) };
        if !(512..=65536).contains(&page_size) || !page_size.is_power_of_two() {
            return Err(corrupt(format!("page size {page_size}")));
        }
        let reserved = usize::from(data[20]);
        let usable = page_size.checked_sub(reserved).filter(|u| *u >= 480).ok_or_else(|| corrupt(format!("{reserved} reserved bytes per page")))?;
        let wal = data[18] == 2 || data[19] == 2;
        let file_pages = u32::try_from(data.len() / page_size).unwrap_or(u32::MAX);
        // The header's page count is valid only when written by a version that maintains it
        // (its change counter matches the "version-valid-for" number).
        let header_pages = u32_at(data, 28).unwrap_or(0);
        let valid = u32_at(data, 24) == u32_at(data, 92);
        let page_count = if valid && header_pages > 0 && header_pages <= file_pages { header_pages } else { file_pages };
        if page_count == 0 {
            return Err(corrupt("no complete page"));
        }
        let encoding = match u32_at(data, 56).unwrap_or(0) {
            0 | 1 => Encoding::Utf8,
            2 => Encoding::Utf16Le,
            3 => Encoding::Utf16Be,
            e => return Err(corrupt(format!("text encoding {e}"))),
        };
        let mut db = Database { data, page_size, usable, page_count, encoding, wal, schema: Vec::new() };
        let mut schema = Vec::new();
        db.walk(1, &mut |_, payload| {
            let v = db.record(payload)?;
            let text = |i: usize| v.get(i).and_then(Value::as_str).unwrap_or_default().to_string();
            let root = v.get(3).and_then(Value::as_i64).and_then(|r| u32::try_from(r).ok()).unwrap_or(0);
            schema.push(SchemaEntry { kind: text(0), name: text(1), table: text(2), root, sql: text(4) });
            Ok(())
        })?;
        db.schema = schema;
        Ok(db)
    }

    /// The page size in bytes.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Whether the file is in write-ahead-log mode: recent changes may still be in the `-wal`
    /// file next to it, which this reader does not apply.
    pub fn wal_mode(&self) -> bool {
        self.wal
    }

    /// Every schema entry (tables, indexes, views, triggers).
    pub fn schema(&self) -> &[SchemaEntry] {
        &self.schema
    }

    /// Names of the tables.
    pub fn table_names(&self) -> impl Iterator<Item = &str> {
        self.schema.iter().filter(|e| e.kind == "table").map(|e| e.name.as_str())
    }

    /// Whether a table of that name exists (ASCII case-insensitive).
    pub fn has_table(&self, name: &str) -> bool {
        self.table_names().any(|t| t.eq_ignore_ascii_case(name))
    }

    /// A table's description.
    pub fn table(&self, name: &str) -> Result<Table> {
        let e = self.schema.iter().find(|e| e.kind == "table" && e.name.eq_ignore_ascii_case(name)).ok_or_else(|| Error::NoTable(name.to_string()))?;
        let cols: Columns = parse_create_table(&e.sql).ok_or_else(|| corrupt(format!("cannot read the columns of table \"{}\"", e.name)))?;
        if cols.without_rowid {
            return Err(Error::Unsupported(format!("WITHOUT ROWID table \"{}\"", e.name)));
        }
        if e.root == 0 {
            return Err(Error::Unsupported(format!("virtual table \"{}\"", e.name)));
        }
        Ok(Table { name: e.name.clone(), columns: cols.names, rowid_alias: cols.rowid_alias, real: cols.real, root: e.root })
    }

    /// Every row of a table, in row-id order.
    pub fn rows(&self, table: &Table) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        self.walk(table.root, &mut |rowid, payload| {
            let mut values = self.record(payload)?;
            if let Some(a) = table.rowid_alias
                && let Some(v) = values.get_mut(a)
                && v.is_null()
            {
                *v = Value::Integer(rowid);
            }
            // a REAL column stores whole numbers as integers to save space
            for (v, real) in values.iter_mut().zip(&table.real) {
                if *real && let Value::Integer(i) = *v {
                    *v = Value::Real(i as f64);
                }
            }
            rows.push(Row { rowid, values });
            Ok(())
        })?;
        Ok(rows)
    }

    /// A table's description and all its rows.
    pub fn read_table(&self, name: &str) -> Result<(Table, Vec<Row>)> {
        let t = self.table(name)?;
        let rows = self.rows(&t)?;
        Ok((t, rows))
    }

    /// Page `n` (counting from 1), checked to lie inside the file.
    fn page(&self, n: u32) -> Result<&'a [u8]> {
        if n == 0 || n > self.page_count {
            return Err(corrupt(format!("page {n} is outside the file ({} pages)", self.page_count)));
        }
        let start = (n as usize - 1).checked_mul(self.page_size).ok_or_else(|| corrupt("page offset overflows"))?;
        self.data.get(start..start + self.page_size).ok_or_else(|| corrupt(format!("page {n} is truncated")))
    }

    /// Visit every cell of the table b-tree rooted at `root`, in key order.
    fn walk(&self, root: u32, f: &mut dyn FnMut(i64, Cow<'a, [u8]>) -> Result<()>) -> Result<()> {
        let mut stack = vec![(root, 0usize)];
        let mut seen = HashSet::new();
        while let Some((n, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return Err(corrupt("b-tree too deep"));
            }
            if !seen.insert(n) {
                return Err(corrupt(format!("page {n} is referenced twice")));
            }
            let page = self.page(n)?;
            let h = if n == 1 { 100 } else { 0 };
            let kind = *page.get(h).ok_or_else(|| corrupt("short page"))?;
            let cells = usize::from(u16_at(page, h + 3).ok_or_else(|| corrupt("short page header"))?);
            let header_len = match kind {
                LEAF_TABLE => 8,
                INTERIOR_TABLE => 12,
                INTERIOR_INDEX | LEAF_INDEX => return Err(Error::Unsupported(format!("page {n} is an index b-tree page inside a table"))),
                k => return Err(corrupt(format!("page {n} has page type {k:#04x}"))),
            };
            let ptrs = h + header_len;
            if ptrs + cells * 2 > self.usable {
                return Err(corrupt(format!("page {n} claims {cells} cells")));
            }
            let cell_at = |i: usize| -> Result<usize> {
                let off = usize::from(u16_at(page, ptrs + i * 2).ok_or_else(|| corrupt("short cell pointer array"))?);
                if off < ptrs + cells * 2 || off >= self.usable {
                    return Err(corrupt(format!("page {n}: cell offset {off}")));
                }
                Ok(off)
            };
            if kind == INTERIOR_TABLE {
                let right = u32_at(page, h + 8).ok_or_else(|| corrupt("short interior header"))?;
                stack.push((right, depth + 1));
                for i in (0..cells).rev() {
                    let child = u32_at(page, cell_at(i)?).ok_or_else(|| corrupt("short interior cell"))?;
                    stack.push((child, depth + 1));
                }
                continue;
            }
            for i in 0..cells {
                let at = cell_at(i)?;
                let (len, p) = read_varint(page, at).ok_or_else(|| corrupt("bad payload size"))?;
                let (rowid, p) = read_varint(page, p).ok_or_else(|| corrupt("bad row id"))?;
                let payload = self.payload(page, p, len)?;
                f(rowid as i64, payload)?;
            }
        }
        Ok(())
    }

    /// The payload of a table-leaf cell whose local part starts at `at`, following its overflow
    /// chain when it does not fit on the page.
    fn payload(&self, page: &'a [u8], at: usize, len: u64) -> Result<Cow<'a, [u8]>> {
        let u = self.usable as u64;
        let max_total = u64::from(self.page_count).saturating_mul(u);
        if len > max_total {
            return Err(corrupt(format!("payload of {len} bytes in a {max_total}-byte file")));
        }
        let x = u - 35;
        if len <= x {
            let end = at.checked_add(len as usize).ok_or_else(|| corrupt("payload overflows"))?;
            return page.get(at..end).filter(|_| end <= self.usable).map(Cow::Borrowed).ok_or_else(|| corrupt("payload runs off the page"));
        }
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + ((len - m) % (u - 4));
        let local = if k <= x { k } else { m } as usize;
        let local_end = at + local;
        let mut out = Vec::with_capacity(len as usize);
        out.extend_from_slice(page.get(at..local_end).filter(|_| local_end + 4 <= self.usable).ok_or_else(|| corrupt("payload runs off the page"))?);
        let mut next = u32_at(page, local_end).ok_or_else(|| corrupt("missing overflow page number"))?;
        let mut seen = HashSet::new();
        while (out.len() as u64) < len {
            if !seen.insert(next) {
                return Err(corrupt(format!("overflow page {next} is referenced twice")));
            }
            let p = self.page(next)?;
            let take = ((len - out.len() as u64).min(u - 4)) as usize;
            out.extend_from_slice(p.get(4..4 + take).ok_or_else(|| corrupt("short overflow page"))?);
            next = u32_at(p, 0).ok_or_else(|| corrupt("short overflow page"))?;
        }
        Ok(Cow::Owned(out))
    }

    /// Decode a record: a header of serial types, then the values.
    fn record(&self, b: Cow<'_, [u8]>) -> Result<Vec<Value>> {
        let b = &*b;
        let (hlen, mut hp) = read_varint(b, 0).ok_or_else(|| corrupt("bad record header size"))?;
        let hlen = usize::try_from(hlen).ok().filter(|h| *h <= b.len()).ok_or_else(|| corrupt("record header runs past the record"))?;
        let mut bp = hlen;
        let mut values = Vec::new();
        while hp < hlen {
            let (t, np) = read_varint(b, hp).ok_or_else(|| corrupt("bad serial type"))?;
            hp = np;
            let int = |n: usize, bp: usize| -> Result<Value> {
                let s = b.get(bp..bp + n).ok_or_else(|| corrupt("record value runs past the record"))?;
                let mut v: i64 = if s.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
                for x in s {
                    v = (v << 8) | i64::from(*x);
                }
                Ok(Value::Integer(v))
            };
            let (value, len) = match t {
                0 => (Value::Null, 0),
                1 => (int(1, bp)?, 1),
                2 => (int(2, bp)?, 2),
                3 => (int(3, bp)?, 3),
                4 => (int(4, bp)?, 4),
                5 => (int(6, bp)?, 6),
                6 => (int(8, bp)?, 8),
                7 => {
                    let s = b.get(bp..bp + 8).ok_or_else(|| corrupt("record value runs past the record"))?;
                    let mut a = [0u8; 8];
                    a.copy_from_slice(s);
                    (Value::Real(f64::from_be_bytes(a)), 8)
                }
                8 => (Value::Integer(0), 0),
                9 => (Value::Integer(1), 0),
                10 | 11 => return Err(corrupt(format!("reserved serial type {t}"))),
                n => {
                    let len = usize::try_from((n - 12) / 2).map_err(|_| corrupt("value too large"))?;
                    let end = bp.checked_add(len).ok_or_else(|| corrupt("value too large"))?;
                    let s = b.get(bp..end).ok_or_else(|| corrupt("record value runs past the record"))?;
                    if n % 2 == 0 { (Value::Blob(s.to_vec()), len) } else { (Value::Text(self.text(s)), len) }
                }
            };
            values.push(value);
            bp += len;
        }
        Ok(values)
    }

    /// TEXT bytes, in the database's text encoding.
    fn text(&self, s: &[u8]) -> String {
        let units = |le: bool| s.chunks_exact(2).map(move |c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
        match self.encoding {
            Encoding::Utf8 => String::from_utf8_lossy(s).into_owned(),
            Encoding::Utf16Le => String::from_utf16_lossy(&units(true).collect::<Vec<_>>()),
            Encoding::Utf16Be => String::from_utf16_lossy(&units(false).collect::<Vec<_>>()),
        }
    }
}
