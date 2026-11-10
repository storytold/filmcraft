//! The export result names the encoder that wrote the file, and says why a hardware encoder
//! declined (#3952, #4339).
//!
//! A decline is not an error: the software encoder takes the export over, and the result has to
//! say which encoder wrote the file - the same bytes under a different name are not the same
//! file. The encoder registry is process-wide, so the hardware encoders of these tests decline
//! only the exports whose path carries their marker, and leave every other export alone.

use super::*;

/// The path fragment that marks the exports the hardware encoders decline.
const DECLINED: &str = "hw-declined";
/// The path fragment that marks the two-pass export whose second pass is declined.
const TWO_PASS: &str = "two-pass-declined";
/// Why the first hardware encoder declined.
const FIRST: &str = "the hardware encoder declined this export";
/// Why the second one declined (the first reason is the one kept).
const SECOND: &str = "the second hardware encoder declined as well";
/// Why the hardware encoder declined the second pass of the two-pass export.
const SECOND_PASS: &str = "the hardware encoder declined the second pass";

/// A hardware encoder that declines the export, saying why.
fn declining(format: Format, _w: u32, _h: u32, _rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 || !s.path.contains(DECLINED) {
        return None;
    }
    note_hw_encode_declined_with(FIRST);
    Some(Err(ExportError::Encode(FIRST.into())))
}

/// A second hardware encoder, declining the same export: the first reason is the one kept.
fn also_declining(format: Format, _w: u32, _h: u32, _rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 || !s.path.contains(DECLINED) {
        return None;
    }
    note_hw_encode_declined_with(SECOND);
    Some(Err(ExportError::Encode(SECOND.into())))
}

/// A hardware encoder that declines the second pass of a two-pass export only: the export has to
/// pick the reason up when it makes its final encoder.
fn declining_second_pass(format: Format, _w: u32, _h: u32, _rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::H264 || !s.path.contains(TWO_PASS) || !matches!(&s.h264_pass, H264Pass::Second(_)) {
        return None;
    }
    note_hw_encode_declined_with(SECOND_PASS);
    Some(Err(ExportError::Encode(SECOND_PASS.into())))
}

/// The declining factories are registered once, in a fixed order: the registry is process-wide, and
/// the first reason is only the first one if the order is the same whichever test registers them
/// first.
fn register_declining_factories() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // `register_encoder` puts a factory in front of the others, so the one registered last is
        // the first one tried.
        register_encoder(declining_second_pass);
        register_encoder(also_declining);
        register_encoder(declining);
    });
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("fc-encoder-name-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// A hardware encoder that declines leaves the software encoder to write the file: the result says
/// which encoder wrote it and why the hardware one declined (#3952).
#[test]
fn a_declined_export_names_the_software_encoder_and_the_reason() {
    register_declining_factories();
    let (p, seq, m) = crate::tests::project();
    let prog = Progress::default();
    let r = export(&p, seq, &ExportSettings { format: Format::H264, path: tmp("hw-declined-1.mp4"), ..Default::default() }, &m, &prog).unwrap();
    assert_eq!(r.encoder, "software H.264", "the software encoder wrote the file");
    assert_eq!(r.declined.as_deref(), Some(FIRST), "the result says why the hardware encoder declined");
    // the status the user reads says it too, without reading the result
    let status = prog.status.lock().unwrap_or_else(|e| e.into_inner());
    assert!(status.contains("software H.264"), "{status}");
    assert!(status.contains(FIRST), "{status}");
}

/// The decline reason belongs to the export that had it: the next export, with no hardware
/// encoder in the way, reports no decline (#4339).
#[test]
fn a_decline_does_not_leak_into_the_next_export() {
    register_declining_factories();
    let (p, seq, m) = crate::tests::project();
    let declined =
        export(&p, seq, &ExportSettings { format: Format::H264, path: tmp("hw-declined-2.mp4"), ..Default::default() }, &m, &Progress::default()).unwrap();
    assert_eq!(declined.declined.as_deref(), Some(FIRST));
    let plain =
        export(&p, seq, &ExportSettings { format: Format::H264, path: tmp("hw-accepted.mp4"), ..Default::default() }, &m, &Progress::default()).unwrap();
    assert_eq!(plain.declined, None, "the reason belongs to the export that had it");
    assert_eq!(plain.encoder, "software H.264");
}

/// Two hardware encoders decline the same export: the first reason is the one kept (#4339).
#[test]
fn two_declines_in_one_export_keep_the_first_reason() {
    register_declining_factories();
    let (p, seq, m) = crate::tests::project();
    let r =
        export(&p, seq, &ExportSettings { format: Format::H264, path: tmp("hw-declined-twice.mp4"), ..Default::default() }, &m, &Progress::default()).unwrap();
    assert_eq!(r.declined.as_deref(), Some(FIRST), "the second reason does not replace the first");
}

/// The reason is kept until it is cleared: the first one wins, and the next export forgets it.
#[test]
fn the_first_decline_reason_is_the_one_kept() {
    clear_hw_decline_reason();
    note_hw_encode_declined_with(FIRST);
    note_hw_encode_declined_with(SECOND);
    assert_eq!(hw_decline_reason().as_deref(), Some(FIRST));
    clear_hw_decline_reason();
    assert_eq!(hw_decline_reason(), None, "the reason is forgotten for the next export");
}

/// A two-pass export makes its encoder twice: the final encoder is the one the result reports,
/// with the reason its decline gave (#4339).
#[test]
fn a_two_pass_export_reports_the_final_encoder() {
    register_declining_factories();
    let (p, seq, m) = crate::tests::project();
    let r = export(
        &p,
        seq,
        &ExportSettings {
            format: Format::H264,
            path: tmp("two-pass-declined.mp4"),
            bitrate_mode: BitrateMode::Vbr2Pass,
            bitrate_kbps: 2000,
            include_audio: false,
            ..Default::default()
        },
        &m,
        &Progress::default(),
    )
    .unwrap();
    assert_eq!(r.encoder, "software H.264", "the software encoder wrote the file");
    assert_eq!(r.declined.as_deref(), Some(SECOND_PASS), "the second pass declined, and the result says so");
}

/// A software encoder with no name of its own reports the software default (#4339).
#[test]
fn a_software_encoder_reports_the_software_default() {
    let enc = crate::encoders::mjpeg_factory(Format::Mjpeg, 16, 16, FrameRate::FPS_24, &ExportSettings::default()).unwrap().unwrap();
    assert_eq!(enc.name(), "software");
    assert_eq!(enc.id(), "software", "and the stable identifier");
}

/// The result carries the stable identifier beside the readable name, so a consumer compares
/// `encoder_id` and a person reads `encoder` (#4339).
#[test]
fn the_result_carries_the_stable_encoder_id() {
    let (p, seq, m) = crate::tests::project();
    let r = export(&p, seq, &ExportSettings { format: Format::H264, path: tmp("encoder-id.mp4"), ..Default::default() }, &m, &Progress::default()).unwrap();
    assert_eq!(r.encoder, "software H.264", "the readable text");
    assert_eq!(r.encoder_id, "software", "the stable identifier");
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["encoder_id"], "software");
    let back: Report = serde_json::from_value(v).unwrap();
    assert_eq!(back.encoder_id, "software");
}

