use filmcraft_export::capabilities::software_encoder_capabilities;
use filmcraft_media::capabilities::*;

#[test]
fn reports_h264_and_the_two_missing_software_encoders() {
    let records = software_encoder_capabilities().unwrap();
    assert_eq!(records.iter().map(|c| c.codec).collect::<Vec<_>>(), vec![VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1]);
    for record in &records {
        record.validate().unwrap();
        assert_eq!(record.mode, AccelerationMode::Software);
        assert_eq!(record.operation, Operation::Encode);
        assert!(record.adapter_id.is_none());
    }
    let h264 = records.iter().find(|c| c.codec == VideoCodec::H264).unwrap();
    assert!(filmcraft_export::Format::H264.has_builtin_encoder());
    assert!(!filmcraft_export::Format::Hevc.has_builtin_encoder());
    assert_eq!(h264.status, CapabilityStatus::Available);
    assert_eq!(h264.evidence, vec![CapabilityEvidence::ImplementationCoverage]);
    assert!(h264.requires_configuration_validation);
    assert_eq!(h264.formats.len(), 3);
    for format in &h264.formats {
        assert_eq!(format.bit_depth, 8);
        assert_eq!(format.chroma, ChromaSampling::C420);
        assert_eq!(format.limits.width, Some(InclusiveRange { min: 2, max: 16384, step: 2 }));
        assert_eq!(format.limits.height, Some(InclusiveRange { min: 2, max: 16384, step: 2 }));
        assert_eq!(format.limits.max_frame_rate, Some(RationalRate { num: 1000, den: 1 }));
        assert!(format.hdr_modes.is_none());
        assert_eq!(
            format.encoder_controls,
            Some(vec![EncoderControl::Cbr, EncoderControl::VbrOnePass, EncoderControl::VbrTwoPass, EncoderControl::KeyframeInterval,])
        );
    }
    for codec in [VideoCodec::Hevc, VideoCodec::Av1] {
        let record = records.iter().find(|c| c.codec == codec).unwrap();
        assert_eq!(record.status, CapabilityStatus::NotImplemented);
        assert!(record.formats.is_empty());
        assert!(record.evidence.is_empty());
        assert!(!record.requires_configuration_validation);
        assert!(record.reason.as_deref().is_some_and(|r| !r.is_empty()));
    }
}

fn fake_hardware_available() -> bool {
    true
}

#[test]
fn hardware_hevc_availability_does_not_create_cpu_support() {
    let before = software_encoder_capabilities().unwrap();
    filmcraft_export::register_format_probe(filmcraft_export::Format::Hevc, fake_hardware_available);
    assert!(filmcraft_export::available(filmcraft_export::Format::Hevc));
    assert_eq!(software_encoder_capabilities().unwrap(), before);
}

#[test]
fn six_software_records_form_a_valid_portable_snapshot() {
    let mut snapshot = CapabilitySnapshot::new(0);
    snapshot.capabilities = filmcraft_codecs::capabilities::software_decoder_capabilities().unwrap();
    snapshot.capabilities.extend(software_encoder_capabilities().unwrap());
    assert_eq!(snapshot.capabilities.len(), 6);
    assert_eq!(snapshot.discovery, DiscoveryState::NotStarted);
    assert!(snapshot.cpu.is_none());
    assert!(snapshot.gpus.is_empty());
    assert_eq!(decode_snapshot(&encode_snapshot(&snapshot).unwrap()).unwrap(), snapshot);
    assert_eq!(filmcraft_export::HardwareEncoding::default(), filmcraft_export::HardwareEncoding::Off);
}
