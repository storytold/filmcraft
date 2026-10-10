//! Argument parsing: options may appear anywhere; everything else is positional.

use serde_json::{Map, Value};

/// Options that take a value (`--project p`); every other `--x` is a flag.
const VALUED: &[&str] = &[
    "--project",
    "--save-as",
    "--bridge",
    "--format",
    "--scale",
    "--quality",
    "--seconds",
    "--out",
    "--frames",
    "--preset",
    "--range",
    "--start",
    "--end",
    "--data-dir",
    "--settings",
];

#[derive(Debug, Default)]
pub struct Args {
    pub positionals: Vec<String>,
    opts: Vec<(String, Option<String>)>,
}

impl Args {
    pub fn parse(it: impl IntoIterator<Item = String>) -> Self {
        let mut a = Args::default();
        let mut it = it.into_iter();
        while let Some(s) = it.next() {
            if s == "--" {
                a.positionals.extend(it.by_ref());
            } else if let Some((k, v)) = s.split_once('=').filter(|(k, _)| k.starts_with("--")) {
                a.opts.push((k.to_string(), Some(v.to_string())));
            } else if VALUED.contains(&s.as_str()) {
                let v = it.next();
                a.opts.push((s, v));
            } else if s.starts_with("--") && s.len() > 2 {
                a.opts.push((s, None));
            } else {
                a.positionals.push(s);
            }
        }
        a
    }

    pub fn pos(&self, i: usize) -> Option<&str> {
        self.positionals.get(i).map(String::as_str)
    }

    pub fn opt(&self, k: &str) -> Option<&str> {
        self.opts.iter().rev().find(|(n, _)| n == k).and_then(|(_, v)| v.as_deref())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.opts.iter().any(|(n, _)| n == k)
    }

    /// The first option that is not in `allowed`, if any.
    pub fn unknown_opt(&self, allowed: &[&str]) -> Option<&str> {
        self.opts.iter().map(|(n, _)| n.as_str()).find(|n| !allowed.contains(n))
    }

    /// Command params from positionals `from..`: one JSON object, or `key=value` pairs
    /// (dotted keys nest: `color.r=1`).
    pub fn params_from(&self, from: usize) -> Result<Value, String> {
        let rest = self.positionals.get(from..).unwrap_or_default();
        if let [one] = rest
            && one.trim_start().starts_with('{')
        {
            return serde_json::from_str(one).map_err(|e| format!("params JSON: {e}"));
        }
        let mut m = Map::new();
        for kv in rest {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("expected key=value, got `{kv}`"))?;
            insert_dotted(&mut m, k, parse_value(v));
        }
        Ok(Value::Object(m))
    }
}

fn insert_dotted(m: &mut Map<String, Value>, key: &str, v: Value) {
    match key.split_once('.') {
        None => {
            m.insert(key.to_string(), v);
        }
        Some((head, tail)) => {
            let e = m.entry(head.to_string()).or_insert_with(|| Value::Object(Map::new()));
            if !e.is_object() {
                *e = Value::Object(Map::new());
            }
            if let Value::Object(child) = e {
                insert_dotted(child, tail, v);
            }
        }
    }
}

/// JSON when it parses (numbers, bools, null, arrays, objects, quoted strings), else a string.
pub fn parse_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(s: &str) -> Args {
        Args::parse(s.split_whitespace().map(str::to_string))
    }

    #[test]
    fn options_anywhere_and_positionals() {
        let a = args("exec --demo timeline.razor seconds=3.5 --project p.fcproj --save --compact");
        assert_eq!(a.positionals, ["exec", "timeline.razor", "seconds=3.5"]);
        assert_eq!(a.opt("--project"), Some("p.fcproj"));
        assert!(a.flag("--save") && a.flag("--demo") && a.flag("--compact"));
        assert!(!a.flag("--bridge"));
        let b = args("--bridge=127.0.0.1:9876 inspect sequence");
        assert_eq!(b.opt("--bridge"), Some("127.0.0.1:9876"));
        assert_eq!(b.pos(1), Some("sequence"));
    }

    #[test]
    fn key_value_params() {
        let a = args("exec x seconds=3.5 name=Selects on=true ids=[1,2] color.r=1 color.g=0.5 s=\"7\"");
        assert_eq!(a.params_from(2).unwrap(), json!({"seconds": 3.5, "name": "Selects", "on": true, "ids": [1, 2], "color": {"r": 1, "g": 0.5}, "s": "7"}));
        assert!(args("exec x novalue").params_from(2).is_err());
        assert_eq!(args("exec x").params_from(2).unwrap(), json!({}));
    }

    #[test]
    fn json_params() {
        let a = Args::parse(["exec".into(), "x".into(), r#"{"a": {"b": [1]}}"#.into()]);
        assert_eq!(a.params_from(2).unwrap(), json!({"a": {"b": [1]}}));
    }

    #[test]
    fn unknown_opt_reports_first_unlisted_option() {
        let allowed = ["--bridge", "--project"];
        assert_eq!(args("mcp --automation-read-root X").unknown_opt(&allowed), Some("--automation-read-root"));
        assert_eq!(args("mcp --bridge=a --typo=1").unknown_opt(&allowed), Some("--typo"));
        assert_eq!(args("mcp --project p --bridge a").unknown_opt(&allowed), None);
    }

    #[test]
    fn double_dash_ends_options() {
        let a = args("import -- --weird-name.mov a.mov");
        assert_eq!(a.positionals, ["import", "--weird-name.mov", "a.mov"]);
    }
}
