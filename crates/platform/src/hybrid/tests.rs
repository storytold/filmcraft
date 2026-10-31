use filmcraft_codecs::hw::SampleOutput;
use filmcraft_frame::VideoFrame;

use super::*;

fn frame(pts: i64) -> DecodedFrame {
    DecodedFrame { pts, frame: VideoFrame::rgba8(1, 1, vec![0, 0, 0, 255]), draft: false }
}

fn plan(pictures: usize) -> Option<SampleOutput> {
    Some(SampleOutput { pictures, ..Default::default() })
}

fn snapshot(ledger: &Ledger) -> Vec<(Vec<u8>, i64)> {
    ledger.inputs.iter().map(|input| (input.data.clone(), input.pts)).collect()
}

fn idr_plan() -> Option<SampleOutput> {
    Some(SampleOutput { pictures: 1, restart: true, ..Default::default() })
}

#[test]
fn replay_prefix_waits_for_every_output_and_releases_bytes() {
    let payloads = [b"a".as_slice(), b"bb".as_slice(), b"ccc".as_slice()];
    let mut ledger = Ledger::default();
    for (pts, payload) in payloads.iter().enumerate() {
        assert!(ledger.remember(payload, pts as i64, plan(1)));
    }
    ledger.retire_before = payloads.len() - 1;
    assert!(ledger.publish(&[frame(0)], false));
    assert_eq!(ledger.inputs.len(), payloads.len(), "one pending prefix picture prevents retirement");
    assert!(ledger.publish(&[frame(1)], false));
    assert_eq!(snapshot(&ledger), vec![(payloads[2].to_vec(), 2)]);
    assert_eq!(ledger.bytes, payloads[2].len());
    assert_eq!(ledger.retire_before, 0);
}

#[test]
fn non_output_packets_do_not_pin_the_replay_log() {
    // One AUD (no picture) + one IDR per output; explicit intent isolates ledger behavior.
    const PACKETS_PER_PICTURE: usize = 2;
    let (aud, idr) = ([1, 0x09], [1, 0x65]);
    let mut ledger = Ledger::default();
    for picture in 0..replay::MAX_INPUTS {
        let pts = (picture * PACKETS_PER_PICTURE) as i64;
        assert!(ledger.remember(&aud, pts, plan(0)), "non-output input must not exhaust replay history");
        assert!(ledger.remember(&idr, pts + 1, idr_plan()), "restart must remain recoverable");
        assert!(ledger.publish(&[frame(pts + 1)], false));
    }
    assert_eq!(snapshot(&ledger), vec![(idr.to_vec(), (replay::MAX_INPUTS * PACKETS_PER_PICTURE - 1) as i64)]);
    assert_eq!(ledger.bytes, idr.len());
}

#[test]
fn newer_output_does_not_make_a_missing_picture_non_output() {
    let (idr, slice) = ([1, 0x65], [1, 0x41]);
    let mut ledger = Ledger::default();
    assert!(ledger.remember(&idr, 0, idr_plan()));
    assert!(ledger.publish(&[frame(0)], false));
    assert!(ledger.remember(&slice, 1, plan(1)));
    assert!(ledger.remember(&idr, 2, idr_plan()));
    assert!(!ledger.publish(&[frame(2)], false), "newer output cannot escape before recovery");
    assert!(ledger.inputs.iter().any(|input| input.pts == 1), "missing picture stays recoverable");
    assert!(ledger.publish(&[frame(1), frame(2)], true));
}

#[test]
fn restart_input_keeps_its_sample_and_byte_bounds() {
    let idr = [1, 0x65];
    let mut ledger = Ledger::default();
    for pts in 0..replay::MAX_INPUTS {
        assert!(ledger.remember(&idr, pts as i64, idr_plan()));
    }
    assert!(!ledger.remember(&idr, replay::MAX_INPUTS as i64, idr_plan()));
    assert_eq!(ledger.inputs.len(), replay::MAX_INPUTS);
    let oversized = vec![0; replay::MAX_BYTES + 1];
    let mut ledger = Ledger::default();
    assert!(!ledger.remember(&oversized, 0, idr_plan()));
    assert!(ledger.inputs.is_empty());
    assert_eq!(ledger.bytes, 0);
}

#[test]
fn cra_keeps_previous_restart_references_after_output() {
    let (idr, cra) = ([2, 19 << 1, 1], [2, 21 << 1, 1]);
    let mut ledger = Ledger::default();
    assert!(ledger.remember(&idr, 0, idr_plan()));
    assert!(ledger.publish(&[frame(0)], false));
    assert!(ledger.remember(&cra, 1, Some(SampleOutput { pictures: 1, restart: true, keep_previous_restart: true, ..Default::default() })));
    assert_eq!(snapshot(&ledger), vec![(idr.to_vec(), 0), (cra.to_vec(), 1)]);
}

#[test]
fn replay_output_has_a_hard_limit() {
    let mut retained = Vec::new();
    replay::retain(&mut retained, (0..replay::MAX_OUTPUTS as i64).map(frame).collect()).unwrap();
    let err = replay::retain(&mut retained, vec![frame(replay::MAX_OUTPUTS as i64)]).unwrap_err();
    assert!(err.to_string().contains("too many frames"));
    assert_eq!(retained.len(), replay::MAX_OUTPUTS);
}

#[test]
fn equal_pts_do_not_merge_non_display_and_display_input_identity() {
    let mut ledger = Ledger::default();
    assert!(ledger.remember(&[1, 9], 0, plan(0)));
    assert!(ledger.remember(&[1, 0x41], 0, plan(1)));
    assert_eq!(ledger.inputs.len(), 2, "retain both distinct inputs with equal presentation time");
    assert!(ledger.publish(&[frame(0)], false));
    assert!(!ledger.pending(), "one output satisfies only the displayed input");
}

#[test]
fn replay_deduplication_consumes_occurrences_not_timestamp_membership() {
    let mut ledger = Ledger::default();
    assert!(ledger.remember(&[1, 0x41], 0, plan(1)));
    assert!(ledger.remember(&[1, 0x41], 0, plan(1)));
    assert!(ledger.publish(&[frame(0), frame(0)], false));
    let (_, mut tail) = ledger.into_replay(Vec::new()).unwrap();
    assert_eq!(tail.merge(vec![frame(0), frame(0), frame(0)]).unwrap().len(), 1);
    assert_eq!(tail.merge(vec![frame(0)]).unwrap().len(), 1, "new equal-PTS output is not a replay duplicate");
}

#[test]
fn excess_hardware_occurrences_cannot_escape_during_recovery() {
    let mut ledger = Ledger::default();
    assert!(ledger.remember(&[1], 0, plan(1)));
    ledger.submit();
    assert!(ledger.remember(&[2], 1, plan(1)));
    ledger.submit();
    assert!(!ledger.publish(&[frame(0), frame(0)], false), "hardware exceeded one picture's obligation");
    let (_, mut tail) = ledger.into_replay(vec![frame(0), frame(0)]).unwrap();
    let mut out = tail.merge(vec![frame(0)]).unwrap();
    out.extend(tail.finish(vec![frame(1)]).unwrap());
    assert_eq!(out.iter().map(|frame| frame.pts).collect::<Vec<_>>(), [0, 1], "one software occurrence replaces the admissible hardware occurrence");
}
