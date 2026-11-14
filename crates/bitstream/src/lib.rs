//! Bit-level reading and writing: a re-export of [`craft_media::bitstream`] from craft-libs.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_media::bitstream::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_lengths_nal_size_does_not_wrap() {
        // #2801: adding the four-byte prefix to u32::MAX overflowed usize on wasm32.
        for data in [&[0xff; 4][..], &[0, 0, 0, 1, 0x67, 0xff, 0xff, 0xff, 0xff][..]] {
            let result = std::panic::catch_unwind(|| length_prefixed_nals(data, 4));
            assert_eq!(result.expect("NAL splitting must not panic"), Err(BitError::Eof));
        }
    }

    #[test]
    fn hostile_lengths_nal_prefix_width_is_bounded() {
        for width in [0, 5, 8, usize::MAX] {
            for data in [&[][..], &[1, 0x67][..]] {
                let result = std::panic::catch_unwind(|| length_prefixed_nals(data, width));
                assert_eq!(result.expect("invalid prefix widths must not panic or loop"), Err(BitError::InvalidNalLengthSize));
            }
        }
    }

    #[test]
    fn hostile_lengths_nal_mutations_and_truncations() {
        for width in 1..=4 {
            let mut data = vec![0; width]; // retain an empty NAL
            data.extend_from_slice(&1u32.to_be_bytes()[4 - width..]);
            data.push(0x67);
            assert_eq!(length_prefixed_nals(&data, width).unwrap(), vec![&[][..], &[0x67][..]]);
            assert_eq!(length_prefixed_nals(&[], width).unwrap(), Vec::<&[u8]>::new());
            for end in 0..=data.len() {
                std::panic::catch_unwind(|| length_prefixed_nals(&data[..end], width)).expect("truncated NAL sample must not panic").ok();
            }
            for index in 0..data.len() {
                for bit in 0..8 {
                    let mut mutated = data.clone();
                    mutated[index] ^= 1 << bit;
                    std::panic::catch_unwind(|| length_prefixed_nals(&mutated, width)).expect("mutated NAL sample must not panic").ok();
                }
            }
        }
    }
}
