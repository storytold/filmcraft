use std::cell::Cell;
use std::collections::HashMap;
use std::io::Read;

use roxmltree::{Document, Node, ParsingOptions};

use crate::xml::{child, child_text, elements};
use crate::{Error, Result};

pub(super) const PROJECT: &str = "Premiere Pro project";
pub(super) const PRESETS: &str = "Premiere effect presets";
const MAX_OBJECTS: usize = 200_000;

pub(super) fn error(format: &'static str, message: impl Into<String>) -> Error {
    Error::Parse { format, message: message.into() }
}

pub(super) fn decode(bytes: &[u8], format: &'static str) -> Result<String> {
    if bytes.len() > super::MAX_DOCUMENT_BYTES {
        return Err(error(format, "document exceeds the 64 MiB size limit"));
    }
    let mut decoded = Vec::new();
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(bytes)
            .take(super::MAX_DOCUMENT_BYTES as u64 + 1)
            .read_to_end(&mut decoded)
            .map_err(|e| error(format, format!("invalid gzip document: {e}")))?;
        if decoded.len() > super::MAX_DOCUMENT_BYTES {
            return Err(error(format, "decompressed document exceeds the 64 MiB size limit"));
        }
        decoded.as_slice()
    } else {
        bytes
    };
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little = bytes.starts_with(&[0xff, 0xfe]);
        let payload = bytes.get(2..).unwrap_or_default();
        if !payload.len().is_multiple_of(2) {
            return Err(error(format, "truncated UTF-16 document"));
        }
        let words: Vec<u16> =
            payload.chunks_exact(2).map(|b| if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }).collect();
        return String::from_utf16(&words).map_err(|e| error(format, e.to_string()));
    }
    std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)).map(str::to_string).map_err(|e| error(format, e.to_string()))
}

pub(super) fn parse<'i>(text: &'i str, format: &'static str) -> Result<Document<'i>> {
    let doc = Document::parse_with_options(text, ParsingOptions { allow_dtd: false, nodes_limit: 2_000_000, ..Default::default() })
        .map_err(|e| error(format, e.to_string()))?;
    let root = doc.root_element();
    if !root.has_tag_name("PremiereData") || root.attribute("Version") != Some("3") {
        return Err(error(format, "expected a PremiereData version 3 object graph"));
    }
    // Shared graph objects can repeat one field thousands of times. Bound fields before any
    // expansion, including raw whitespace, so the document cap also bounds parsing/copying work.
    for node in doc.descendants().filter(Node::is_element) {
        let name = node.tag_name().name();
        if name.len() > 256 {
            return Err(error(format, "XML field name exceeds 256 bytes"));
        }
        for attr in node.attributes() {
            if matches!(attr.name(), "ObjectID" | "ObjectUID" | "ObjectRef" | "ObjectURef") && attr.value().len() > 256 {
                return Err(error(format, "object identifier exceeds 256 bytes"));
            }
        }
        let limit = match name {
            "FilePath" | "ActualMediaFilePath" | "RelativePath" => 131_072,
            "AudioChannelLayout" => 16_384,
            "CurrentValue" | "StartKeyframe" | "FrameRect" => 1024,
            "FrameRate" | "Duration" | "Start" | "End" | "InPoint" | "OutPoint" | "AnchorInPoint" | "AnchorOutPoint" | "Type" | "Speed"
            | "TransitionDuration" | "ParameterID" | "Bypass" | "IsTimeVarying" | "IsLocked" | "IsMuted" | "IsSyncLocked" | "Alignment" => 128,
            "Description" => 4096,
            _ if name.ends_with("Name") => 4096,
            _ => continue,
        };
        if node.text().is_some_and(|text| text.len() > limit) {
            return Err(error(format, format!("{name} exceeds the {limit} byte field limit")));
        }
    }
    Ok(doc)
}

pub(super) fn at<'a, 'i>(mut n: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
    for part in path {
        n = child(n, part)?;
    }
    Some(n)
}

pub(super) fn text_at<'a>(n: Node<'a, '_>, path: &[&str]) -> Option<&'a str> {
    at(n, path).and_then(|n| n.text()).map(str::trim)
}

