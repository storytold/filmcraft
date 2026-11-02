//! CEA-608 (line 21) character sets and control codes, from the public specification
//! (ANSI/CTA-608-E; 47 CFR §15.119).
//!
//! Bytes travel in pairs with odd parity in bit 7. A pair whose first byte (parity stripped) is
//! 0x10–0x1F is a control code (channel 1: 0x10–0x17, channel 2: 0x18–0x1F); otherwise both bytes
//! are characters from the basic set (0x00 = padding).

/// Add odd parity to a 7-bit value.
pub fn with_parity(b: u8) -> u8 {
    let b = b & 0x7f;
    if b.count_ones().is_multiple_of(2) { b | 0x80 } else { b }
}

/// Basic character set (0x20–0x7F). Differs from ASCII in ten positions.
pub fn basic_char(b: u8) -> Option<char> {
    Some(match b {
        0x2a => 'á',
        0x5c => 'é',
        0x5e => 'í',
        0x5f => 'ó',
        0x60 => 'ú',
        0x7b => 'ç',
        0x7c => '÷',
        0x7d => 'Ñ',
        0x7e => 'ñ',
        0x7f => '█',
        0x20..=0x7e => b as char,
        _ => return None,
    })
}

/// Special characters: control pair `0x11, 0x30 + i`.
pub const SPECIAL: [char; 16] = ['®', '°', '½', '¿', '™', '¢', '£', '♪', 'à', '\u{a0}', 'è', 'â', 'ê', 'î', 'ô', 'û'];

/// Extended Spanish/French/miscellaneous: control pair `0x12, 0x20 + i`.
pub const EXT_12: [char; 32] = [
    'Á', 'É', 'Ó', 'Ú', 'Ü', 'ü', '‘', '¡', '*', '\'', '—', '©', '℠', '•', '“', '”', 'À', 'Â', 'Ç', 'È', 'Ê', 'Ë', 'ë', 'Î', 'Ï', 'ï', 'Ô', 'Ù', 'ù', 'Û', '«',
    '»',
];

/// Extended Portuguese/German/Danish: control pair `0x13, 0x20 + i`.
pub const EXT_13: [char; 32] = [
    'Ã', 'ã', 'Í', 'Ì', 'ì', 'Ò', 'ò', 'Õ', 'õ', '{', '}', '\\', '^', '_', '|', '~', 'Ä', 'ä', 'Ö', 'ö', 'ß', '¥', '¤', '¦', 'Å', 'å', 'Ø', 'ø', '┌', '┐', '└',
    '┘',
];

/// How a character is transmitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoded {
    /// One basic-set byte.
    Basic(u8),
    /// A two-byte special/extended code (channel 1 first byte, second byte). Extended characters
    /// replace the preceding character, so a basic `fallback` is sent first for older decoders.
    Pair { b1: u8, b2: u8, fallback: Option<u8> },
}

fn basic_byte(c: char) -> Option<u8> {
    (0x20u8..=0x7f).find(|&b| basic_char(b) == Some(c))
}

/// The close basic-set character sent for one 608 has no code for, else `c`.
fn stand_in(c: char) -> char {
    match c {
        '’' | 'ʼ' => '\'',
        '\t' => ' ',
        '–' => '-',
        _ => c,
    }
}

/// What a 608 decoder shows for `c`: `c`, the stand-in [`encode_char`] sends instead (’ as '),
/// or `None` when 608 has no code for it and the writer drops it.
pub fn shown_as(c: char) -> Option<char> {
    encode_char(c).map(|_| stand_in(c))
}

/// Encode one character (`None` if 608 cannot show it).
pub fn encode_char(c: char) -> Option<Encoded> {
    let c = stand_in(c);
    if let Some(b) = basic_byte(c) {
        return Some(Encoded::Basic(b));
    }
    if let Some(i) = SPECIAL.iter().position(|&s| s == c) {
        return Some(Encoded::Pair { b1: 0x11, b2: 0x30 + i as u8, fallback: None });
    }
    let fallback = |c: char| -> u8 {
        let f = match c {
            'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'O',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'ü' | 'ù' => 'u',
            'ë' => 'e',
            'ï' | 'ì' => 'i',
            'ã' | 'ä' | 'å' => 'a',
            'ò' | 'õ' | 'ö' | 'ø' => 'o',
            'Ç' => 'C',
            'ß' => 's',
            '‘' | '“' | '”' | '«' | '»' => '"',
            '{' | '┌' | '└' => '[',
            '}' | '┐' | '┘' => ']',
            '¡' => '!',
            '*' | '•' => '.',
            _ => '-',
        };
        basic_byte(f).unwrap_or(0x2d)
    };
    if let Some(i) = EXT_12.iter().position(|&s| s == c) {
        return Some(Encoded::Pair { b1: 0x12, b2: 0x20 + i as u8, fallback: Some(fallback(c)) });
    }
    if let Some(i) = EXT_13.iter().position(|&s| s == c) {
        return Some(Encoded::Pair { b1: 0x13, b2: 0x20 + i as u8, fallback: Some(fallback(c)) });
    }
    None
}

