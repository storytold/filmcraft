use filmcraft_media::capabilities::*;
use std::panic::catch_unwind;

fn row() -> CodecCapability {
    CodecCapability::software(VideoCodec::H264, Operation::Encode, "filmcraft-h264enc", vec![FormatCapability::new(VideoProfile::H264High, 8)])
}
fn snapshot() -> CapabilitySnapshot {
    let mut s = CapabilitySnapshot::new(7);
    s.capabilities.push(row());
    s
}
fn gpu(id: &str, software: bool) -> GpuInfo {
    GpuInfo { id: id.into(), name: None, vendor_id: None, device_id: None, driver: None, is_software: software, kind: GpuKind::Unknown }
}

#[test]
fn round_trip_preserves_unknown_limits_and_evidence() {
    let s = snapshot();
    let encoded = encode_snapshot(&s).unwrap();
    assert_eq!(decode_snapshot(&encoded).unwrap(), s);
    let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let format = &value["capabilities"][0]["formats"][0];
    assert!(format["limits"]["width"].is_null());
    assert!(format["hdr_modes"].is_null());
    assert!(format["encoder_controls"].is_null());
    assert_eq!(value["capabilities"][0]["status"], "available");
    assert_eq!(value["capabilities"][0]["evidence"][0], "implementation_coverage");
    assert_eq!(value["capabilities"][0]["requires_configuration_validation"], true);
    let mut empty_known_set = snapshot();
    empty_known_set.capabilities.get_mut(0).unwrap().formats.get_mut(0).unwrap().hdr_modes = Some(Vec::new());
    assert_ne!(decode_snapshot(&encode_snapshot(&empty_known_set).unwrap()).unwrap(), s);
}

