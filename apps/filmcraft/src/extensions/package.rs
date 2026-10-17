//! Bounded, in-memory `.fcplugin` packages. No archive entry is extracted to disk.
//!
//! The small ZIP central-directory preflight bounds metadata before the ZIP reader
//! allocates it, and catches duplicate names before a ZIP reader can coalesce them.
//! ZIP field offsets follow PKWARE APPNOTE 6.3.9, sections 4.3.12 and 4.3.16.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::net::{Ipv4Addr, Ipv6Addr};

pub const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_EXPANSION_RATIO: u64 = 200;
const PERMISSIONS: [&str; 4] = ["project.read", "playhead.write", "markers.write", "network"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub entry: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub network_origins: Vec<String>,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if !(3..=100).contains(&self.id.len())
            || !self.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            || !self.id.split('.').all(|label| {
                !label.is_empty()
                    && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                    && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            })
        {
            return Err("Extension id must contain 3–100 lowercase ASCII letters, digits, dots or hyphens, with alphanumeric label boundaries".into());
        }
        validate_asset_path(&format!("{}.fcplugin", self.id))?;
        if self.name.is_empty() || self.name.len() > 256 || self.name.trim() != self.name || self.name.chars().any(char::is_control) {
            return Err("Extension name must be non-empty, at most 256 bytes, and contain no control characters or surrounding whitespace".into());
        }
        if !valid_version(&self.version) {
            return Err("Extension version must be three unsigned decimal components, such as 1.0.0".into());
        }
        if self.api_version != 1 {
            return Err("Unsupported extension apiVersion; this host supports version 1".into());
        }
        validate_asset_path(&self.entry)?;
        if !matches!(asset_mime_type(&self.entry), Some("text/html; charset=utf-8")) {
            return Err("Extension entry must be a packaged .html or .htm file".into());
        }
        if self.permissions.len() > PERMISSIONS.len() {
            return Err("Too many extension permissions".into());
        }
        let mut permissions = BTreeSet::new();
        for permission in &self.permissions {
            if !PERMISSIONS.contains(&permission.as_str()) || !permissions.insert(permission) {
                return Err("Unknown or repeated extension permission".into());
            }
        }
        if self.network_origins.len() > 16 {
            return Err("An extension may declare at most 16 network origins".into());
        }
        if !self.network_origins.is_empty() && !self.has_permission("network") {
            return Err("networkOrigins requires the network permission".into());
        }
        if self.has_permission("network") && self.network_origins.is_empty() {
            return Err("The network permission requires at least one explicit network origin".into());
        }
        let mut origins = BTreeSet::new();
        for origin in &self.network_origins {
            validate_network_origin(origin)?;
            // The default HTTPS port denotes the same origin as an omitted port.
            if !origins.insert(origin.strip_suffix(":443").unwrap_or(origin)) {
                return Err("Repeated extension network origin".into());
            }
        }
        Ok(())
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|value| value == permission)
    }
}

