//! SFNT face metadata discovery for FilmCraft fonts.
pub use craft_font_catalog::sfnt::{FaceNames, read_faces, read_faces_bytes};

#[cfg(test)]
mod tests {
    use super::*;
    fn be16(b: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
    }
    fn be32(b: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_be_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
    }
    #[test]
    fn reads_bundled_names() {
        let f = read_faces_bytes(crate::fonts::INTER_BOLD);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].family, "Inter");
        assert_eq!(f[0].style, "Bold");
        assert_eq!(f[0].weight, 700);
        assert!(!f[0].italic);
        let i = read_faces_bytes(crate::fonts::INTER_ITALIC);
        assert!(i[0].italic, "{:?}", i[0]);
        let n = read_faces_bytes(crate::fonts::NOTO_SERIF_REGULAR);
        assert_eq!(n[0].family, "Noto Serif");
        assert!(read_faces_bytes(b"not a font").is_empty());
    }

    #[test]
    fn faces_without_outline_tables_are_left_out() {
        use skrifa::MetadataProvider;
        // Renaming the `glyf` record of a copy of Inter Regular gives a face with no outline
        // tables, like a face whose glyphs are bitmaps only or are kept in Apple's `hvgl` table.
        // The new tag keeps the table directory sorted.
        let font = crate::fonts::INTER_REGULAR;
        let n = usize::from(be16(font, 4).unwrap());
        let glyf = (0..n).map(|i| 12 + 16 * i).find(|&r| &font[r..r + 4] == b"glyf").unwrap();
        let mut renamed = font.to_vec();
        renamed[glyf..glyf + 4].copy_from_slice(b"gxyz");
        // skrifa maps 'A' to a glyph of the copy and reads no outline for it.
        let copy = skrifa::FontRef::new(&renamed).unwrap();
        assert!(copy.outline_glyphs().get(copy.charmap().map('A').unwrap()).is_none());
        assert!(read_faces_bytes(&renamed).is_empty());
        assert_eq!(read_faces_bytes(font).len(), 1);
        // A collection holds both faces, which share one copy of the tables. Table offsets count
        // from the start of the collection. Only the face with outlines is listed, with its index 1.
        let dir = 12 + 16 * n;
        let tables_at = 20 + 2 * dir;
        let shifted = |face: &[u8]| {
            let mut d = face[..dir].to_vec();
            for r in (12..dir).step_by(16) {
                let o = be32(&d, r + 8).unwrap() + tables_at as u32;
                d[r + 8..r + 12].copy_from_slice(&o.to_be_bytes());
            }
            d
        };
        // The header holds the tag, version 1.0, the number of faces and where each face's table
        // directory starts.
        let mut ttc = [*b"ttcf", [0, 1, 0, 0], 2u32.to_be_bytes(), 20u32.to_be_bytes(), (20 + dir as u32).to_be_bytes()].concat();
        ttc.extend(shifted(&renamed));
        ttc.extend(shifted(font));
        ttc.extend_from_slice(font);
        let faces = read_faces_bytes(&ttc);
        assert_eq!(faces.len(), 1);
        assert_eq!((faces[0].index, faces[0].family.as_str(), faces[0].style.as_str()), (1, "Inter", "Regular"));
    }
}
