//! WebVTT (`.vtt`).
//!
//! ```text
//! WEBVTT
//!
//! STYLE
//! ::cue { color: yellow }
//!
//! intro
//! 00:00:01.000 --> 00:00:03.500 line:90% align:center
//! <v Ann>Hello there
//! ```
//!
//! Kept on read: cue identifiers, cue settings (verbatim), a leading `<v Speaker>` voice span (as
//! the speaker), inline markup in the text, and STYLE / REGION / NOTE blocks. A missing `WEBVTT`
//! signature is tolerated. Timestamps may omit hours.

use crate::srt::parse_timing_line;
use crate::{Cue, Document, Error, Result, format_clock};

/// Split a leading `<v Name>` / `<v.class Name>` voice span off cue text.
pub fn split_voice(text: &str) -> (Option<String>, String) {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<v")
        && (rest.starts_with(' ') || rest.starts_with('.') || rest.starts_with('\t'))
        && let Some(close) = rest.find('>')
    {
        let inner = &rest[..close];
        // skip classes: `.loud.x Name`
        let name = match inner.find([' ', '\t']) {
            Some(sp) => inner[sp..].trim(),
            None => "",
        };
        let body = &rest[close + 1..];
        // a closing </v> at the very end goes too; one in the middle means several voices: keep all
        let body = match body.find("</v>") {
            Some(p) if p + 4 == body.trim_end().len() => body[..p].to_string(),
            Some(_) => return (None, text.to_string()),
            None => body.to_string(),
        };
        if body.contains("<v ") || body.contains("<v.") || body.contains("<v\t") {
            return (None, text.to_string());
        }
        return ((!name.is_empty()).then(|| name.to_string()), body);
    }
    (None, text.to_string())
}

