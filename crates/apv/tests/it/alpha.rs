//! Auxiliary alpha frame PBUs (type 27): a valid one becomes the alpha plane, a damaged one is
//! ignored without disturbing the primary picture.

use filmcraft_apv::header::PBU_ALPHA_FRAME;
use filmcraft_apv::*;

const W: u32 = 64;
const H: u32 = 32;

fn encode(profile: Profile, seed: usize) -> Vec<u8> {
    let chroma = profile.default_chroma();
    let mut f = Frame::new(W, H, chroma, profile.default_bit_depth(), false);
    for (i, s) in f.y.iter_mut().enumerate() {
        *s = (64 + (i * seed) % 800) as u16;
    }
    let mut cfg = EncoderConfig::new(profile, W, H);
    cfg.qp = Some(16);
    Encoder::with_config(cfg).unwrap().encode(&f).unwrap()
}

/// The first PBU of an access unit written by [`Encoder::encode`] (header and payload).
fn first_pbu(au: &[u8]) -> &[u8] {
    let n = u32::from_be_bytes(au[4..8].try_into().unwrap()) as usize;
    &au[8..8 + n]
}

/// `primary` with `alpha_pbu` appended as an auxiliary alpha frame PBU.
fn with_alpha(primary: &[u8], alpha_pbu: &[u8]) -> Vec<u8> {
    let mut au = primary.to_vec();
    au.extend_from_slice(&(alpha_pbu.len() as u32).to_be_bytes());
    au.push(PBU_ALPHA_FRAME);
    au.extend_from_slice(&alpha_pbu[1..]);
    au
}

#[test]
fn auxiliary_alpha_frame_becomes_the_alpha_plane() {
    let primary = encode(Profile::P422_10, 37);
    let luma = encode(Profile::P400_10, 11);
    let expected = decode_frame(&luma).unwrap().y;
    let f = decode_frame(&with_alpha(&primary, first_pbu(&luma))).unwrap();
    assert_eq!(f.alpha.as_deref(), Some(&expected[..]));
    assert_eq!(f.y, decode_frame(&primary).unwrap().y);
}

#[test]
fn truncated_auxiliary_alpha_frame_is_ignored() {
    let primary = encode(Profile::P422_10, 37);
    let luma = encode(Profile::P400_10, 11);
    let pbu = first_pbu(&luma);
    // keep the frame header and drop the tile data: the auxiliary frame cannot be decoded
    for keep in [pbu.len() / 2, pbu.len() - 1] {
        let f = decode_frame(&with_alpha(&primary, &pbu[..keep])).unwrap();
        assert!(f.alpha.is_none(), "kept {keep} of {} bytes", pbu.len());
        assert_eq!(f.y, decode_frame(&primary).unwrap().y);
    }
}
