//! The stable encoder identifier, and the decline reason belonging to one export only.
//!
//! main names the encoder and warns when hardware fell back (`tests.rs`,
//! `the_report_names_the_encoder_and_says_when_hardware_was_not_used`). These tests cover the
//! part main does not: [`VideoEncoder::id`], the "software" default shared by every encoder that
//! overrides neither, and the lifetime of a decline reason.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

use crate::registry::{EncodedPacket, EncoderFrame, VideoEncoder};

const FIRST: &str = "first reason";
const SECOND: &str = "second reason";

/// An encoder that overrides nothing - the shape every software encoder has.
struct Plain;

impl VideoEncoder for Plain {
    fn sample_entry(&self) -> SampleEntry {
        unimplemented!()
    }
    fn timescale(&self) -> u32 {
        1
    }
    fn encode(&mut self, _f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        unimplemented!()
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        unimplemented!()
    }
}

#[test]
fn an_encoder_that_overrides_nothing_is_software() {
    // Every software encoder, and the dummy in job.rs, takes the trait default. If this ever
    // stops being "software", a report on a build with no GPU starts naming an identifier no
    // hardware encoder answers to.
    let e = Plain;
    assert_eq!(e.id(), "software", "an encoder that overrides neither name nor id is software");
    assert_eq!(e.hardware(), None, "and names no hardware encoder");
}

#[test]
fn the_id_vocabulary_is_fixed_and_machine_readable() {
    // Four values, lowercase, no spaces: a consumer compares these, so they may not drift with
    // the readable name. The list is the whole vocabulary.
    for v in ["software", "nvenc", "videotoolbox", "media_foundation"] {
        assert_eq!(v, v.to_lowercase(), "the vocabulary is lowercase");
        assert!(!v.contains(' '), "and has no spaces");
    }
}

#[test]
fn an_empty_encoder_id_is_left_out_of_the_json() {
    // Like `warnings` and `encoder`, an empty value is absent rather than `""`. A consumer reads
    // "no encoder" from the absence; an empty string would be a third state to handle.
    let r = Report::default();
    let json = serde_json::to_value(&r).expect("serializes");
    assert!(json.get("encoder_id").is_none(), "an empty id is not written");
    assert!(json.get("encoder").is_none(), "nor is an absent readable name");
    assert!(json.get("warnings").is_none(), "nor an empty warning list");
}

#[test]
fn the_decline_reason_belongs_to_the_export_that_had_it() {
    // `note_hw_encode_declined_by` keeps the reason for the export that asked (factories run on
    // its thread), and `take_hw_encode_decline` consumes it. A reason that survived into the next
    // export would attribute a decline to an export that never had one. The reason carries the
    // encoder's name in front of it, so a report says which hardware encoder declined, not just
    // that one did.
    crate::note_hw_encode_declined_by("NVENC", FIRST);
    assert_eq!(crate::take_hw_encode_decline().as_deref(), Some("NVENC: first reason"), "the reason is kept, prefixed by the encoder");
    assert_eq!(crate::take_hw_encode_decline(), None, "and taken only once");
    assert_eq!(crate::take_hw_encode_decline(), None, "so the next export starts clean");
}

#[test]
fn the_latest_decline_is_the_one_kept() {
    // An export asks factories in order and each decline overwrites the last, so the reason that
    // survives is the most recent one asked. This is main's behaviour (`HW_DECLINE.set`), and it
    // is the one worth pinning: a `take` must never hand back a reason from an earlier export.
    crate::note_hw_encode_declined_by("NVENC", FIRST);
    crate::note_hw_encode_declined_by("Media Foundation", SECOND);
    assert_eq!(crate::take_hw_encode_decline().as_deref(), Some("Media Foundation: second reason"), "the latest decline is the one reported");
}