/// Miscellaneous control codes (second byte, first byte 0x14 on channel 1).
pub mod misc {
    pub const RCL: u8 = 0x20; // resume caption loading (pop-on)
    pub const BS: u8 = 0x21; // backspace
    pub const AOF: u8 = 0x22;
    pub const AON: u8 = 0x23;
    pub const DER: u8 = 0x24; // delete to end of row
    pub const RU2: u8 = 0x25; // roll-up 2/3/4 rows
    pub const RU3: u8 = 0x26;
    pub const RU4: u8 = 0x27;
    pub const FON: u8 = 0x28;
    pub const RDC: u8 = 0x29; // resume direct captioning (paint-on)
    pub const TR: u8 = 0x2a;
    pub const RTD: u8 = 0x2b;
    pub const EDM: u8 = 0x2c; // erase displayed memory
    pub const CR: u8 = 0x2d; // carriage return (roll-up)
    pub const ENM: u8 = 0x2e; // erase non-displayed memory
    pub const EOC: u8 = 0x2f; // end of caption (swap memories)
}

/// Row (1–15) and indent column addressed by a preamble address code, if `(b1, b2)` is one
/// (channel bit already removed from `b1`).
pub fn decode_pac(b1: u8, b2: u8) -> Option<(usize, usize)> {
    if !(0x40..=0x7f).contains(&b2) {
        return None;
    }
    let hi = b2 >= 0x60;
    let row = match (b1, hi) {
        (0x11, false) => 1,
        (0x11, true) => 2,
        (0x12, false) => 3,
        (0x12, true) => 4,
        (0x15, false) => 5,
        (0x15, true) => 6,
        (0x16, false) => 7,
        (0x16, true) => 8,
        (0x17, false) => 9,
        (0x17, true) => 10,
        (0x10, false) => 11,
        (0x13, false) => 12,
        (0x13, true) => 13,
        (0x14, false) => 14,
        (0x14, true) => 15,
        _ => return None,
    };
    let a = b2 & 0x1f;
    let indent = if a & 0x10 != 0 { ((a & 0x0e) >> 1) as usize * 4 } else { 0 };
    Some((row, indent))
}

/// Preamble address code for `row` (1–15) with an indent (rounded down to a multiple of 4).
pub fn encode_pac(row: usize, indent: usize) -> (u8, u8) {
    let (b1, hi) = match row {
        1 => (0x11, false),
        2 => (0x11, true),
        3 => (0x12, false),
        4 => (0x12, true),
        5 => (0x15, false),
        6 => (0x15, true),
        7 => (0x16, false),
        8 => (0x16, true),
        9 => (0x17, false),
        10 => (0x17, true),
        11 => (0x10, false),
        12 => (0x13, false),
        13 => (0x13, true),
        14 => (0x14, false),
        _ => (0x14, true),
    };
    let attr = if indent >= 4 { 0x10 | (((indent / 4).min(7) as u8) << 1) } else { 0 };
    (b1, (if hi { 0x60 } else { 0x40 }) | attr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity() {
        assert_eq!(with_parity(0x14), 0x94);
        assert_eq!(with_parity(0x2c), 0x2c);
        assert_eq!(with_parity(0x20), 0x20);
        assert_eq!(with_parity(0x2f), 0x2f);
        assert_eq!(with_parity(0x2e), 0xae);
        for b in 0..128u8 {
            assert_eq!(with_parity(b).count_ones() % 2, 1);
        }
    }

    #[test]
    fn pac_roundtrip() {
        for row in 1..=15 {
            for indent in (0..32).step_by(4) {
                let (b1, b2) = encode_pac(row, indent);
                assert_eq!(decode_pac(b1, b2), Some((row, indent)), "row {row} indent {indent}");
            }
        }
    }

    #[test]
    fn chars() {
        assert_eq!(encode_char('A'), Some(Encoded::Basic(0x41)));
        assert_eq!(encode_char('é'), Some(Encoded::Basic(0x5c)));
        assert_eq!(encode_char('♪'), Some(Encoded::Pair { b1: 0x11, b2: 0x37, fallback: None }));
        assert!(matches!(encode_char('{'), Some(Encoded::Pair { b1: 0x13, b2: 0x29, .. })));
        assert!(matches!(encode_char('*'), Some(Encoded::Pair { b1: 0x12, b2: 0x28, .. })));
        assert_eq!(encode_char('漢'), None);
    }
}
