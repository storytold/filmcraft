//! Parameter set / slice header / POC parsing over the fixture streams.

use crate::common;

use filmcraft_bitstream::{annexb_nals, unescape_rbsp};
use filmcraft_h264::params::{Pps, Sps};
use filmcraft_h264::slice::{NalHeader, PocState, SliceHeader, nal_type};

/// Parse every NAL of a fixture; returns (sps, number of pictures, pocs in decoding order).
fn parse_stream(name: &str) -> Option<(Sps, usize, Vec<i32>)> {
    let f = common::fixture(name);
    let (h264, _) = common::ensure(f)?;
    let data = std::fs::read(h264).unwrap();
    let mut spss: Vec<Option<Sps>> = vec![None; 32];
    let mut ppss: Vec<Option<Pps>> = vec![None; 256];
    let mut poc_state = PocState::default();
    let mut pocs = Vec::new();
    let mut last: Option<SliceHeader> = None;
    let mut pictures = 0;
    for nal in annexb_nals(&data) {
        let hdr = NalHeader::parse(nal[0]).unwrap();
        let rbsp = unescape_rbsp(&nal[1..]);
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let s = Sps::parse(&rbsp).unwrap();
                let id = s.id as usize;
                spss[id] = Some(s);
            }
            nal_type::PPS => {
                let p = Pps::parse(&rbsp, &spss).unwrap();
                let id = p.id as usize;
                ppss[id] = Some(p);
            }
            nal_type::SLICE | nal_type::IDR => {
                let (sh, _, sps) = SliceHeader::parse(&rbsp, hdr, |id| {
                    let p = ppss[id as usize].as_ref().unwrap();
                    Ok((p, spss[p.sps_id as usize].as_ref().unwrap()))
                })
                .unwrap();
                assert!(sh.header_bits <= rbsp.len() * 8);
                if sh.first_mb_in_slice == 0 {
                    if let Some(prev) = &last {
                        let poc = poc_state.compute(prev, sps);
                        poc_state.update(prev, &poc);
                        pocs.push(poc.frame());
                    }
                    pictures += 1;
                    last = Some(sh);
                }
            }
            _ => {}
        }
    }
    let sps = spss.into_iter().flatten().next().unwrap();
    if let Some(prev) = &last {
        let poc = poc_state.compute(prev, &sps);
        pocs.push(poc.frame());
    }
    Some((sps, pictures, pocs))
}

#[test]
fn baseline_headers() {
    let Some((sps, pictures, pocs)) = parse_stream("baseline_qcif") else { return };
    assert_eq!(sps.profile_idc, 66);
    assert_eq!((sps.width(), sps.height()), (176, 144));
    assert_eq!(pictures, 30);
    // no B-frames: POC strictly increases between IDRs (keyint 15)
    for w in pocs.windows(2) {
        assert!(w[1] > w[0] || w[1] == 0, "{pocs:?}");
    }
}

#[test]
fn cropped_high_profile_headers() {
    let Some((sps, pictures, _)) = parse_stream("crop_1918x1078") else { return };
    assert_eq!(sps.profile_idc, 100);
    assert_eq!(sps.chroma_format_idc, 1);
    assert_eq!(sps.crop_rect(), (0, 0, 1918, 1078));
    assert_eq!(pictures, 5);
    let vui = sps.vui.as_ref().expect("x264 writes a VUI");
    assert!(vui.bitstream_restriction.is_some());
}

#[test]
fn b_frame_pocs_are_a_permutation() {
    let Some((_, pictures, pocs)) = parse_stream("bpyramid") else { return };
    assert_eq!(pictures, 30);
    let mut sorted = pocs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), pocs.len(), "duplicate POCs: {pocs:?}");
    // decoding order differs from output order
    assert_ne!(sorted, pocs);
}

#[test]
fn custom_scaling_matrices_parsed() {
    let Some(f) = common::ensure(common::fixture("cqm_jvt")) else { return };
    let data = std::fs::read(f.0).unwrap();
    let mut spss: Vec<Option<Sps>> = vec![None; 32];
    let mut found = false;
    for nal in annexb_nals(&data) {
        let hdr = NalHeader::parse(nal[0]).unwrap();
        let rbsp = unescape_rbsp(&nal[1..]);
        if hdr.nal_unit_type == nal_type::SPS {
            let s = Sps::parse(&rbsp).unwrap();
            let id = s.id as usize;
            spss[id] = Some(s);
        } else if hdr.nal_unit_type == nal_type::PPS {
            let p = Pps::parse(&rbsp, &spss).unwrap();
            // JVT matrices are the spec defaults: intra 4x4 luma (0,0) = 6
            assert_eq!(p.scaling.m4[0][0], 6);
            assert_eq!(p.scaling.m8[0][0], 6);
            assert!(p.transform_8x8_mode);
            found = true;
        }
    }
    assert!(found);
}