/// The result the CLI and MCP read: the new fields are there when they are set, and back again
/// when it is read (#4339).
#[test]
fn the_report_serializes_the_encoder_and_the_decline() {
    let r = Report {
        path: "a.mp4".into(),
        frames: 24,
        seconds: 1.0,
        bytes: 1024,
        render_fps: 24.0,
        extra_files: Vec::new(),
        encoder: "NVENC H.264".into(),
        encoder_id: "nvenc".into(),
        declined: Some("no hardware session".into()),
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["encoder"], "NVENC H.264");
    assert_eq!(v["encoder_id"], "nvenc");
    assert_eq!(v["declined"], "no hardware session");
    let back: Report = serde_json::from_value(v).unwrap();
    assert_eq!(back.path, r.path);
    assert_eq!(back.frames, r.frames);
    assert_eq!(back.encoder, r.encoder);
    assert_eq!(back.encoder_id, r.encoder_id);
    assert_eq!(back.declined, r.declined);
    assert_eq!(back.extra_files, r.extra_files);
}

/// An export with no hardware encoder and no decline leaves the fields out: the JSON a consumer
/// reads says nothing it does not know (#4339).
#[test]
fn an_empty_encoder_and_decline_are_left_out() {
    let r = Report {
        path: "a.mp4".into(),
        frames: 0,
        seconds: 0.0,
        bytes: 0,
        render_fps: 0.0,
        extra_files: Vec::new(),
        encoder: String::new(),
        encoder_id: String::new(),
        declined: None,
    };
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("encoder").is_none(), "{v}");
    assert!(v.get("encoder_id").is_none(), "{v}");
    assert!(v.get("declined").is_none(), "{v}");
    assert!(v.get("extra_files").is_none(), "{v}");
}

/// A result written before the encoder was named still reads (#4339).
#[test]
fn a_report_written_before_the_encoder_field_still_reads() {
    let legacy = serde_json::json!({"path": "a.mp4", "frames": 24, "seconds": 1.0, "bytes": 1024, "render_fps": 24.0});
    let r: Report = serde_json::from_value(legacy).unwrap();
    assert_eq!(r.encoder, "");
    assert_eq!(r.encoder_id, "");
    assert_eq!(r.declined, None);
    assert!(r.extra_files.is_empty());
}
