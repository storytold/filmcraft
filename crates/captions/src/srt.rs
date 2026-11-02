//! SubRip (`.srt`).
//!
//! ```text
//! 1
//! 00:00:01,000 --> 00:00:03,500
//! First line
//! <i>second line</i>
//!
//! 2
//! …
//! ```
//!
//! Reading accepts what real files contain: missing or wrong indexes, `.` instead of `,`, 1–3
//! digit fractions, missing hours, extra spaces, trailing SSA position coordinates
//! (`X1:… Y2:…`), and cues not separated by a blank line (a new cue starts at a timing line, or at
//! an integer line directly followed by one). Writing renumbers from 1 and rounds to milliseconds.

use filmcraft_time::Tick;

use crate::{Cue, Document, Error, Result, format_clock, parse_clock};

/// Parse `start --> end[ settings]` (SRT or VTT timing line). Returns (start, end, settings).
pub fn parse_timing_line(line: &str) -> Option<(Tick, Tick, &str)> {
    let (a, rest) = line.split_once("-->")?;
    let start = parse_clock(a.trim())?;
    let rest = rest.trim_start();
    let end_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let end = parse_clock(&rest[..end_len])?;
    Some((start, end, rest[end_len..].trim()))
}

fn is_index(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty() && l.len() <= 10 && l.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse(text: &str) -> Result<Document> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut doc = Document::default();
    let mut i = 0;
    let mut found_any = false;
    while i < lines.len() {
        let Some((start, end, _settings)) = parse_timing_line(lines[i]) else {
            let l = lines[i].trim();
            if !l.is_empty() && !is_index(l) && found_any {
                doc.warnings.push(format!("line {}: ignored stray text \"{l}\"", i + 1));
            }
            i += 1;
            continue;
        };
        found_any = true;
        i += 1;
        let mut text_lines: Vec<&str> = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            if l.trim().is_empty() {
                break;
            }
            if parse_timing_line(l).is_some() {
                break;
            }
            if is_index(l) && lines.get(i + 1).is_some_and(|n| parse_timing_line(n).is_some()) {
                break;
            }
            text_lines.push(l.trim_end());
            i += 1;
        }
        if end <= start {
            doc.warnings.push(format!("cue at {} has end before start; skipped", format_clock(start, ',')));
            continue;
        }
        doc.cues.push(Cue { start, end, text: text_lines.join("\n"), ..Default::default() });
    }
    if !found_any && !text.trim().is_empty() {
        return Err(Error::NotFormat("SubRip"));
    }
    Ok(doc)
}

pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    for (n, c) in doc.cues.iter().enumerate() {
        out.push_str(&format!("{}\n{} --> {}\n", n + 1, format_clock(c.start, ','), format_clock(c.end, ',')));
        // blank lines inside text would end the cue: drop them
        for l in c.text.lines().filter(|l| !l.trim().is_empty()) {
            out.push_str(l);
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

    fn ms(v: i64) -> Tick {
        Tick(v * TICKS_PER_MS)
    }

    #[test]
    fn basic() {
        let d = parse("1\n00:00:01,000 --> 00:00:02,500\nHello\n<i>World</i>\n\n2\n00:00:03,000 --> 00:00:04,000\nBye\n").unwrap();
        assert_eq!(d.cues.len(), 2);
        assert_eq!(d.cues[0].start, ms(1000));
        assert_eq!(d.cues[0].end, ms(2500));
        assert_eq!(d.cues[0].text, "Hello\n<i>World</i>");
        assert_eq!(write(&d), "1\n00:00:01,000 --> 00:00:02,500\nHello\n<i>World</i>\n\n2\n00:00:03,000 --> 00:00:04,000\nBye\n\n");
    }

    #[test]
    fn sloppy() {
        // no indexes, period ms, short fractions, no hours, coordinates, no blank line between cues
        let src = "00:00:01.5 --> 00:00:02.25 X1:10 X2:20 Y1:1 Y2:2\nA\n7\n00:03,000 --> 00:04,000\nB\n00:00:05,000-->00:00:06,000\n42\n\n\n";
        let d = parse(src).unwrap();
        let got: Vec<(i64, i64, &str)> = d.cues.iter().map(|c| (c.start.0 / TICKS_PER_MS, c.end.0 / TICKS_PER_MS, c.text.as_str())).collect();
        assert_eq!(got, vec![(1500, 2250, "A"), (3000, 4000, "B"), (5000, 6000, "42")]);
    }

    #[test]
    fn ignores_byte_order_mark() {
        let d = parse("\u{feff}00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        assert_eq!(d.cues.len(), 1);
        assert_eq!(d.cues[0].start, ms(1000));
        assert_eq!(d.cues[0].text, "Hi");
        let d = parse("\u{feff}00:00:01,000 --> 00:00:02,000\nHi\n\n2\n00:00:03,000 --> 00:00:04,000\nBye\n").unwrap();
        assert_eq!(d.cues.len(), 2);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("hello world\nthis is not srt").is_err());
        assert!(parse("").unwrap().cues.is_empty());
    }

    #[test]
    fn skips_inverted_cue() {
        let d = parse("1\n00:00:05,000 --> 00:00:04,000\nX\n\n2\n00:00:06,000 --> 00:00:07,000\nY\n").unwrap();
        assert_eq!(d.cues.len(), 1);
        assert_eq!(d.warnings.len(), 1);
    }
}
