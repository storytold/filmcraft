//! `Session::enabled_state`: the reason behind a greyed menu entry.

use super::*;

#[test]
fn a_disabled_command_names_its_reason() {
    let s = Session::default();
    assert_eq!(s.enabled_state("link.editIn"), Err("no sequence is open".to_string()));
    assert!(!s.is_enabled("link.editIn"));
}

#[test]
fn an_enabled_command_has_no_reason() {
    let s = Session::default();
    assert_eq!(s.enabled_state("file.save"), Ok(()));
    assert!(s.is_enabled("file.save"));
}

#[test]
fn an_unknown_command_says_so() {
    let s = Session::default();
    assert_eq!(s.enabled_state("no.such.command"), Err("unknown command".to_string()));
    assert!(!s.is_enabled("no.such.command"));
}

#[test]
fn an_unsaved_project_does_not_disable_edit_in() {
    // The path is checked when the command runs, after the UI has asked where to save.
    let mut s = Session::default();
    s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
    assert!(s.path.is_none());
    assert_eq!(s.enabled_state("link.editIn"), Err("select a video clip in the timeline".to_string()));
}

#[test]
fn edit_in_is_grey_for_a_selection_that_mixes_linked_and_unlinked_clips() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
    let sequence = s.active_sequence().unwrap();
    let clips: Vec<(ClipId, ItemId)> = sequence.video_tracks.iter().flat_map(|track| track.items.iter()).map(|clip| (clip.id, clip.item)).collect();
    assert!(clips.len() >= 2, "the demo project needs two video clips");
    s.state.selection = vec![clips[0].0, clips[1].0];
    assert_eq!(s.enabled_state("link.editIn"), Ok(()));
    s.project_mut().item_mut(clips[0].1).unwrap().metadata.insert("craft.link".to_string(), "{}".to_string());
    assert_eq!(s.enabled_state("link.editIn"), Err("the selection mixes linked and unlinked clips".to_string()));
    s.state.selection = vec![clips[0].0];
    assert_eq!(s.enabled_state("link.editIn"), Ok(()), "a linked clip alone reopens its link");
}
