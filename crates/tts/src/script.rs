//! Script parsing: speech text and pause markers.
//!
//! A pause marker is `[pause <n>s]` or `[pause <n>ms]` (case-insensitive, `n` a decimal number,
//! spaces optional: `[pause 1.5s]`, `[Pause 500 ms]`). Pauses are capped at [`MAX_PAUSE_MS`]. A
//! bracket that is not a well-formed marker is ordinary text. The Text to Speech panel's
//! *Add pause* button inserts `[pause 1s]`.

/// Longest single pause.
pub const MAX_PAUSE_MS: u32 = 10_000;

/// A piece of a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Speech(String),
    Pause { millis: u32 },
}

/// The marker *Add pause* inserts.
pub const PAUSE_MARKER: &str = "[pause 1s]";

/// Split `text` into speech and pauses, in order. Empty speech pieces are dropped.
pub fn parse(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut speech = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let (before, from) = rest.split_at(open);
        speech.push_str(before);
        let close = from.find(']');
        match (close, close.and_then(|c| from.get(..=c)).and_then(pause_millis)) {
            (Some(close), Some(millis)) => {
                flush(&mut out, &mut speech);
                out.push(Segment::Pause { millis });
                rest = from.get(close + 1..).unwrap_or("");
            }
            _ => {
                speech.push('[');
                rest = from.get(1..).unwrap_or("");
            }
        }
    }
    speech.push_str(rest);
    flush(&mut out, &mut speech);
    out
}

fn flush(out: &mut Vec<Segment>, speech: &mut String) {
    if !speech.trim().is_empty() {
        out.push(Segment::Speech(std::mem::take(speech)));
    }
    speech.clear();
}

/// `[pause 1.5s]` → 1500. None if `marker` is not a pause marker.
fn pause_millis(marker: &str) -> Option<u32> {
    let inner = marker.strip_prefix('[')?.strip_suffix(']')?.trim();
    let lower = inner.to_ascii_lowercase();
    let arg = lower.strip_prefix("pause")?.trim();
    let (num, scale) = if let Some(n) = arg.strip_suffix("ms") { (n.trim(), 1.0) } else { (arg.strip_suffix('s')?.trim(), 1000.0) };
    if num.is_empty() || num.len() > 12 || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let v: f64 = num.parse().ok()?;
    let ms = (v * scale).round();
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    Some(ms.min(f64::from(MAX_PAUSE_MS)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(s: &str) -> Segment {
        Segment::Speech(s.into())
    }

    #[test]
    fn splits_speech_and_pauses() {
        assert_eq!(parse("Hello. [pause 1s] World"), vec![sp("Hello. "), Segment::Pause { millis: 1000 }, sp(" World")]);
        assert_eq!(parse("[Pause 500 ms]Go"), vec![Segment::Pause { millis: 500 }, sp("Go")]);
        assert_eq!(parse("a[pause 1.5s][pause 2s]b"), vec![sp("a"), Segment::Pause { millis: 1500 }, Segment::Pause { millis: 2000 }, sp("b")]);
        assert_eq!(parse(PAUSE_MARKER), vec![Segment::Pause { millis: 1000 }]);
    }

    #[test]
    fn malformed_markers_are_text() {
        assert_eq!(parse("[pause]"), vec![sp("[pause]")]);
        assert_eq!(parse("[pause 1x] a"), vec![sp("[pause 1x] a")]);
        assert_eq!(parse("[pause 1s"), vec![sp("[pause 1s")]);
        assert_eq!(parse("[note] [pause -1s]"), vec![sp("[note] [pause -1s]")]);
        assert_eq!(parse("[[pause 1s]]"), vec![sp("["), Segment::Pause { millis: 1000 }, sp("]")]);
    }

    #[test]
    fn hostile_inputs_never_panic_and_pauses_are_capped() {
        assert_eq!(parse("[pause 999999999999s]"), vec![Segment::Pause { millis: MAX_PAUSE_MS }]);
        assert_eq!(parse("[pause 1e9s]"), vec![sp("[pause 1e9s]")]);
        for s in ["", "[", "]", "[[[[", "日本語[pause 1s]👍", "\u{301}[", "[pause 1.2.3s]", "\0[pause 0s]"] {
            let _ = parse(s);
        }
        assert_eq!(parse("  "), vec![]);
    }
}