#[test]
fn hostile_json_is_rejected_without_panicking() {
    let base = serde_json::to_value(snapshot()).unwrap();
    let mut cases = Vec::new();
    let mut v = base.clone();
    v["schema_version"] = 99.into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = base.clone();
    v["capabilities"][0]["codec"] = "unknown_codec".into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = base.clone();
    v["capabilities"][0]["formats"][0]["bit_depth"] = (-1).into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = base.clone();
    v["generation"] = u64::MAX.to_string().into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = base;
    v["unexpected"] = true.into();
    cases.push(serde_json::to_vec(&v).unwrap());
    cases.extend([b"null".to_vec(), b"{".to_vec(), b"{\"schema_version\":NaN}".to_vec()]);
    for bytes in cases {
        let result = catch_unwind(|| decode_snapshot(&bytes));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    let oversized = vec![b' '; MAX_SNAPSHOT_BYTES + 1];
    assert!(decode_snapshot(&oversized).is_err());
}

#[test]
fn rejects_invalid_ranges_rates_profiles_and_claims() {
    for range in [InclusiveRange { min: 0, max: 16, step: 1 }, InclusiveRange { min: 16, max: 2, step: 1 }, InclusiveRange { min: 2, max: 16, step: 0 }] {
        let limits = CapabilityLimits { width: Some(range), ..Default::default() };
        assert!(catch_unwind(|| limits.validate()).unwrap().is_err());
    }
    for rate in [RationalRate { num: 0, den: 1 }, RationalRate { num: 1, den: 0 }] {
        let limits = CapabilityLimits { max_frame_rate: Some(rate), ..Default::default() };
        assert!(limits.validate().is_err());
    }
    let limits = CapabilityLimits { max_frame_rate: Some(RationalRate { num: u32::MAX, den: u32::MAX }), ..Default::default() };
    assert!(limits.validate().is_ok());
    let mut c = row();
    c.codec = VideoCodec::Hevc;
    assert!(c.validate().is_err());
    let mut c = row();
    c.formats.get_mut(0).unwrap().bit_depth = 10;
    assert!(c.validate().is_err());
    let mut c = row();
    c.formats.get_mut(0).unwrap().chroma = ChromaSampling::C444;
    assert!(c.validate().is_err());
    let mut c = row();
    c.evidence.clear();
    assert!(c.validate().is_err());
    let mut c = row();
    c.requires_configuration_validation = false;
    assert!(c.validate().is_err());
    let mut c = row();
    c.status = CapabilityStatus::NotImplemented;
    c.reason = Some("missing encoder".into());
    assert!(c.validate().is_err());
    let mut c = row();
    c.mode = AccelerationMode::Hardware;
    assert!(c.validate().is_err());
    let mut c = row();
    c.status = CapabilityStatus::QueryFailed;
    assert!(c.validate().is_err());
    let mut c = row();
    c.operation = Operation::Decode;
    c.formats.get_mut(0).unwrap().encoder_controls = Some(vec![EncoderControl::Cbr]);
    assert!(c.validate().is_err());
}

#[test]
fn enforces_inventory_associations_and_record_bounds() {
    let mut s = snapshot();
    s.capabilities.get_mut(0).unwrap().adapter_id = Some("dxgi:01".into());
    assert!(s.validate().is_err());
    s.capabilities.get_mut(0).unwrap().mode = AccelerationMode::Hardware;
    assert!(s.validate().is_err());
    s.gpus.push(gpu("dxgi:01", true));
    assert!(s.validate().is_err());
    s.gpus.get_mut(0).unwrap().is_software = false;
    assert!(s.validate().is_ok());
    s.gpus.push(gpu("dxgi:01", false));
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.capabilities.push(row());
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.gpus = (0..=MAX_GPUS).map(|i| gpu(&format!("dxgi:{i:016x}"), false)).collect();
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.capabilities = vec![row(); MAX_CAPABILITIES + 1];
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.cpu = Some(CpuInfo { model: None, architecture: None, logical_processors: Some(0) });
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.cpu = Some(CpuInfo { model: Some("🎥".repeat(128)), architecture: None, logical_processors: Some(4) });
    assert!(s.validate().is_ok());
    s.cpu.as_mut().unwrap().model = Some("🎥".repeat(129));
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.truncated = true;
    assert!(s.validate().is_err());
    s.issues.push(DiscoveryIssue { backend_id: None, adapter_id: None, reason: "record cap reached".into() });
    s.discovery = DiscoveryState::Partial;
    assert!(s.validate().is_ok());
    for state in [DiscoveryState::Complete, DiscoveryState::NotStarted, DiscoveryState::Checking] {
        s.discovery = state;
        assert!(s.validate().is_err());
    }
    let mut s = snapshot();
    let mut software_gpu = gpu("dxgi:02", true);
    software_gpu.kind = GpuKind::Discrete;
    s.gpus.push(software_gpu);
    assert!(s.validate().is_err());
}

#[test]
fn validates_duplicate_formats_optional_sets_and_issues() {
    let mut c = row();
    c.formats.push(FormatCapability::new(VideoProfile::H264High, 8));
    assert!(c.validate().is_err());
    let mut c = row();
    c.evidence.push(CapabilityEvidence::ImplementationCoverage);
    assert!(c.validate().is_err());
    let mut c = row();
    c.formats.get_mut(0).unwrap().hdr_modes = Some(vec![HdrMode::Pq, HdrMode::Pq]);
    assert!(c.validate().is_err());
    let mut c = row();
    c.formats.get_mut(0).unwrap().encoder_controls = Some(vec![EncoderControl::Cbr, EncoderControl::Cbr]);
    assert!(c.validate().is_err());
    let mut c = row();
    c.backend_id = "bad backend".into();
    assert!(c.validate().is_err());
    let mut c = row();
    c.reason = Some("x".repeat(MAX_REASON_BYTES + 1));
    assert!(c.validate().is_err());
    let mut c = row();
    c.backend_id = "x".repeat(MAX_ID_BYTES + 1);
    assert!(c.validate().is_err());
    let mut c = row();
    c.formats = vec![FormatCapability::new(VideoProfile::H264High, 8); MAX_FORMATS + 1];
    assert!(c.validate().unwrap_err().to_string().contains("format count"));
    let mut s = snapshot();
    s.discovery = DiscoveryState::Failed;
    assert!(s.validate().is_err());
    s.issues.push(DiscoveryIssue { backend_id: None, adapter_id: None, reason: "worker failed".into() });
    assert!(s.validate().is_ok());
    s.issues.get_mut(0).unwrap().reason = " ".into();
    assert!(s.validate().is_err());
    let mut s = snapshot();
    s.issues = vec![DiscoveryIssue { backend_id: None, adapter_id: None, reason: "failure".into() }; MAX_ISSUES + 1];
    assert!(s.validate().is_err());
}

#[test]
fn escaped_json_output_stays_bounded() {
    let mut s = CapabilitySnapshot::new(1);
    s.capabilities = (0..MAX_CAPABILITIES)
        .map(|i| {
            let mut c = row();
            c.backend_id = format!("backend-{i}");
            c.reason = Some("\0".repeat(MAX_REASON_BYTES));
            c
        })
        .collect();
    assert!(s.validate().is_ok());
    assert!(encode_snapshot(&s).is_err());
}

#[test]
fn snapshot_truncations_and_bit_flips_never_panic() {
    let bytes = serde_json::to_vec(&snapshot()).unwrap();
    for end in 0..bytes.len() {
        let result = catch_unwind(|| decode_snapshot(&bytes[..end]));
        assert!(result.is_ok(), "truncation at {end} panicked");
        assert!(result.unwrap().is_err(), "truncation at {end} was accepted");
    }
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut changed = bytes.clone();
            *changed.get_mut(index).unwrap() ^= 1u8 << bit;
            let result = catch_unwind(|| decode_snapshot(&changed));
            assert!(result.is_ok(), "bit {bit} at {index} panicked");
            if let Ok(snapshot) = result.unwrap() {
                snapshot.validate().unwrap();
            }
        }
    }
}