fn valid_version(version: &str) -> bool {
    if version.len() > 32 {
        return false;
    }
    let mut count = 0;
    for component in version.split('.') {
        if component.is_empty()
            || !component.bytes().all(|b| b.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
            || component.parse::<u32>().is_err()
        {
            return false;
        }
        count += 1;
    }
    count == 3
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Accept a literal HTTPS origin, never a URL path, wildcard or CSP expression.
/// Hosts use lowercase ASCII DNS (punycode is allowed), canonical IPv4 or IPv6.
pub fn validate_network_origin(origin: &str) -> Result<(), String> {
    let invalid =
        || "Network origins must be explicit HTTPS origins such as https://api.example.com, without a path, wildcard, credentials or query".to_owned();
    if origin.len() > 300 || !origin.is_ascii() {
        return Err(invalid());
    }
    let authority = origin.strip_prefix("https://").ok_or_else(invalid)?;
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (address, suffix) = ipv6.split_once(']').ok_or_else(invalid)?;
        let parsed = address.parse::<Ipv6Addr>().map_err(|_| invalid())?;
        if parsed.to_string() != address {
            return Err(invalid());
        }
        let port = if suffix.is_empty() { None } else { Some(suffix.strip_prefix(':').ok_or_else(invalid)?) };
        (None, port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        (Some(host), port)
    };
    if let Some(host) = host {
        if host.is_empty() || host.len() > 253 || !host.split('.').all(valid_label) {
            return Err(invalid());
        }
        // Avoid browser IPv4 aliases such as 127.1, octal, hex or integer hosts.
        let last = host.rsplit('.').next().ok_or_else(invalid)?;
        if !last.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
            let address = host.parse::<Ipv4Addr>().map_err(|_| invalid())?;
            if address.to_string() != host {
                return Err(invalid());
            }
        }
    }
    if let Some(port) = port {
        let number = port.parse::<u16>().map_err(|_| invalid())?;
        if number == 0 || number.to_string() != port {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Paths have one portable spelling across ZIP, URL, macOS and Windows handling.
/// Percent escapes are forbidden rather than being decoded a second time.
pub fn validate_asset_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 240
        || !path.is_ascii()
        || !path.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
    {
        return Err(
            "Package paths must be relative ASCII paths with slash-separated letters, digits, underscores, hyphens and dots; no dot segments or URL escapes"
                .into(),
        );
    }
    // These basenames are device names on Windows, even with an extension.
    for part in path.split('/') {
        let stem = part.split('.').next().unwrap_or_default().to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes().last().is_some_and(|b| (b'1'..=b'9').contains(b)))
        {
            return Err("Package paths must not contain reserved device names".into());
        }
    }
    Ok(())
}

/// MIME types are explicit; unknown assets are rejected instead of sniffed.
pub fn asset_mime_type(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "js" | "mjs" => Some("text/javascript; charset=utf-8"),
        "json" | "map" => Some("application/json; charset=utf-8"),
        "txt" => Some("text/plain; charset=utf-8"),
        "svg" => Some("image/svg+xml; charset=utf-8"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "ico" => Some("image/x-icon"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Package {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err("Extension package exceeds the 32 MiB archive limit".into());
        }
        let records = preflight_zip(bytes)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("Invalid extension ZIP: {error}"))?;
        if archive.len() != records.len() || archive.offset() != 0 {
            return Err("Extension ZIP has inconsistent entries or a prepended payload".into());
        }
        if archive.has_overlapping_entries().map_err(|error| format!("Invalid ZIP entry ranges: {error}"))? {
            return Err("Overlapping ZIP entries are not allowed".into());
        }
        let mut files = BTreeMap::new();
        for record in records {
            let mut file = archive.by_name(&record.name).map_err(|error| format!("Cannot open packaged asset: {error}"))?;
            if file.encrypted()
                || file.is_symlink()
                || file.size() != record.size
                || file.compressed_size() != record.compressed_size
                || file.is_dir() != record.directory
            {
                return Err("Unsupported or inconsistent ZIP entry metadata".into());
            }
            if record.directory {
                continue;
            }
            let mut content = Vec::new();
            (&mut file).take(record.size + 1).read_to_end(&mut content).map_err(|error| format!("Cannot decode packaged asset: {error}"))?;
            if content.len() as u64 != record.size {
                return Err("Packaged asset does not match its declared size".into());
            }
            validate_asset_content(&record.name, &content)?;
            files.insert(record.name, content);
        }
        let manifest_bytes = files.get("manifest.json").ok_or("Package requires manifest.json at its root")?;
        if manifest_bytes.len() > MAX_MANIFEST_BYTES {
            return Err("Extension manifest exceeds 64 KiB".into());
        }
        let manifest: Manifest = serde_json::from_slice(manifest_bytes).map_err(|error| format!("Invalid extension manifest: {error}"))?;
        manifest.validate()?;
        if !files.contains_key(&manifest.entry) {
            return Err("The extension entry file is missing from its package".into());
        }
        Ok(Self { manifest, files })
    }
}

fn validate_asset_content(path: &str, bytes: &[u8]) -> Result<(), String> {
    let mime = asset_mime_type(path).ok_or("Unsupported packaged asset type")?;
    if mime.ends_with("charset=utf-8") {
        let text = std::str::from_utf8(bytes).map_err(|_| "Packaged text assets must be UTF-8")?;
        if text.contains('\0') {
            return Err("Packaged text assets must not contain NUL characters".into());
        }
        if mime.starts_with("application/json") {
            serde_json::from_str::<serde_json::Value>(text).map_err(|error| format!("Invalid packaged JSON: {error}"))?;
        }
    }
    Ok(())
}

struct ZipRecord {
    name: String,
    directory: bool,
    size: u64,
    compressed_size: u64,
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], String> {
    let end = offset.checked_add(N).ok_or("ZIP offset overflow")?;
    bytes.get(offset..end).ok_or("Truncated ZIP metadata")?.try_into().map_err(|_| "Truncated ZIP metadata".into())
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(field(bytes, offset)?))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(field(bytes, offset)?))
}

