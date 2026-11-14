//! The top-level parameter keys a command takes, read from its params doc, so a key it doesn't
//! take can be reported instead of silently doing nothing (#4599).

use serde_json::Value;

/// The time keys a command can take in any of these forms, bare or after a prefix (`time`,
/// `startTime`, `introFrames`); see `commands::time_p`.
const TIME: [(&str, &str); 6] =
    [("time", "Time"), ("frame", "Frame"), ("frames", "Frames"), ("seconds", "Seconds"), ("timecode", "Timecode"), ("ticks", "Ticks")];

/// The keys a params doc names, or `None` when the doc isn't only `{..}` objects joined by `|`
/// (prose after it, or an open `…` such as `…settings params`): such a command isn't checked.
/// A key is a quoted name followed by `:` (or `?:`) at the object's top level; `"a|b":` names
/// two, and `"a":x|"b":y` names both.
pub fn doc_keys(doc: &str) -> Option<Vec<&str>> {
    let bytes = doc.as_bytes();
    let (mut keys, mut depth, mut objects, mut i) = (Vec::new(), 0usize, 0usize, 0usize);
    while let Some(&c) = bytes.get(i) {
        match c {
            b'"' if depth > 0 => {
                let end = i + 1 + doc.get(i + 1..)?.find('"')?;
                let name = doc.get(i + 1..end)?;
                let after = doc.get(end + 1..)?.trim_start();
                let after = after.strip_prefix('?').unwrap_or(after).trim_start();
                if depth == 1 && after.starts_with(':') && !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'|') {
                    for key in name.split('|').filter(|k| !k.is_empty()) {
                        if !keys.contains(&key) {
                            keys.push(key);
                        }
                    }
                }
                i = end;
            }
            b'{' | b'[' | b'(' => {
                if depth == 0 {
                    if c != b'{' {
                        return None;
                    }
                    objects += 1;
                }
                depth += 1;
            }
            b'}' | b']' | b')' => depth = depth.checked_sub(1)?,
            _ if depth == 0 && c != b'|' && !c.is_ascii_whitespace() => return None,
            _ if depth == 1 && doc.get(i..).is_some_and(|rest| rest.starts_with('…')) => return None,
            _ => {}
        }
        i += 1;
    }
    (depth == 0 && objects > 0).then_some(keys)
}

/// `Some(prefix)` when `key` is a time key, bare (`seconds`) or after a prefix (`startSeconds`).
fn time_prefix(key: &str) -> Option<&str> {
    TIME.iter().find_map(|(bare, cap)| if key == *bare { Some("") } else { key.strip_suffix(cap).filter(|p| !p.is_empty()) })
}

/// Does a command whose doc names `doc` take `key`? Besides the doc's own keys: `wait`, which
/// frontends add to long commands; `clip` for `clips` and back, as `clips_p` reads either; and
/// every form of a time key it takes (`time_p` reads `time`, `frame`, `seconds` or `timecode`, and
/// `delta` comes with `deltaFrames`).
fn takes(doc: &[&str], key: &str) -> bool {
    if key == "wait" || doc.contains(&key) || (key == "clip" && doc.contains(&"clips")) || (key == "clips" && doc.contains(&"clip")) {
        return true;
    }
    let Some(prefix) = time_prefix(key) else { return false };
    doc.iter().any(|d| time_prefix(d) == Some(prefix) || (!prefix.is_empty() && *d == prefix))
}

/// The top-level keys of `params` that command `id` doesn't take, by its params doc. Empty for an
/// unknown command, a doc that isn't a plain list of keys, or params that aren't an object.
pub fn unknown_params(id: &str, params: &Value) -> Vec<String> {
    let (Some(spec), Some(obj)) = (crate::commands::find(id), params.as_object()) else { return Vec::new() };
    let Some(doc) = doc_keys(spec.params) else { return Vec::new() };
    obj.keys().filter(|k| !takes(&doc, k)).cloned().collect()
}

/// What a frontend says about [`unknown_params`]: the command still runs, and those keys do
/// nothing. A warning and not an error, because a command could still read a key its doc doesn't
/// name; returning it as an error from `Session::execute` instead is a one-line change.
pub fn unknown_params_warning(id: &str, params: &Value) -> Option<String> {
    let unknown = unknown_params(id, params);
    if unknown.is_empty() {
        return None;
    }
    let doc = crate::commands::find(id).and_then(|s| doc_keys(s.params)).unwrap_or_default();
    let list = unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ");
    let accepted = if doc.is_empty() { "no parameters".to_string() } else { doc.join(", ") };
    Some(format!("`{id}` ignores unknown parameter{} {list} (it takes {accepted})", if unknown.len() == 1 { "" } else { "s" }))
}
