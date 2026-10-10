//! The block-style subset of YAML that NeMo writes for `model_config.yaml` (OmegaConf dumps):
//! nested `key: value` maps by indentation, `- item` sequences (also at the parent key's
//! indentation) and `[a, b]` flow sequences of scalars. Values are kept as strings under dotted
//! paths (`encoder.d_model`); sequences of maps and block scalars are skipped, since the model
//! settings we read never use them.
//!
//! Hostile input: the text size and line count are capped, no recursion.

use crate::SpeechError;

const MAX_TEXT: usize = 16 << 20;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Scalar(String),
    List(Vec<String>),
}

/// A parsed configuration: dotted path → scalar or list of scalars.
#[derive(Clone, Debug, Default)]
pub struct Yaml {
    entries: Vec<(String, Value)>,
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"'))) {
        return s.get(1..s.len() - 1).unwrap_or_default().to_string();
    }
    s.to_string()
}

/// Strip a ` #` comment outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return line.get(..i).unwrap_or(line),
            None => {}
        }
        prev = c;
    }
    line
}

/// Split `key: value` (the colon must be followed by a space or the end).
fn key_value(s: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ':' => {
                let rest = s.get(i + 1..).unwrap_or_default();
                if rest.is_empty() || rest.starts_with(' ') {
                    return Some((s.get(..i)?.trim(), rest.trim()));
                }
            }
            None => {}
        }
    }
    None
}

fn flow_list(s: &str) -> Option<Vec<String>> {
    let inner = s.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(inner.split(',').map(unquote).collect())
}

impl Yaml {
    pub fn parse(text: &str) -> Result<Self, SpeechError> {
        if text.len() > MAX_TEXT {
            return Err(SpeechError::Model("model_config.yaml is too large".into()));
        }
        let mut entries: Vec<(String, Value)> = Vec::new();
        // open map keys: (indent, key)
        let mut stack: Vec<(usize, String)> = Vec::new();
        // a block scalar (`|`, `>`) or sequence of maps being skipped: lines indented deeper than this
        let mut skip_deeper: Option<usize> = None;
        for raw in text.lines() {
            let line = strip_comment(raw.trim_end());
            if line.trim().is_empty() || line.trim_start().starts_with("---") {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let body = line.trim_start();
            if let Some(d) = skip_deeper {
                if indent > d {
                    continue;
                }
                skip_deeper = None;
            }
            if let Some(item) = body.strip_prefix("- ").or(if body == "-" { Some("") } else { None }) {
                // a sequence item belongs to the innermost open key at this indentation or less
                while stack.last().is_some_and(|(i, _)| *i > indent) {
                    stack.pop();
                }
                let path = stack.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>().join(".");
                if key_value(item).is_some() || item.is_empty() {
                    // a sequence of maps (datasets, augmentations): not needed
                    skip_deeper = Some(indent);
                    continue;
                }
                match entries.iter_mut().rev().find(|(p, _)| *p == path) {
                    Some((_, Value::List(l))) => l.push(unquote(item)),
                    _ => entries.push((path, Value::List(vec![unquote(item)]))),
                }
                continue;
            }
            let Some((key, value)) = key_value(body) else { continue };
            while stack.last().is_some_and(|(i, _)| *i >= indent) {
                stack.pop();
            }
            let mut path = stack.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>().join(".");
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(&unquote(key));
            if value.is_empty() {
                if stack.len() >= MAX_DEPTH {
                    return Err(SpeechError::Model("model_config.yaml is nested too deeply".into()));
                }
                stack.push((indent, unquote(key)));
            } else if value == "|" || value == ">" || value.starts_with("|") || value.starts_with('>') {
                skip_deeper = Some(indent);
            } else if let Some(l) = flow_list(value) {
                entries.push((path, Value::List(l)));
            } else {
                entries.push((path, Value::Scalar(unquote(value))));
            }
        }
        Ok(Self { entries })
    }

    fn get(&self, path: &str) -> Option<&Value> {
        self.entries.iter().rev().find(|(p, _)| p == path).map(|(_, v)| v)
    }

    /// A scalar as written (`null` included).
    pub fn raw(&self, path: &str) -> Option<&str> {
        match self.get(path)? {
            Value::Scalar(s) => Some(s.as_str()),
            Value::List(_) => None,
        }
    }

    /// A scalar (`null` and `~` are absent).
    pub fn str(&self, path: &str) -> Option<&str> {
        match self.get(path)? {
            Value::Scalar(s) if s != "null" && s != "~" => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn int(&self, path: &str) -> Option<i64> {
        self.str(path)?.parse().ok()
    }

    pub fn float(&self, path: &str) -> Option<f64> {
        self.str(path)?.parse().ok().filter(|v: &f64| v.is_finite())
    }

    pub fn bool(&self, path: &str) -> Option<bool> {
        match self.str(path)? {
            "true" | "True" => Some(true),
            "false" | "False" => Some(false),
            _ => None,
        }
    }

    /// A sequence of scalars.
    pub fn list(&self, path: &str) -> Option<&[String]> {
        match self.get(path)? {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn int_list(&self, path: &str) -> Option<Vec<i64>> {
        self.list(path)?.iter().map(|s| s.parse().ok()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "\
sample_rate: 16000
model_defaults:
  tdt_durations:
  - 0
  - 1
  - 2
train_ds:
  manifest_filepath:
  - - /a/b.json
    - /c.json
  is_tarred: true # comment
  augmentor:
    - name: x
      prob: 0.5
preprocessor:
  _target_: nemo.collections.asr.modules.AudioToMelSpectrogramPreprocessor
  window: 'hann'
  dither: 1.0e-05
  pad_to: 0
  note: \"a: b # not a comment\"
encoder:
  att_context_size: [-1, -1]
  xscaling: false
  desc: |
    free text: here
    - not an item
  n_heads: 8
decoding:
  durations:
  - 0
  - 4
";

    #[test]
    fn reads_nemo_style_config() {
        let y = Yaml::parse(CFG).unwrap();
        assert_eq!(y.int("sample_rate"), Some(16000));
        assert_eq!(y.int_list("model_defaults.tdt_durations"), Some(vec![0, 1, 2]));
        assert_eq!(y.str("preprocessor.window"), Some("hann"));
        assert_eq!(y.float("preprocessor.dither"), Some(1e-5));
        assert_eq!(y.int("preprocessor.pad_to"), Some(0));
        assert_eq!(y.str("preprocessor.note"), Some("a: b # not a comment"));
        assert_eq!(y.int_list("encoder.att_context_size"), Some(vec![-1, -1]));
        assert_eq!(y.bool("encoder.xscaling"), Some(false));
        assert_eq!(y.int("encoder.n_heads"), Some(8));
        assert_eq!(y.bool("train_ds.is_tarred"), Some(true));
        assert_eq!(y.int_list("decoding.durations"), Some(vec![0, 4]));
        assert_eq!(y.str("encoder.missing"), None);
    }

    #[test]
    fn garbage_does_not_panic() {
        for s in ["", ":", "- - -", "a:\n - [", "  :\n:x", "a: [1, 2", "\u{feff}a: 1\n\t- x", "a:\n  b:\n    - c: d\n  - e"] {
            let _ = Yaml::parse(s).unwrap();
        }
        let deep: String = (0..200).map(|i| format!("{}k{i}:\n", " ".repeat(i))).collect();
        assert!(Yaml::parse(&deep).is_err());
    }
}
