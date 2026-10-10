//! SentencePiece model files (`tokenizer.model`): the piece table of the `ModelProto` protobuf
//! (field 1: repeated `SentencePiece { piece = 1; score = 2; type = 3 }`), for turning token ids
//! back into text. Encoding is never needed for transcription.
//!
//! Format reference: the `sentencepiece_model.proto` schema published with SentencePiece
//! (Apache-2.0) and the protobuf wire-format documentation.
//!
//! Hostile input: lengths are checked against the data, piece count and length are capped.

use crate::SpeechError;

const MAX_PIECES: usize = 1 << 20;
const MAX_PIECE_LEN: usize = 1024;

/// The word-boundary marker (U+2581).
pub const SPACE: char = '\u{2581}';

/// Piece types of the SentencePiece schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Unknown,
    Control,
    UserDefined,
    Unused,
    Byte,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub text: String,
    pub kind: Kind,
}

impl Piece {
    /// Text the piece contributes to a transcript: specials (`<unk>`, `<|nospeech|>`, control
    /// pieces) contribute nothing.
    pub fn is_text(&self) -> bool {
        match self.kind {
            Kind::Normal | Kind::Byte => true,
            Kind::UserDefined => !(self.text.starts_with('<') && self.text.ends_with('>')),
            _ => false,
        }
    }

    /// The byte of a `<0xNN>` byte-fallback piece.
    pub fn byte(&self) -> Option<u8> {
        if self.kind != Kind::Byte {
            return None;
        }
        let hex = self.text.strip_prefix("<0x")?.strip_suffix('>')?;
        u8::from_str_radix(hex, 16).ok()
    }
}

fn bad(msg: &str) -> SpeechError {
    SpeechError::Model(format!("tokenizer.model: {msg}"))
}

fn varint(d: &[u8], p: &mut usize) -> Result<u64, SpeechError> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *d.get(*p).ok_or_else(|| bad("truncated"))?;
        *p += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(bad("varint too long"))
}

/// Skip a field of wire type `wt`, or return a length-delimited payload.
fn field<'a>(d: &'a [u8], p: &mut usize, wt: u64) -> Result<Option<&'a [u8]>, SpeechError> {
    match wt {
        0 => {
            varint(d, p)?;
            Ok(None)
        }
        1 | 5 => {
            *p = p.checked_add(if wt == 1 { 8 } else { 4 }).filter(|&e| e <= d.len()).ok_or_else(|| bad("truncated"))?;
            Ok(None)
        }
        2 => {
            let n = usize::try_from(varint(d, p)?).map_err(|_| bad("length"))?;
            let e = p.checked_add(n).filter(|&e| e <= d.len()).ok_or_else(|| bad("truncated"))?;
            let s = d.get(*p..e).ok_or_else(|| bad("truncated"))?;
            *p = e;
            Ok(Some(s))
        }
        _ => Err(bad("unsupported wire type")),
    }
}

/// The pieces of a serialized `ModelProto`, indexed by token id.
pub fn pieces(data: &[u8]) -> Result<Vec<Piece>, SpeechError> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        let key = varint(data, &mut p)?;
        let (num, wt) = (key >> 3, key & 7);
        let payload = field(data, &mut p, wt)?;
        if num != 1 {
            continue;
        }
        let msg = payload.ok_or_else(|| bad("piece is not a message"))?;
        if out.len() >= MAX_PIECES {
            return Err(bad("too many pieces"));
        }
        let (mut text, mut kind) = (String::new(), Kind::Normal);
        let mut q = 0usize;
        while q < msg.len() {
            let k = varint(msg, &mut q)?;
            let (n, w) = (k >> 3, k & 7);
            if n == 3 && w == 0 {
                kind = match varint(msg, &mut q)? {
                    2 => Kind::Unknown,
                    3 => Kind::Control,
                    4 => Kind::UserDefined,
                    5 => Kind::Unused,
                    6 => Kind::Byte,
                    _ => Kind::Normal,
                };
                continue;
            }
            let v = field(msg, &mut q, w)?;
            if let (1, Some(s)) = (n, v) {
                if s.len() > MAX_PIECE_LEN {
                    return Err(bad("piece too long"));
                }
                text = String::from_utf8_lossy(s).into_owned();
            }
        }
        out.push(Piece { text, kind });
    }
    if out.is_empty() {
        return Err(bad("no pieces"));
    }
    Ok(out)
}

/// Decode token ids to text (SentencePiece rules: `▁` is a space, byte pieces are joined as
/// UTF-8, specials are dropped, the leading space is removed).
pub fn decode(table: &[Piece], ids: &[u32]) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    for &id in ids {
        let Some(p) = table.get(id as usize) else { continue };
        if let Some(b) = p.byte() {
            bytes.push(b);
        } else if p.is_text() {
            bytes.extend_from_slice(p.text.replace(SPACE, " ").as_bytes());
        }
    }
    let s = String::from_utf8_lossy(&bytes).into_owned();
    s.strip_prefix(' ').map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto(pieces: &[(&str, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (t, k) in pieces {
            let mut m = vec![0x0a, t.len() as u8];
            m.extend_from_slice(t.as_bytes());
            m.extend_from_slice(&[0x15, 0, 0, 0x80, 0xbf]); // score -1.0
            if *k != 1 {
                m.extend_from_slice(&[0x18, *k as u8]);
            }
            out.push(0x0a);
            out.push(m.len() as u8);
            out.extend_from_slice(&m);
        }
        // a trainer spec (field 2) is skipped
        out.extend_from_slice(&[0x12, 2, 0x08, 1]);
        out
    }

    #[test]
    fn decodes_pieces() {
        let t = pieces(&proto(&[("<unk>", 2), ("<|nospeech|>", 4), ("\u{2581}He", 1), ("llo", 1), (",", 1), ("<0xC3>", 6), ("<0xA4>", 6), ("\u{2581}h", 1)]))
            .unwrap();
        assert_eq!(t.len(), 8);
        assert_eq!(t[1].kind, Kind::UserDefined);
        assert!(!t[1].is_text());
        assert_eq!(decode(&t, &[0, 1, 2, 3, 4, 7, 5, 6]), "Hello, hä");
        assert_eq!(decode(&t, &[99]), "");
    }

    #[test]
    fn hostile_protobuf_fails_cleanly() {
        assert!(pieces(&[]).is_err());
        assert!(pieces(&[0x0a, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
        assert!(pieces(&[0xff; 11]).is_err());
        let good = proto(&[("\u{2581}a", 1), ("b", 1)]);
        for cut in 0..good.len() {
            let _ = pieces(&good[..cut]);
        }
        for i in 0..good.len() {
            for bit in 0..8 {
                let mut b = good.clone();
                b[i] ^= 1 << bit;
                let _ = pieces(&b);
            }
        }
    }
}
