//! A build without the `h264`, `hevc`, `aac` and `prores` features: those streams fail with an
//! error that names the missing feature, the other codecs still decode, and a registered decoder
//! takes the streams the build leaves out.
//!
//! `cargo test -p filmcraft-codecs --no-default-features --test without_licensed_codecs`

#![cfg(not(any(feature = "h264", feature = "hevc", feature = "aac", feature = "prores")))]

use filmcraft_codecs::audio::PacketDecoder;
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_codecs::video::{DecodedFrame, VideoDecoder, h264_annexb, hevc_annexb};
use filmcraft_codecs::{CodecError, make_video_decoder, register_video_decoder, software_video_decoder};
use filmcraft_isobmff::{AvcConfig, CodecConfig, FourCc, HevcConfig, SampleEntry};

fn avcc() -> AvcConfig {
    AvcConfig::new(vec![vec![0x67, 0x42, 0x00, 0x1e]], vec![vec![0x68, 0xce, 0x38, 0x80]], 4)
}

fn avc() -> SampleEntry {
    SampleEntry::avc(avcc(), 64, 32)
}

fn names_feature(r: Result<Box<dyn VideoDecoder>, CodecError>, feature: &str) {
    match r {
        Err(CodecError::Unsupported(msg)) => assert!(msg.contains(&format!("`{feature}`")), "{msg}"),
        Err(e) => panic!("expected an unsupported error naming `{feature}`, got {e}"),
        Ok(d) => panic!("expected an unsupported error naming `{feature}`, got the decoder {}", d.name()),
    }
}

#[test]
fn left_out_codecs_name_their_feature() {
    names_feature(software_video_decoder(&avc()), "h264");
    names_feature(software_video_decoder(&SampleEntry::hevc(HevcConfig::default(), 64, 32)), "hevc");
    names_feature(software_video_decoder(&SampleEntry::prores(FourCc(*b"apch"), 64, 32)), "prores");
    names_feature(h264_annexb(), "h264");
    names_feature(hevc_annexb(), "hevc");
    let Err(CodecError::Unsupported(msg)) = NalStreamInfo::from_avcc(&avcc().to_bytes()) else { panic!("H.264 stream info without the feature") };
    assert!(msg.contains("`h264`"), "{msg}");
    // AAC-LC, 44.1 kHz stereo
    let Err(CodecError::Unsupported(msg)) = PacketDecoder::aac(&[0x12, 0x10], 44_100) else { panic!("AAC decoder without the feature") };
    assert!(msg.contains("`aac`"), "{msg}");
}

#[test]
fn other_codecs_still_decode() {
    assert!(software_video_decoder(&SampleEntry::jpeg(64, 32)).is_ok());
    assert!(software_video_decoder(&SampleEntry::dnx(FourCc(*b"AVdh"), 64, 32)).is_ok());
}

struct Stub;

impl VideoDecoder for Stub {
    fn decode(&mut self, _sample: &[u8], _pts: i64) -> filmcraft_codecs::Result<Vec<DecodedFrame>> {
        Ok(Vec::new())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "platform H.264"
    }
}

fn platform_h264(e: &SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Avc(_)).then(|| Ok(Box::new(Stub) as Box<dyn VideoDecoder>))
}

#[test]
fn a_registered_decoder_takes_a_left_out_codec() {
    register_video_decoder(platform_h264);
    let d = make_video_decoder(&avc()).expect("the registered decoder");
    assert_eq!(d.name(), "platform H.264");
}