fn validate_local_header(bytes: &[u8], offset: usize, directory_start: usize, central: &[u8; 46], name: &str) -> Result<(), String> {
    let local: [u8; 30] = field(bytes, offset)?;
    if offset >= directory_start
        || field::<4>(&local, 0)? != *b"PK\x03\x04"
        || u16_at(&local, 6)? != u16_at(central, 8)?
        || u16_at(&local, 8)? != u16_at(central, 10)?
    {
        return Err("Inconsistent ZIP local header".into());
    }
    let name_len = usize::from(u16_at(&local, 26)?);
    let extra_len = usize::from(u16_at(&local, 28)?);
    if name_len != name.len() || extra_len > 4096 {
        return Err("Inconsistent or oversized ZIP local metadata".into());
    }
    let name_start = offset.checked_add(30).ok_or("ZIP offset overflow")?;
    let name_end = name_start.checked_add(name_len).ok_or("ZIP offset overflow")?;
    if bytes.get(name_start..name_end) != Some(name.as_bytes()) {
        return Err("ZIP local and central asset names differ".into());
    }
    let compressed = usize::try_from(u32_at(central, 20)?).map_err(|_| "ZIP size overflow")?;
    let data_end = name_end.checked_add(extra_len).and_then(|v| v.checked_add(compressed)).ok_or("ZIP offset overflow")?;
    if data_end > directory_start {
        return Err("ZIP asset overlaps the central directory".into());
    }
    // Streaming ZIP writers may defer these values to a data descriptor.
    if u16_at(&local, 6)? & 8 == 0
        && (u32_at(&local, 14)? != u32_at(central, 16)? || u32_at(&local, 18)? != u32_at(central, 20)? || u32_at(&local, 22)? != u32_at(central, 24)?)
    {
        return Err("ZIP local and central sizes or checksums differ".into());
    }
    Ok(())
}

