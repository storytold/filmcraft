//! Minimal XML writer and read helpers over `roxmltree`.

use std::fmt::Write as _;

use roxmltree::Node;

pub(crate) fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {}
            c => o.push(c),
        }
    }
    o
}

/// Pretty-printing XML writer (two-space indent, one element per line).
pub(crate) struct XmlWriter {
    out: String,
    stack: Vec<String>,
}

impl XmlWriter {
    pub fn new(doctype: Option<&str>) -> Self {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        if let Some(d) = doctype {
            let _ = writeln!(out, "<!DOCTYPE {d}>");
        }
        Self { out, stack: Vec::new() }
    }

    fn indent(&mut self) {
        for _ in 0..self.stack.len() {
            self.out.push_str("  ");
        }
    }

    fn start_tag(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.indent();
        self.out.push('<');
        self.out.push_str(name);
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", escape(v));
        }
    }

    pub fn open(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.start_tag(name, attrs);
        self.out.push_str(">\n");
        self.stack.push(name.to_string());
    }

    pub fn close(&mut self) {
        let Some(name) = self.stack.pop() else { return };
        self.indent();
        let _ = writeln!(self.out, "</{name}>");
    }

    pub fn empty(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.start_tag(name, attrs);
        self.out.push_str("/>\n");
    }

    /// `<name attrs>value</name>`.
    pub fn text_attrs(&mut self, name: &str, attrs: &[(&str, &str)], value: &str) {
        self.start_tag(name, attrs);
        let _ = writeln!(self.out, ">{}</{name}>", escape(value));
    }

    pub fn text(&mut self, name: &str, value: impl std::fmt::Display) {
        self.text_attrs(name, &[], &value.to_string());
    }

    pub fn finish(mut self) -> String {
        while !self.stack.is_empty() {
            self.close();
        }
        self.out
    }
}

pub(crate) fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

pub(crate) fn children<'a, 'i>(n: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(move |c| c.is_element() && c.tag_name().name() == name)
}

pub(crate) fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

/// Trimmed text of a node's direct text content.
pub(crate) fn text<'a>(n: Node<'a, '_>) -> &'a str {
    n.text().map(str::trim).unwrap_or("")
}

pub(crate) fn path_node<'a, 'i>(n: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
    path.iter().try_fold(n, |cur, p| child(cur, p))
}

/// Text of the element at `path` below `n`.
pub(crate) fn path_text<'a>(n: Node<'a, '_>, path: &[&str]) -> Option<&'a str> {
    path_node(n, path).map(text)
}

pub(crate) fn child_text<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name).map(text)
}

pub(crate) fn child_i64(n: Node<'_, '_>, name: &str) -> Option<i64> {
    child_text(n, name).and_then(|t| t.parse::<i64>().ok().or_else(|| t.parse::<f64>().ok().map(|f| f.round() as i64)))
}

pub(crate) fn child_f64(n: Node<'_, '_>, name: &str) -> Option<f64> {
    child_text(n, name).and_then(|t| t.parse::<f64>().ok())
}

pub(crate) fn child_bool(n: Node<'_, '_>, name: &str) -> Option<bool> {
    child_text(n, name).map(|t| t.eq_ignore_ascii_case("true") || t == "1")
}

pub(crate) fn bool_str(b: bool) -> &'static str {
    if b { "TRUE" } else { "FALSE" }
}

/// Format an f64 compactly (no trailing zeros, no exponent for normal ranges).
pub(crate) fn num(v: f64) -> String {
    if v == v.round() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    if !v.is_finite() {
        return "0".into();
    }
    // Shortest representation that parses back to the same f64.
    format!("{v}")
}

pub(crate) fn parse(text: &str) -> Result<roxmltree::Document<'_>, String> {
    let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    roxmltree::Document::parse_with_options(text, opts).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_roundtrip() {
        let mut w = XmlWriter::new(Some("xmeml"));
        w.open("a", &[("x", "1 & 2")]);
        w.text("b", "<hi>");
        w.empty("c", &[]);
        let s = w.finish();
        let d = parse(&s).unwrap();
        let a = d.root_element();
        assert_eq!(a.attribute("x"), Some("1 & 2"));
        assert_eq!(child_text(a, "b"), Some("<hi>"));
        assert!(child(a, "c").is_some());
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(100.0), "100");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(0.1 + 0.2).parse::<f64>().unwrap(), 0.1 + 0.2);
    }
}
