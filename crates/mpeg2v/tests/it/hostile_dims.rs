//! A 14-byte sequence header must not make the decoder size its frame buffers at 16383 x 16383
//! (gigabytes per picture). Hand-built, no ffmpeg.

use filmcraft_mpeg2v::Decoder;

#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    n: usize,
}

impl Bits {
    fn put(&mut self, value: u32, bits: usize) {
        for i in (0..bits).rev() {
            if self.n.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1
                && let Some(b) = self.bytes.last_mut()
            {
                *b |= 0x80 >> (self.n % 8);
            }
            self.n += 1;
        }
    }
}

/// Sequence header plus sequence extension for a `w` x `h` 4:4:4 stream.
fn sequence(w: u32, h: u32, chroma_format: u32) -> Vec<u8> {
    let mut s = Bits::default();
    s.put(w & 0xfff, 12);
    s.put(h & 0xfff, 12);
    s.put(1, 4); // aspect_ratio_information
    s.put(4, 4); // frame_rate_code
    s.put(1000, 18); // bit_rate_value
    s.put(1, 1); // marker
    s.put(100, 10); // vbv_buffer_size_value
    s.put(0, 3); // constrained_parameters_flag, load_intra, load_non_intra
    let mut e = Bits::default();
    e.put(1, 4); // sequence_extension
    e.put(0x48, 8); // profile_and_level_indication
    e.put(1, 1); // progressive_sequence
    e.put(chroma_format, 2);
    e.put(w >> 12, 2);
    e.put(h >> 12, 2);
    e.put(0, 12); // bit_rate_extension
    e.put(1, 1); // marker
    e.put(0, 8 + 1 + 2 + 5); // vbv_buffer_size_extension, low_delay, frame_rate_extension_n/d
    let mut out = vec![0, 0, 1, 0xb3];
    out.extend(s.bytes);
    out.extend([0, 0, 1, 0xb5]);
    out.extend(e.bytes);
    out
}

fn accepted(w: u32, h: u32, chroma_format: u32) -> bool {
    let mut dec = Decoder::new();
    let _ = dec.decode(&sequence(w, h, chroma_format), 0);
    dec.sequence().is_some()
}

#[test]
fn ordinary_sequences_are_accepted() {
    assert!(accepted(1920, 1080, 1));
    assert!(accepted(7680, 4320, 3));
}

#[test]
fn gigantic_sequences_are_refused() {
    assert!(!accepted(16383, 16383, 3));
    assert!(!accepted(16383, 16383, 1));
    assert!(!accepted(8192, 8192, 1));
}