pub(super) fn key(n: Node<'_, '_>) -> String {
    if let Some(id) = n.attribute("ObjectUID") {
        format!("u:{id}")
    } else if let Some(id) = n.attribute("ObjectID") {
        format!("i:{id}")
    } else {
        format!("node:{}", n.id().get())
    }
}

pub(super) struct Graph<'a, 'i> {
    pub root: Node<'a, 'i>,
    pub format: &'static str,
    objects: HashMap<String, Node<'a, 'i>>,
    work_left: Cell<usize>,
}

impl<'a, 'i> Graph<'a, 'i> {
    pub fn new(root: Node<'a, 'i>, format: &'static str) -> Result<Self> {
        let mut objects = HashMap::new();
        for n in elements(root) {
            for (field, prefix) in [("ObjectID", "i:"), ("ObjectUID", "u:")] {
                if let Some(id) = n.attribute(field) {
                    if id.is_empty() || objects.insert(format!("{prefix}{id}"), n).is_some() {
                        return Err(error(format, format!("empty or duplicate {field} `{id}`")));
                    }
                    if objects.len() > MAX_OBJECTS {
                        return Err(error(format, "object graph exceeds the 200000 object limit"));
                    }
                }
            }
        }
        Ok(Self { root, format, objects, work_left: Cell::new(2_000_000) })
    }

    pub fn charge(&self) -> Result<()> {
        let left = self.work_left.get().checked_sub(1).ok_or_else(|| error(self.format, "object graph expansion exceeds the work limit"))?;
        self.work_left.set(left);
        Ok(())
    }

    pub fn resolve(&self, n: Node<'a, 'i>) -> Result<Node<'a, 'i>> {
        self.charge()?;
        let target = if let Some(id) = n.attribute("ObjectRef") {
            format!("i:{id}")
        } else if let Some(id) = n.attribute("ObjectURef") {
            format!("u:{id}")
        } else {
            return Err(error(self.format, format!("{} has no object reference", n.tag_name().name())));
        };
        self.objects.get(&target).copied().ok_or_else(|| error(self.format, format!("missing object {target}")))
    }

    pub fn reference(&self, n: Node<'a, 'i>, path: &[&str]) -> Result<Node<'a, 'i>> {
        self.resolve(at(n, path).ok_or_else(|| error(self.format, format!("{} is missing {}", n.tag_name().name(), path.join("/"))))?)
    }

    pub fn references(&self, n: Node<'a, 'i>, path: &[&str]) -> Result<Vec<Node<'a, 'i>>> {
        let Some(list) = at(n, path) else { return Ok(Vec::new()) };
        let mut refs = Vec::new();
        for (ordinal, r) in elements(list).enumerate() {
            if refs.len() >= MAX_OBJECTS {
                return Err(error(self.format, "too many references in an object list"));
            }
            let index = r.attribute("Index").map(|s| s.parse::<usize>()).transpose().map_err(|e| error(self.format, e.to_string()))?.unwrap_or(ordinal);
            if index >= MAX_OBJECTS {
                return Err(error(self.format, "object-list index is out of range"));
            }
            refs.push((index, self.resolve(r)?));
        }
        refs.sort_by_key(|r| r.0);
        Ok(refs.into_iter().map(|r| r.1).collect())
    }

    pub fn integer(&self, n: Node<'_, '_>, path: &[&str], default: i64) -> Result<i64> {
        text_at(n, path)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<i64>().map_err(|e| error(self.format, format!("invalid {}: {e}", path.join("/")))))
            .unwrap_or(Ok(default))
    }

    pub fn required_integer(&self, n: Node<'_, '_>, path: &[&str]) -> Result<i64> {
        let value = text_at(n, path).ok_or_else(|| error(self.format, format!("missing {}", path.join("/"))))?;
        value.parse::<i64>().map_err(|e| error(self.format, format!("invalid {}: {e}", path.join("/"))))
    }

    pub fn boolean(&self, n: Node<'_, '_>, path: &[&str], default: bool) -> Result<bool> {
        match text_at(n, path) {
            None | Some("") => Ok(default),
            Some("true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            Some(v) => Err(error(self.format, format!("invalid boolean `{v}` in {}", path.join("/")))),
        }
    }
}

pub(super) fn static_text<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    child_text(n, "StartKeyframe").and_then(|s| s.split(',').nth(1)).or_else(|| child_text(n, "CurrentValue"))
}
