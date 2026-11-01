//! The page-level keys of `filmcraft-config.json` that are not remote storage. Pure, so it runs
//! natively in `tests/it`.
//!
//! ```json
//! {"startup":"empty"}
//! ```

use serde_json::Value;

/// What the page opens when nothing was recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// The demo project (the default).
    Demo,
    /// An empty session.
    Empty,
}

/// The `startup` key. `Ok(None)` when it is absent; `Err` (to log as a warning, then ignore) for a
/// value other than `"empty"` or `"demo"`, or a file that is not a JSON object.
pub fn parse_startup(bytes: &[u8]) -> Result<Option<Startup>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))?;
    // A top-level array, string or number has no keys: reject it instead of reading it as "no startup".
    let obj = v.as_object().ok_or("config is not a JSON object")?;
    match obj.get("startup") {
        None => Ok(None),
        Some(Value::String(s)) if s == "empty" => Ok(Some(Startup::Empty)),
        Some(Value::String(s)) if s == "demo" => Ok(Some(Startup::Demo)),
        Some(_) => Err("startup must be \"empty\" or \"demo\"".into()),
    }
}

/// Whether to open the demo project: the URL flags win (`?empty`, then `?demo`), then the config.
pub fn open_demo(empty_flag: bool, demo_flag: bool, config: Option<Startup>) -> bool {
    if empty_flag {
        return false;
    }
    demo_flag || config != Some(Startup::Empty)
}
