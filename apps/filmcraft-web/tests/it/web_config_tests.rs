//! The `startup` key of the web config and the rule for opening the demo.

use crate::web_config::{Startup, open_demo, parse_startup};

/// The two accepted values parse; an absent key is `None`.
#[test]
fn startup_values() {
    assert_eq!(parse_startup(br#"{"startup":"empty"}"#), Ok(Some(Startup::Empty)));
    assert_eq!(parse_startup(br#"{"startup":"demo"}"#), Ok(Some(Startup::Demo)));
    assert_eq!(parse_startup(br#"{"remote":{}}"#), Ok(None));
    assert_eq!(parse_startup(b"{}"), Ok(None));
}

/// Unknown values, wrong types, invalid JSON and non-object JSON are errors the page ignores.
#[test]
fn unknown_or_hostile_startup_is_an_error_to_ignore() {
    for j in [
        r#"{"startup":"Empty"}"#,
        r#"{"startup":""}"#,
        r#"{"startup":1}"#,
        r#"{"startup":null}"#,
        r#"{"startup":["empty"]}"#,
        "not json",
        "",
        "[]",
        "null",
        "\"empty\"",
        "1",
    ] {
        assert!(parse_startup(j.as_bytes()).is_err(), "{j}");
    }
}

/// `?empty` beats `?demo`, which beats the config; the demo is the default.
#[test]
fn demo_rule() {
    assert!(open_demo(false, false, None));
    assert!(open_demo(false, false, Some(Startup::Demo)));
    assert!(!open_demo(false, false, Some(Startup::Empty)));
    assert!(open_demo(false, true, Some(Startup::Empty)), "?demo forces it");
    assert!(!open_demo(true, false, None), "?empty still works");
    assert!(!open_demo(true, true, None), "?empty wins");
}
