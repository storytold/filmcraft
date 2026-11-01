//! The same URL policy applies to command edits and explicit browser opening.

/// Return a browser-ready absolute HTTP(S) URL, or `None` for empty/invalid addresses.
/// Never pass project text directly to the host opener: projects and interchange are untrusted.
pub fn marker_web_url(value: &str) -> Option<String> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    let (_, rest) = value.split_once(':')?;
    if !rest.starts_with("//") || value.contains('\\') {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_addresses_are_parsed_and_other_handlers_are_rejected() {
        assert_eq!(marker_web_url(" HTTPS://Example.com/review?q=a%20b#shot ").as_deref(), Some("https://example.com/review?q=a%20b#shot"));
        assert_eq!(marker_web_url("http://localhost:8080/review").as_deref(), Some("http://localhost:8080/review"));
        assert!(marker_web_url("https://例え.テスト/場面").is_some());
        for bad in [
            "",
            "example.com",
            "//example.com",
            "https:",
            "https:example.com",
            "https://",
            "https://a:99999",
            "https://a\\b",
            "https://user:pass@example.com",
            "file:///tmp/a",
            "javascript:alert(1)",
            "data:text/html,test",
            "mailto:a@example.com",
            "custom://app",
            "https://exa\nmple.com",
            "https://a\0",
            "https://a\t",
        ] {
            assert_eq!(marker_web_url(bad), None, "{bad:?}");
        }
        assert!(marker_web_url(&format!("https://example.com/{}", "a".repeat(8192))).is_none());
    }
}
