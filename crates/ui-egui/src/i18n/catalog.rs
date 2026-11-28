//! Parser and lookup tables for translation catalogs (`*.tsv`, format documented in `es.tsv`).

use std::collections::HashMap;

/// A parsed catalog. Built once per language and kept for the life of the process.
#[derive(Debug, Default)]
pub struct Catalog {
    /// context-free strings: English source → translation (looked up every frame)
    plain: HashMap<String, String>,
    /// strings with a disambiguating context, keyed `context \u{1} source`
    contextual: HashMap<String, String>,
    /// context-free entries with `{name}` placeholders, for messages built at run time
    /// ([`Catalog::template`])
    templates: Vec<Template>,
}

/// A catalog entry whose source has placeholders: the literal text around them (`lits.len()` is
/// `names.len() + 1`) and the translation to fill.
#[derive(Debug)]
struct Template {
    lits: Vec<String>,
    names: Vec<String>,
    translation: String,
    /// Bytes of literal text: the more, the more specific the match.
    weight: usize,
}

impl Template {
    /// None when the source has no placeholder, two placeholders side by side (ambiguous), an
    /// unclosed brace, or too little literal text to identify a message.
    fn parse(src: &str, translation: &str) -> Option<Template> {
        let (mut lits, mut names) = (Vec::new(), Vec::new());
        let mut rest = src;
        while let Some((lit, after)) = rest.split_once('{') {
            let (name, tail) = after.split_once('}')?;
            if name.is_empty() || name.contains('{') || (!names.is_empty() && lit.is_empty()) {
                return None;
            }
            lits.push(lit.to_string());
            names.push(name.to_string());
            rest = tail;
        }
        lits.push(rest.to_string());
        let weight = lits.iter().map(String::len).sum();
        (!names.is_empty() && weight >= 8).then(|| Template { lits, names, translation: translation.to_string(), weight })
    }

    /// The placeholder values when `msg` is this template filled in.
    fn captures<'m>(&self, msg: &'m str) -> Option<Vec<&'m str>> {
        let (first, last) = (self.lits.first()?, self.lits.last()?);
        let mut rest = msg.strip_prefix(first.as_str())?.strip_suffix(last.as_str())?;
        let mut out = Vec::with_capacity(self.names.len());
        // each inner literal ends the capture before it (leftmost match); the last capture is
        // what remains before the suffix
        for lit in self.lits.iter().skip(1).take(self.names.len().saturating_sub(1)) {
            let at = rest.find(lit.as_str())?;
            out.push(rest.get(..at)?);
            rest = rest.get(at + lit.len()..)?;
        }
        out.push(rest);
        // a value that opens the template is a name or a count ("{n} selected"), never a clause:
        // otherwise "command … is not available right now: nothing selected" would read as a
        // count of "command … nothing" and come out half translated
        let lead_is_clause = first.is_empty() && out.first().is_some_and(|c| [": ", "; ", ". ", "\n"].iter().any(|s| c.contains(s)));
        (!lead_is_clause && out.iter().all(|c| !c.is_empty())).then_some(out)
    }
}

/// A catalog entry as read from the file: (context, source, translation).
pub type Entry = (String, String, String);

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Read the entries of a catalog file. Malformed lines are returned as errors (and skipped), so a
/// bad translation never breaks the UI; the tests insist the bundled catalogs have none.
pub fn parse_entries(text: &str) -> (Vec<Entry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        match (cols.next(), cols.next(), cols.next(), cols.next()) {
            (Some(ctx), Some(src), Some(tr), None) if !src.is_empty() && !tr.is_empty() => {
                entries.push((unescape(ctx), unescape(src), unescape(tr)));
            }
            _ => errors.push(format!("line {}: expected `context<TAB>source<TAB>translation`", n + 1)),
        }
    }
    (entries, errors)
}

impl Catalog {
    pub fn parse(text: &str) -> Catalog {
        let mut c = Catalog::default();
        for (ctx, src, tr) in parse_entries(text).0 {
            if ctx.is_empty() {
                if let Some(t) = Template::parse(&src, &tr) {
                    c.templates.push(t);
                }
                c.plain.insert(src, tr);
            } else {
                c.contextual.insert(format!("{ctx}\u{1}{src}"), tr);
            }
        }
        c
    }

    pub fn plain(&self, s: &str) -> Option<&str> {
        self.plain.get(s).map(String::as_str)
    }

    pub fn contextual(&self, ctx: &str, s: &str) -> Option<&str> {
        self.contextual.get(&format!("{ctx}\u{1}{s}")).map(String::as_str)
    }

    /// A message built at run time from a catalog template (`"Failed: {e}"` matches
    /// `"Failed: disk full"`): the most specific matching template's translation and the
    /// placeholder values, by name. Linear in the catalog: for status messages, not every frame.
    pub fn template<'m>(&self, msg: &'m str) -> Option<(&str, Vec<(&str, &'m str)>)> {
        let mut best: Option<(&Template, Vec<&'m str>)> = None;
        for t in &self.templates {
            if best.as_ref().is_some_and(|(b, _)| b.weight >= t.weight) {
                continue;
            }
            if let Some(caps) = t.captures(msg) {
                best = Some((t, caps));
            }
        }
        let (t, caps) = best?;
        Some((t.translation.as_str(), t.names.iter().map(String::as_str).zip(caps).collect()))
    }
}

/// `{name}` placeholders of a template, in order of appearance.
#[cfg(test)]
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find('{') {
        let after = &rest[a + 1..];
        match after.find('}') {
            Some(b) => {
                out.push(&after[..b]);
                rest = &after[b + 1..];
            }
            None => break,
        }
    }
    out
}
