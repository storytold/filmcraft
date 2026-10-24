use filmcraft_codecs::capabilities::software_decoder_capabilities;
use filmcraft_media::capabilities::*;

#[test]
fn reports_the_three_existing_software_decoders() {
    let records = software_decoder_capabilities().unwrap();
    assert_eq!(records.iter().map(|c| c.codec).collect::<Vec<_>>(), vec![VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1]);
    for record in &records {
        record.validate().unwrap();
        assert_eq!(record.operation, Operation::Decode);
        assert_eq!(record.mode, AccelerationMode::Software);
        assert_eq!(record.status, CapabilityStatus::Available);
        assert!(record.adapter_id.is_none());
        assert_eq!(record.evidence, vec![CapabilityEvidence::ImplementationCoverage]);
        assert!(record.requires_configuration_validation);
        assert!(record.formats.iter().all(|f| f.limits.width.is_none() && f.hdr_modes.is_none()));
    }
    let h264 = records.iter().find(|c| c.codec == VideoCodec::H264).unwrap();
    assert_eq!(h264.formats.len(), 3);
    assert!(h264.formats.iter().all(|f| f.bit_depth == 8 && f.limits.progressive_only == Some(true)));
    let hevc = records.iter().find(|c| c.codec == VideoCodec::Hevc).unwrap();
    assert!(hevc.formats.iter().any(|f| f.profile == VideoProfile::HevcMain10 && f.bit_depth == 10));
    assert!(!hevc.formats.iter().any(|f| f.profile == VideoProfile::HevcMain && f.bit_depth == 10));
    let av1 = records.iter().find(|c| c.codec == VideoCodec::Av1).unwrap();
    assert_eq!(av1.formats.iter().map(|f| f.bit_depth).collect::<Vec<_>>(), vec![8, 10]);
}

fn unrelated_factory(_: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    None
}

#[test]
fn hardware_registry_changes_do_not_change_software_coverage() {
    let before = software_decoder_capabilities().unwrap();
    filmcraft_codecs::register_video_decoder(unrelated_factory);
    assert_eq!(software_decoder_capabilities().unwrap(), before);
}

#[test]
fn upstream_vp8_decoder_remains_available() {
    use filmcraft_isobmff::{CodecConfig, FourCc, SampleEntry};
    let format = FourCc(*b"vp08");
    let entry = SampleEntry::video(format, CodecConfig::Unknown { fourcc: format, raw: Vec::new() }, 16, 16);
    assert!(filmcraft_codecs::software_video_decoder(&entry).is_ok());
    assert!(filmcraft_codecs::make_video_decoder(&entry).is_ok());
}
