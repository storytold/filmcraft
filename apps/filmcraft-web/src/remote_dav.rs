//! WebDAV wire format for the web build's remote storage: the PROPFIND request body, the
//! multistatus parser, and the checks on a ranged GET response.
//!
//! Pure (no browser types), so it runs natively in `tests/it`. A server's answer is untrusted:
//! bodies and entry counts are capped, sizes and ranges are checked with checked arithmetic, and
//! a response that does not match the request is an error, never data.

use crate::remote_config::{Config, Mount};

/// How long a remote request may take before it is aborted (milliseconds): metadata requests are
/// short; GET and PUT move file data and get a generous total cap.
pub fn timeout_ms(method: &str) -> i32 {
    match method {
        "GET" | "PUT" => 300_000,
        _ => 20_000,
    }
}

/// A request timeout cut to the time left in the caller's budget (`None`: no budget), at least
/// 1 ms so an expired budget aborts the request at once.
pub fn capped_timeout_ms(base: i32, left_ms: Option<u64>) -> i32 {
    match left_ms {
        Some(left) => i32::try_from(left).map_or(base, |l| l.min(base)).max(1),
        None => base,
    }
}

/// Largest PROPFIND answer read (bytes).
pub const MAX_XML_BYTES: usize = 16 << 20;
/// Most entries of one directory listing.
pub const MAX_ENTRIES: usize = 20_000;
/// Most XML nodes parsed.
const MAX_NODES: u32 = 400_000;
/// Largest project file fetched whole (bytes).
pub const MAX_PROJECT_BYTES: u64 = 64 << 20;

/// PROPFIND request body: the four properties the browser uses.
pub const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:getcontentlength/><d:getlastmodified/><d:resourcetype/><d:getetag/></d:prop></d:propfind>"#;

/// One `<response>` of a multistatus answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DavEntry {
    pub href: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Modification time, seconds since the Unix epoch.
    pub modified: Option<u64>,
    pub etag: Option<String>,
}

/// A direct child of a listed directory, mapped back to a virtual path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    pub path: String,
    pub name: String,
    pub entry: DavEntry,
}

/// Whether the element has this local name in the `DAV:` namespace.
fn is_dav(n: &roxmltree::Node, local: &str) -> bool {
    n.is_element() && n.tag_name().name() == local && n.tag_name().namespace() == Some("DAV:")
}

/// The first `DAV:` child element with this local name.
fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, local: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| is_dav(c, local))
}

/// The trimmed text of an element, `None` when missing or empty.
fn text(n: Option<roxmltree::Node>) -> Option<String> {
    n.and_then(|n| n.text()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// `HTTP/1.1 200 OK`: the status code of a propstat, `None` when malformed.
fn status_code(line: &str) -> Option<u16> {
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Parse a `207 Multi-Status` body. Entries whose properties were not delivered with a 2xx status
/// keep what is known (an `href`); a response without an `href` is dropped.
pub fn parse_multistatus(xml: &[u8]) -> Result<Vec<DavEntry>, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("listing too large".into());
    }
    let s = std::str::from_utf8(xml).map_err(|_| "listing is not UTF-8".to_string())?;
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: MAX_NODES, ..roxmltree::ParsingOptions::default() };
    let doc = roxmltree::Document::parse_with_options(s, opts).map_err(|e| format!("invalid listing: {e}"))?;
    let root = doc.root_element();
    if !is_dav(&root, "multistatus") {
        return Err("not a multistatus answer".into());
    }
    let mut out = Vec::new();
    for r in root.children().filter(|c| is_dav(c, "response")) {
        let Some(href) = text(child(r, "href")) else { continue };
        let mut e = DavEntry { href, ..DavEntry::default() };
        for ps in r.children().filter(|c| is_dav(c, "propstat")) {
            if !text(child(ps, "status")).and_then(|l| status_code(&l)).is_some_and(|c| (200..300).contains(&c)) {
                continue;
            }
            let Some(prop) = child(ps, "prop") else { continue };
            e.is_dir |= child(prop, "resourcetype").is_some_and(|t| child(t, "collection").is_some());
            e.size = e.size.or_else(|| text(child(prop, "getcontentlength")).and_then(|t| t.parse().ok()));
            e.modified = e.modified.or_else(|| text(child(prop, "getlastmodified")).and_then(|t| parse_http_date(&t)));
            e.etag = e.etag.or_else(|| text(child(prop, "getetag")).filter(|t| t.len() <= 256));
        }
        if out.len() >= MAX_ENTRIES {
            return Err("directory has too many entries".into());
        }
        out.push(e);
    }
    Ok(out)
}

/// `Wed, 21 Oct 2015 07:28:00 GMT` (RFC 1123) → Unix seconds; `None` for anything else.
pub fn parse_http_date(s: &str) -> Option<u64> {
    let t = chrono::DateTime::parse_from_rfc2822(s.trim()).ok()?.timestamp();
    u64::try_from(t).ok()
}

/// The direct children of `dir` (a virtual path) in a listing of it: hrefs outside the mount, the
/// directory itself and deeper entries are dropped.
pub fn children_of(cfg: &Config, mount: &Mount, dir: &str, origin: &str, entries: Vec<DavEntry>) -> Vec<Child> {
    let dir = dir.strip_suffix('/').unwrap_or(dir);
    let mut out = Vec::new();
    for entry in entries {
        let Some(path) = cfg.path_for_href(mount, &entry.href, origin) else { continue };
        if crate::remote_config::parent(&path) != Some(dir) || path == dir {
            continue;
        }
        let Some(name) = path.rsplit('/').next().filter(|n| !n.is_empty()).map(str::to_string) else { continue };
        out.push(Child { path, name, entry });
    }
    out
}

/// A validated `Content-Range: bytes a-b/total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    /// Last byte, inclusive.
    pub end: u64,
    pub total: Option<u64>,
}

/// Parse a `Content-Range` header (`bytes a-b/total`).
pub fn parse_content_range(h: &str) -> Option<ContentRange> {
    let rest = h.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let (start, end): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse::<u64>().ok()?),
    };
    (start <= end && total.is_none_or(|t| end < t)).then_some(ContentRange { start, end, total })
}

/// Check the answer to `Range: bytes=start-(start+want-1)` for a file of known `size`. Only `206`
/// with a matching `Content-Range` is data: a `200` full body (the server ignored the range) and
/// anything else is refused. Returns the number of body bytes expected.
pub fn check_range_response(status: u16, content_range: Option<&str>, start: u64, want: u64, size: u64) -> Result<u64, String> {
    if status != 206 {
        return Err(format!("server answered {status} to a range request (a 206 is required)"));
    }
    let r = content_range.and_then(parse_content_range).ok_or("missing or malformed Content-Range")?;
    let end_excl = start.checked_add(want).ok_or("range overflows")?.min(size);
    let expected_end = end_excl.checked_sub(1).ok_or("empty range")?;
    if r.start != start || r.end != expected_end {
        return Err(format!("server returned bytes {}-{} for a request for {start}-{expected_end}", r.start, r.end));
    }
    if r.total.is_some_and(|t| t != size) {
        return Err("file size changed on the server".into());
    }
    expected_end.checked_sub(start).and_then(|n| n.checked_add(1)).ok_or_else(|| "range overflows".to_string())
}

/// A `Content-Length` header value.
pub fn parse_content_length(h: &str) -> Option<u64> {
    h.trim().parse().ok()
}
