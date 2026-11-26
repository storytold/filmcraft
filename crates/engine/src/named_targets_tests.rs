//! #65: the registry says which commands take their targets by name (`CommandSpec::named`), and
//! every command that says so really reads those keys. A command that documented `clips` but read
//! only the selection would pass the enabled check and then act on nothing.

use filmcraft_project::{ClipId, ItemId};
use serde_json::{Value, json};

use crate::commands::{NamedKeys, command_specs};
use crate::{Result, Session};

fn demo() -> (Session, ClipId, ItemId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.state.selection.clear();
    s.state.project_selection.clear();
    let first = s.active_sequence().and_then(|q| q.video_tracks.first()?.items.first().map(|i| (i.id, i.item)));
    let (clip, item) = first.unwrap_or_else(|| panic!("the demo project has no clip"));
    (s, clip, item)
}

/// What a call did, as far as a test can tell: whether it worked and the project it left.
fn outcome(s: &Session, r: Result<Value>) -> (bool, String) {
    (r.is_ok(), serde_json::to_string(&*s.project).unwrap_or_default())
}

#[test]
fn the_flag_follows_the_parameter_doc() {
    let spec = |id: &str| command_specs().iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no command {id}"));
    assert!(spec("clip.enable").named.contains(NamedKeys::CLIPS));
    assert!(!spec("clip.enable").named.contains(NamedKeys::CLIP), "a key the command does not document is not a named target");
    assert!(spec("clip.replaceFromBin").named.contains(NamedKeys::ITEM));
    assert!(spec("edit.undo").named.is_none());
}

#[test]
fn named_targets_are_read_by_the_commands_that_declare_them() {
    let (_, clip, item) = demo();
    // Only timeline clips are compared: `item` / `items` name project items, which are not always
    // what the Project panel would have selected (`sequence.closeOthers` takes the sequence to keep).
    let untouched = outcome(&demo().0, Ok(Value::Null)).1;
    let mut checked = 0;
    for spec in command_specs() {
        if spec.named.is_none() || spec.id.starts_with("file.") || spec.id.starts_with("export.") {
            continue;
        }
        let mut cases: Vec<(&str, Value)> = Vec::new();
        // a key documented without `?` is an argument of the command (`masks.select` takes the
        // clip it selects a mask of), not an alternative to the selection
        if spec.named.contains(NamedKeys::CLIPS) && spec.params.contains("\"clips\":[id]?") {
            cases.push(("clips", json!({"clips": [clip.0]})));
        }
        if spec.named.contains(NamedKeys::CLIP) && spec.params.contains("\"clip\":id?") {
            cases.push(("clip", json!({"clip": clip.0})));
        }

        for (key, params) in cases {
            let (mut named, c1, i1) = demo();
            assert_eq!((c1, i1), (clip, item), "the demo project is deterministic");
            let r = named.execute(spec.id, params.clone());
            let by_name = outcome(&named, r);
            // the same target reached through the selection, plus the other parameters as given
            let (mut selected, ..) = demo();
            selected.state.selection = vec![clip];
            let r = selected.execute(spec.id, json!({}));
            let by_selection = outcome(&selected, r);
            if by_selection.0 {
                assert!(by_name.0, "{}: `{key}` is documented but the command only acts on a selection (list it in SELECTION_ONLY)", spec.id);
                // the selection changed the project, so naming the clip changes it too (what exactly
                // may differ: a selection also reaches linked partners, for one)
                if by_selection.1 != untouched {
                    assert_ne!(by_name.1, untouched, "{}: `{key}` is documented but the command ignores it (list it in SELECTION_ONLY)", spec.id);
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 10, "the check ran against {checked} commands only");
}