fn preflight_zip(bytes: &[u8]) -> Result<Vec<ZipRecord>, String> {
    // ZIP64 and split archives are unnecessary for this deliberately small format.
    let last = bytes.len().checked_sub(22).ok_or("Truncated extension ZIP")?;
    let first = last.saturating_sub(u16::MAX as usize);
    let mut end_record = None;
    for offset in (first..=last).rev() {
        if field::<4>(bytes, offset)? == *b"PK\x05\x06"
            && offset.checked_add(22).and_then(|end| end.checked_add(usize::from(u16_at(bytes, offset + 20).ok()?))) == Some(bytes.len())
        {
            end_record = Some(offset);
            break;
        }
    }
    let end_record = end_record.ok_or("Extension ZIP end record is missing or has trailing data")?;
    let count = usize::from(u16_at(bytes, end_record + 10)?);
    if count == 0 || count > MAX_ENTRIES {
        return Err("Extension ZIP must contain 1–256 entries".into());
    }
    if u16_at(bytes, end_record + 4)? != 0 || u16_at(bytes, end_record + 6)? != 0 || usize::from(u16_at(bytes, end_record + 8)?) != count {
        return Err("Split or ZIP64 extension packages are not supported".into());
    }
    let directory_size = usize::try_from(u32_at(bytes, end_record + 12)?).map_err(|_| "Invalid ZIP directory size")?;
    let mut offset = usize::try_from(u32_at(bytes, end_record + 16)?).map_err(|_| "Invalid ZIP directory offset")?;
    if offset.checked_add(directory_size) != Some(end_record) || directory_size > 1024 * 1024 {
        return Err("Invalid or oversized ZIP central directory".into());
    }
    let directory_start = offset;
    let mut records = Vec::new();
    let mut names = BTreeMap::new();
    let mut total_size = 0_u64;
    for _ in 0..count {
        let header: [u8; 46] = field(bytes, offset)?;
        if field::<4>(&header, 0)? != *b"PK\x01\x02" {
            return Err("Invalid ZIP central-directory record".into());
        }
        let flags = u16_at(&header, 8)?;
        if flags & !0x080e != 0 || !matches!(u16_at(&header, 10)?, 0 | 8) || u16_at(&header, 34)? != 0 {
            return Err("Only unencrypted, single-disk Stored or Deflate ZIP entries are supported".into());
        }
        let name_len = usize::from(u16_at(&header, 28)?);
        let extra_len = usize::from(u16_at(&header, 30)?);
        let comment_len = usize::from(u16_at(&header, 32)?);
        if name_len == 0 || name_len > 241 || extra_len > 4096 || comment_len > 1024 {
            return Err("ZIP entry metadata exceeds package limits".into());
        }
        let name_start = offset.checked_add(46).ok_or("ZIP offset overflow")?;
        let name_end = name_start.checked_add(name_len).ok_or("ZIP offset overflow")?;
        let name = std::str::from_utf8(bytes.get(name_start..name_end).ok_or("Truncated ZIP name")?).map_err(|_| "Package paths must be UTF-8 ASCII")?;
        let directory = name.ends_with('/');
        let path = if directory { name.strip_suffix('/').ok_or("Invalid ZIP directory")? } else { name };
        validate_asset_path(path)?;
        if !directory && asset_mime_type(path).is_none() {
            return Err("Package contains an unsupported asset type".into());
        }
        if names.insert(path.to_ascii_lowercase(), directory).is_some() {
            return Err("Duplicate or case-colliding ZIP entry names are not allowed".into());
        }
        let mode = u32_at(&header, 38)? >> 16;
        let kind = mode & 0o170000;
        if !matches!(kind, 0 | 0o040000 | 0o100000) || (kind == 0o040000 && !directory) || (kind == 0o100000 && directory) {
            return Err("ZIP symlinks and special files are not allowed".into());
        }
        let size = u64::from(u32_at(&header, 24)?);
        let compressed_size = u64::from(u32_at(&header, 20)?);
        if size > MAX_FILE_BYTES as u64
            || (directory && size != 0)
            || size > compressed_size.saturating_mul(MAX_EXPANSION_RATIO)
            || compressed_size > bytes.len() as u64
        {
            return Err("ZIP entry exceeds the 8 MiB size or 200:1 expansion limit".into());
        }
        if name == "manifest.json" && size > MAX_MANIFEST_BYTES as u64 {
            return Err("Extension manifest exceeds 64 KiB".into());
        }
        total_size = total_size.checked_add(size).ok_or("ZIP size overflow")?;
        if total_size > MAX_ARCHIVE_BYTES as u64 {
            return Err("Extension assets exceed the 32 MiB total limit".into());
        }
        let local_offset = usize::try_from(u32_at(&header, 42)?).map_err(|_| "Invalid ZIP local offset")?;
        validate_local_header(bytes, local_offset, directory_start, &header, name)?;
        records.push(ZipRecord { name: name.to_owned(), directory, size, compressed_size });
        offset = name_end.checked_add(extra_len).and_then(|v| v.checked_add(comment_len)).ok_or("ZIP offset overflow")?;
        if offset > end_record {
            return Err("Truncated ZIP central directory".into());
        }
    }
    if offset != end_record {
        return Err("ZIP entry count does not match its central directory".into());
    }
    for name in names.keys() {
        for (offset, _) in name.match_indices('/') {
            if names.get(name.get(..offset).ok_or("Invalid asset path boundary")?) == Some(&false) {
                return Err("A package file cannot also be an asset directory".into());
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn manifest() -> serde_json::Value {
        serde_json::json!({"id":"org.example.review","name":"Review panel","version":"1.0.0","apiVersion":1,"entry":"index.html","permissions":["project.read"],"networkOrigins":[]})
    }

    fn archive(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer.start_file(*name, SimpleFileOptions::default().compression_method(method)).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn package_with(manifest: serde_json::Value, extras: &[(&str, &[u8])]) -> Vec<u8> {
        let json = serde_json::to_vec(&manifest).unwrap();
        let mut entries = vec![("manifest.json", json.as_slice()), ("index.html", b"<!doctype html><title>Review</title>".as_slice())];
        entries.extend_from_slice(extras);
        archive(&entries, zip::CompressionMethod::Stored)
    }

    fn central_offsets(bytes: &[u8]) -> Vec<usize> {
        bytes.windows(4).enumerate().filter_map(|(index, value)| (value == b"PK\x01\x02").then_some(index)).collect()
    }

    #[test]
    fn minimal_and_deflated_packages_load_without_extraction() {
        let bytes = package_with(manifest(), &[("assets/app.js", b"console.log('ready')")]);
        let parsed = Package::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.manifest.id, "org.example.review");
        assert!(parsed.manifest.has_permission("project.read"));
        assert!(!parsed.manifest.has_permission("network"));
        assert_eq!(parsed.files.len(), 3);
        let json = serde_json::to_vec(&manifest()).unwrap();
        let deflated = archive(&[("manifest.json", &json), ("index.html", b"<title>Panel</title>")], zip::CompressionMethod::Deflated);
        assert!(Package::from_bytes(&deflated).is_ok());
    }

    #[test]
    fn manifest_is_strict_and_versioned() {
        for (key, value) in [
            ("apiVersion", serde_json::json!(2)),
            ("apiVersion", serde_json::json!(-1)),
            ("apiVersion", serde_json::json!(1.5)),
            ("unexpected", serde_json::json!(true)),
            ("id", serde_json::json!("../panel")),
            ("id", serde_json::json!("Org.Panel")),
            ("id", serde_json::json!("org..panel")),
            ("id", serde_json::json!("ab")),
            ("name", serde_json::json!("bad\nname")),
            ("version", serde_json::json!("1.0")),
            ("version", serde_json::json!("1.0.0.0")),
            ("version", serde_json::json!("01.0.0")),
            ("version", serde_json::json!("4294967296.0.0")),
            ("entry", serde_json::json!("app.js")),
            ("entry", serde_json::json!("missing.html")),
            ("permissions", serde_json::json!(["filesystem.read"])),
            ("permissions", serde_json::json!(["project.read", "project.read"])),
        ] {
            let mut value_manifest = manifest();
            value_manifest[key] = value;
            assert!(Package::from_bytes(&package_with(value_manifest, &[])).is_err(), "{key}");
        }
        let mut missing = manifest();
        missing.as_object_mut().unwrap().remove("apiVersion");
        assert!(Package::from_bytes(&package_with(missing, &[])).is_err());
    }

    #[test]
    fn network_is_explicit_and_cannot_inject_csp() {
        for origin in ["https://api.example.com", "https://api.example.com:8443", "https://127.0.0.1", "https://[::1]:9443", "https://xn--bcher-kva.example"] {
            assert!(validate_network_origin(origin).is_ok(), "{origin}");
        }
        for origin in [
            "http://api.example.com",
            "https://*.example.com",
            "https://example.com/",
            "https://example.com/a",
            "https://user@example.com",
            "https://example.com?x",
            "https://example.com#x",
            "https://example.com;script-src *",
            "https://example.com\n",
            "https://EXAMPLE.com",
            "https://example.com:0",
            "https://example.com:65536",
            "https://example.com:0443",
            "https://example.com:",
            "https://example.com:443:80",
            "https://127.1",
            "https://0177.0.0.1",
            "https://0x7f000001",
            "https://[::1%25en0]",
            "https://[::1]/",
            "https://example..com",
            "https://-bad.example",
        ] {
            assert!(validate_network_origin(origin).is_err(), "{origin}");
        }
        let mut value = manifest();
        value["networkOrigins"] = serde_json::json!(["https://api.example.com"]);
        assert!(Package::from_bytes(&package_with(value.clone(), &[])).is_err());
        value["permissions"] = serde_json::json!(["network"]);
        assert!(Package::from_bytes(&package_with(value.clone(), &[])).is_ok());
        value["networkOrigins"] = serde_json::json!(["https://api.example.com", "https://api.example.com:443"]);
        assert!(Package::from_bytes(&package_with(value, &[])).is_err());
    }

    #[test]
    fn zip_slip_and_ambiguous_names_are_rejected() {
        for name in [
            "../escape.js",
            "/absolute.js",
            "C:/drive.js",
            "dir\\escape.js",
            "a/../escape.js",
            "a/./file.js",
            "a//file.js",
            "%2e%2e/escape.js",
            "a%2fb.js",
            "a.js?x",
            "a.js#x",
            "bad\0.js",
            "a./file.js",
            "CON.js",
            "a/LPT1.css",
            "한글.js",
        ] {
            assert!(Package::from_bytes(&package_with(manifest(), &[(name, b"x")])).is_err(), "{name:?}");
        }
        assert!(Package::from_bytes(&package_with(manifest(), &[("INDEX.html", b"x")])).is_err());
        assert!(Package::from_bytes(&package_with(manifest(), &[("a.js", b"x"), ("a.js/file.js", b"x")])).is_err());
        // ZipWriter rejects duplicate names itself; mutate an equal-length entry name.
        let mut duplicate = package_with(manifest(), &[("other.html", b"x")]);
        for position in duplicate.windows(10).enumerate().filter_map(|(i, b)| (b == b"other.html").then_some(i)).collect::<Vec<_>>() {
            duplicate[position..position + 10].copy_from_slice(b"index.html");
        }
        assert!(Package::from_bytes(&duplicate).is_err());
    }

    #[test]
    fn symlinks_special_files_and_encryption_are_rejected() {
        for mode in [0o120777_u32, 0o020666, 0o060666, 0o140777] {
            let mut bytes = package_with(manifest(), &[]);
            let offset = central_offsets(&bytes)[1];
            bytes[offset + 5] = 3; // Unix originating system.
            bytes[offset + 38..offset + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            assert!(Package::from_bytes(&bytes).is_err());
        }
        let mut encrypted = package_with(manifest(), &[]);
        let offset = central_offsets(&encrypted)[0];
        encrypted[offset + 8] |= 1;
        assert!(Package::from_bytes(&encrypted).is_err());
    }

    #[test]
    fn bombs_and_hostile_metadata_are_rejected_before_decompression() {
        let repeated = vec![b'x'; 1024 * 1024];
        let json = serde_json::to_vec(&manifest()).unwrap();
        let bomb = archive(&[("manifest.json", &json), ("index.html", &repeated)], zip::CompressionMethod::Deflated);
        assert!(Package::from_bytes(&bomb).unwrap_err().contains("expansion"));
        for (field_offset, value) in [(24, u32::MAX), (20, u32::MAX), (42, u32::MAX)] {
            let mut bytes = package_with(manifest(), &[]);
            let offset = central_offsets(&bytes)[0];
            bytes[offset + field_offset..offset + field_offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(Package::from_bytes(&bytes).is_err());
        }
        let mut entries = package_with(manifest(), &[]);
        let end = entries.len() - 22;
        entries[end + 8..end + 12].copy_from_slice(&[0xff; 4]);
        assert!(Package::from_bytes(&entries).is_err());
        let oversized = vec![0; MAX_ARCHIVE_BYTES + 1];
        assert!(Package::from_bytes(&oversized).is_err());
    }

    #[test]
    fn malformed_and_unsupported_assets_are_rejected() {
        for (name, data) in [("bad.js", b"\xff".as_slice()), ("bad.css", b"a\0b"), ("bad.json", b"{"), ("native.exe", b"binary"), ("native.wasm", b"binary")] {
            assert!(Package::from_bytes(&package_with(manifest(), &[(name, data)])).is_err(), "{name}");
        }
        assert!(Package::from_bytes(&archive(&[("index.html", b"<title>No manifest</title>")], zip::CompressionMethod::Stored)).is_err());
        let mut bytes = package_with(manifest(), &[]);
        bytes.extend_from_slice(b"trailing payload");
        assert!(Package::from_bytes(&bytes).is_err());
    }

    #[test]
    fn truncated_and_mutated_packages_never_panic() {
        let original = package_with(manifest(), &[("app.js", b"console.log('ready')")]);
        for end in 0..original.len() {
            assert!(std::panic::catch_unwind(|| Package::from_bytes(&original[..end])).is_ok(), "truncation {end}");
        }
        for offset in 0..original.len() {
            for mask in [1, 0x80, 0xff] {
                let mut bytes = original.clone();
                bytes[offset] ^= mask;
                assert!(std::panic::catch_unwind(|| Package::from_bytes(&bytes)).is_ok(), "mutation {offset}/{mask}");
            }
        }
    }
}
