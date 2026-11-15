# filmcraft-cfb

Compatibility facade for [craft-cfb](https://github.com/storytold/craft-libs/tree/7f3fdc0dfebd03f33a241f91f916e9a78d3e5c93/crates/cfb),
the shared Compound File Binary reader/writer used for AAF. All format implementation now lives
in craft-libs; this crate retains the `filmcraft_cfb` import path for consumers. AAF object mapping
remains in `filmcraft-interchange`. The public round-trip/hostile-input and writer-scale tests
remain here and now exercise the shared implementation. The private sector-span arithmetic test
is upstream with the implementation.

The shared reader additionally defaults to a 1 GiB per-stream and 1,048,576 directory-entry limit;
FilmCraft does not disable those safeguards. The shared crate is std-only and forbids unsafe code.

## Specification

| Document | Edition | Used for |
|---|---|---|
| [MS-CFB] Compound File Binary File Format (Microsoft Open Specifications) | revision 10.0 (the format is unchanged since 1.0) | header, sector chains, FAT / DIFAT / mini FAT, directory entries, red-black sibling trees, name comparison, version 3 and 4 files |

## Reading

```rust
let bytes = std::fs::read("sequence.aaf")?;
let cf = filmcraft_cfb::CompoundFile::open(&bytes)?;
let header = cf.find("Header-2")?;                      // a storage
println!("{:02x?}", cf.entries[header].clsid);          // its class id
let props = cf.read_path("Header-2/properties")?;       // a stream
for &child in &cf.entries[header].children { /* sorted siblings */ }
```

- Accepts version 3 (512-byte sectors) and version 4 (4096-byte sectors) files; other sector
  shifts between 7 and 16 are tolerated.
- Every chain (DIFAT, FAT, mini FAT, directory, streams) is bounds-checked and cycle-checked; a
  directory tree that references an entry twice is rejected. Damaged or truncated files return
  `Error::Invalid`, never panic or loop.
- Version 3 stream sizes ignore the high 32 bits (§2.6.3).

## Writing

```rust
let mut w = filmcraft_cfb::Writer::new(filmcraft_cfb::Version::V4);
w.set_clsid(filmcraft_cfb::Writer::ROOT, root_class);
let h = w.storage(filmcraft_cfb::Writer::ROOT, "Header-2", header_class)?;
w.stream(h, "properties", bytes)?;
let file: Vec<u8> = w.finish();
```

Layout: stream sectors (streams ≥ 4096 bytes), mini stream, mini FAT, directory, FAT, DIFAT. Each
chain is contiguous. Sibling trees are balanced binary search trees in [MS-CFB] name order
(length first, then upper-cased code units) coloured as valid red-black trees. Names are checked
(1–31 UTF-16 code units, none of `/ \ : !`, unique per storage, case-insensitively).

## Tests

`src/tests.rs`: round trips for both versions with empty, mini, boundary (63/64/65, 4095/4096/4097
bytes) and multi-sector streams and up to 40 siblings; red-black invariants of every sibling tree;
a 7.5 MB stream that needs DIFAT sectors; Unicode and case-insensitive names; truncation at every
29 bytes and 1500 random mutations never panic and only return complete streams.
The writer rejects a duplicate sibling name with an O(1) hash lookup on the upper-cased name (a scan over the
siblings made n children cost O(n^2): 20 000 AAF markers took a minute); `adding_many_children_is_not_quadratic`
and `duplicate_names_are_still_rejected` (`tests/writer_scale.rs`) cover it.