pub fn parse(text: &str) -> Result<Document> {
    let mut doc = Document::default();
    let mut lines = text.lines().peekable();
    // signature + header text
    let has_sig = lines.peek().is_some_and(|l| {
        let l = l.trim_start_matches('\u{feff}');
        l == "WEBVTT" || l.starts_with("WEBVTT ") || l.starts_with("WEBVTT\t")
    });
    if has_sig {
        for l in lines.by_ref() {
            if l.trim().is_empty() {
                break;
            }
        }
    }
    // blocks
    let mut block: Vec<&str> = Vec::new();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for l in lines {
        if l.trim().is_empty() {
            if !block.is_empty() {
                blocks.push(std::mem::take(&mut block));
            }
        } else {
            block.push(l);
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    let mut seen_cue = false;
    for b in blocks {
        let first = b[0].trim();
        if !b.iter().any(|l| l.contains("-->")) && (first == "NOTE" || first.starts_with("NOTE ") || first.starts_with("NOTE\t")) {
            doc.blocks.push(b.join("\n"));
            continue;
        }
        if !seen_cue && !first.contains("-->") && (first == "STYLE" || first == "REGION") {
            doc.blocks.push(b.join("\n"));
            continue;
        }
        let (id, timing_idx) = if b[0].contains("-->") { (None, 0) } else { (Some(b[0].trim().to_string()), 1) };
        let Some(timing) = b.get(timing_idx) else {
            doc.warnings.push(format!("ignored block \"{first}\""));
            continue;
        };
        let Some((start, end, settings)) = parse_timing_line(timing) else {
            doc.warnings.push(format!("bad timing line \"{}\"", timing.trim()));
            continue;
        };
        seen_cue = true;
        if end <= start {
            doc.warnings.push(format!("cue at {} has end before start; skipped", format_clock(start, '.')));
            continue;
        }
        let raw = b[timing_idx + 1..].iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
        let (speaker, text) = split_voice(&raw);
        doc.cues.push(Cue { start, end, text, speaker, id, settings: settings.to_string() });
    }
    if !has_sig && doc.cues.is_empty() && !text.trim().is_empty() {
        return Err(Error::NotFormat("WebVTT"));
    }
    Ok(doc)
}

pub fn write(doc: &Document) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for b in &doc.blocks {
        let b = b.trim_end();
        if !b.is_empty() {
            out.push_str(b);
            out.push_str("\n\n");
        }
    }
    for c in &doc.cues {
        if let Some(id) = c.id.as_deref().map(str::trim).filter(|i| !i.is_empty() && !i.contains("-->")) {
            out.push_str(id);
            out.push('\n');
        }
        out.push_str(&format!("{} --> {}", format_clock(c.start, '.'), format_clock(c.end, '.')));
        if !c.settings.trim().is_empty() {
            out.push(' ');
            out.push_str(c.settings.trim());
        }
        out.push('\n');
        let mut text = String::new();
        if let Some(s) = c.speaker.as_deref().filter(|s| !s.is_empty()) {
            text.push_str(&format!("<v {s}>"));
        }
        text.push_str(&c.text);
        for l in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push_str(&l.replace("-->", "--&gt;"));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TICKS_PER_MS;

    const SAMPLE: &str = "WEBVTT - with header\nKind: captions\n\nSTYLE\n::cue { color: yellow }\n\nNOTE a comment\nspanning lines\n\nintro\n00:01.000 --> 00:00:03.500 line:90% align:start\n<v.loud Ann Smith>Hello there\nsecond line\n\n00:00:04.000 --> 00:00:05.000\n<i>plain</i> &amp; tags\n";

    #[test]
    fn parse_sample() {
        let d = parse(SAMPLE).unwrap();
        assert_eq!(d.blocks, vec!["STYLE\n::cue { color: yellow }".to_string(), "NOTE a comment\nspanning lines".to_string()]);
        assert_eq!(d.cues.len(), 2);
        let c = &d.cues[0];
        assert_eq!(c.id.as_deref(), Some("intro"));
        assert_eq!(c.start.0, 1000 * TICKS_PER_MS);
        assert_eq!(c.end.0, 3500 * TICKS_PER_MS);
        assert_eq!(c.settings, "line:90% align:start");
        assert_eq!(c.speaker.as_deref(), Some("Ann Smith"));
        assert_eq!(c.text, "Hello there\nsecond line");
        assert_eq!(d.cues[1].text, "<i>plain</i> &amp; tags");
        let w = write(&d);
        assert!(w.starts_with("WEBVTT\n\nSTYLE\n::cue { color: yellow }\n\nNOTE a comment\nspanning lines\n\nintro\n00:00:01.000 --> 00:00:03.500 line:90% align:start\n<v Ann Smith>Hello there\n"));
        assert_eq!(parse(&w).unwrap().cues, d.cues);
    }

    #[test]
    fn voices() {
        assert_eq!(split_voice("<v Bob>hi</v>"), (Some("Bob".into()), "hi".into()));
        assert_eq!(split_voice("<v Bob>hi</v> <v Ann>yo</v>"), (None, "<v Bob>hi</v> <v Ann>yo</v>".into()));
        assert_eq!(split_voice("<vx>hi"), (None, "<vx>hi".into()));
    }

    #[test]
    fn multiple_voices_with_tab_separator() {
        let text = "<v Ann>Hello <v\tBob>there";
        assert_eq!(split_voice(text), (None, text.to_string()));
        let source = format!("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n{text}\n");
        let doc = parse(&source).unwrap();
        assert_eq!(doc.cues[0].speaker, None);
        assert_eq!(doc.cues[0].text, text);
        assert_eq!(parse(&write(&doc)).unwrap().cues, doc.cues);
        let closed = "<v Ann>Hi <v\tBob>x</v>";
        assert_eq!(split_voice(closed), (None, closed.to_string()));
    }

    #[test]
    fn note_prefixed_cue_id_is_a_cue() {
        let d = parse("WEBVTT\n\nNOTE 1\n00:00:01.000 --> 00:00:02.000\nhello\n").unwrap();
        assert_eq!(d.cues.len(), 1);
        assert_eq!(d.cues[0].id.as_deref(), Some("NOTE 1"));
        assert!(d.blocks.is_empty());
    }

    #[test]
    fn without_signature() {
        let d = parse("00:00:01.000 --> 00:00:02.000\nx\n").unwrap();
        assert_eq!(d.cues.len(), 1);
        assert!(parse("just text").is_err());
    }
}
