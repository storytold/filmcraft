//! Which parameter keys a command takes, by its params doc, and the warning for the others (#4599).

use filmcraft_engine::params_doc::doc_keys;
use filmcraft_engine::{command_specs, unknown_params, unknown_params_warning};
use serde_json::{Value, json};

#[test]
fn a_doc_names_its_keys_or_is_not_checked() {
    let rows: &[(&str, Option<&[&str]>)] = &[
        ("{}", Some(&[])),
        (r#"{"name":str,"parent":binId?}"#, Some(&["name", "parent"])),
        (r#"{"left":id,"followLinks"?:bool}"#, Some(&["left", "followLinks"])),
        (r#"{"delta":ticks|"deltaFrames":i64}"#, Some(&["delta", "deltaFrames"])),
        (r#"{"clip":id,"effect":index|id}|{"none":true}"#, Some(&["clip", "effect", "none"])),
        (r#"{"path":str} | {"back":true}"#, Some(&["path", "back"])),
        (r#"{"format":"srt|vtt"? (default: from the extension),"track":id?}"#, Some(&["format", "track"])),
        (r#"{"params":{param:value}?,"reverse":bool?}"#, Some(&["params", "reverse"])),
        (r#"{"effect":"gaussian_blur|Gaussian Blur|…"}"#, Some(&["effect"])),
        (r#"{"preset":str?,"wait":bool?, …settings params}"#, None),
        (r#"{"clips":[id]?} (explicit clips work without a selection)"#, None),
        ("as graphics.template.export", None),
        (r#"{"a":str"#, None),
    ];
    for (doc, want) in rows {
        assert_eq!(doc_keys(doc).as_deref(), *want, "{doc}");
    }
}

#[test]
fn unknown_params_by_the_params_doc() {
    let none: &[&str] = &[];
    let rows: Vec<(&str, &str, Value, &[&str])> = vec![
        ("a typo", "file.newBin", json!({"nme": "Selects"}), &["nme"]),
        ("the doc's keys", "file.newBin", json!({"name": "Selects", "parent": 1}), none),
        ("an extra key", "effects.setParam", json!({"clip": 1, "effect": 0, "param": "x", "value": 1, "bogusKey": 1}), &["bogusKey"]),
        ("an alternative in one entry", "timeline.trim", json!({"clip": 1, "edge": "in", "deltaFrames": 2}), none),
        ("seconds for time", "timeline.razor", json!({"seconds": 3.5}), none),
        ("timecode for time", "timeline.razor", json!({"timecode": "00:00:01:00"}), none),
        ("not a time form", "timeline.razor", json!({"secs": 3.5}), &["secs"]),
        ("a whole-doc alternative", "effects.select", json!({"none": true}), none),
        ("clip for clips", "clip.enable", json!({"clip": 1}), none),
        ("wait", "file.newBin", json!({"name": "x", "wait": true}), none),
        ("an open doc isn't checked", "export.quick", json!({"anything": 1}), none),
        ("params that aren't an object", "file.newBin", json!("x"), none),
        ("an unknown command", "no.such.command", json!({"x": 1}), none),
        // keys their docs used to leave out, though the commands read them
        ("generator", "file.newColorMatte", json!({"color": "#ff0000", "seconds": 2, "width": 640, "height": 360, "name": "Red", "bin": 1}), none),
        ("generator", "file.newCountingLeader", json!({"seconds": 2, "width": 640, "height": 360, "name": "Leader"}), none),
        ("generator", "file.newBarsAndTone", json!({"seconds": 2, "width": 640, "height": 360, "name": "Bars"}), none),
        ("generator", "file.newBlackVideo", json!({"seconds": 2, "width": 640, "height": 360, "name": "Black"}), none),
        ("generator", "file.newTransparentVideo", json!({"seconds": 2, "width": 640, "height": 360, "name": "Clear"}), none),
        (
            "transition",
            "effects.apply",
            json!({"clip": 1, "effect": "cross_dissolve", "edge": "in", "frames": 12, "align": "center", "params": {}, "reverse": true}),
            none,
        ),
        (
            "export",
            "file.exportMedia",
            json!({"path": "a.mp4", "gpuRendering": "off", "maxThreads": 2, "maxBitrateKbps": 9000, "keyframeDistance": 48, "sdr": true}),
            none,
        ),
        (
            "vertical text",
            "graphics.newVerticalText",
            json!({"text": "A", "newClip": true, "box": [10, 10], "font": "x", "fontStyle": "Bold", "track": 1}),
            none,
        ),
        ("template", "file.newProjectFromTemplate", json!({"template": "t", "force": true}), none),
    ];
    for (what, id, params, want) in rows {
        assert_eq!(unknown_params(id, &params), *want, "{what}: {id} {params}");
    }
}

#[test]
fn the_warning_names_the_keys_and_what_the_command_takes() {
    assert_eq!(unknown_params_warning("file.newBin", &json!({"name": "x"})), None);
    assert_eq!(
        unknown_params_warning("file.newBin", &json!({"nme": "x"})).as_deref(),
        Some("`file.newBin` ignores unknown parameter `nme` (it takes name, parent)")
    );
    assert_eq!(
        unknown_params_warning("edit.cut", &json!({"clips": [1]})).as_deref(),
        Some("`edit.cut` ignores unknown parameter `clips` (it takes no parameters)")
    );
}

/// Almost every command is checked: a parser change that stops reading most docs would
/// silence the warning everywhere.
#[test]
fn most_params_docs_are_checked() {
    let specs = command_specs();
    let checked = specs.iter().filter(|s| doc_keys(s.params).is_some()).count();
    assert!(checked * 100 >= specs.len() * 95, "{checked} of {} params docs parse", specs.len());
}
