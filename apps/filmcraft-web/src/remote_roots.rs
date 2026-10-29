//! Top-level virtual folders that belong to the app: a remote mount may not shadow them.

/// First path segments (without the slash) a mount `path` may not start with.
pub const RESERVED_ROOTS: &[&str] = &["files", "projects", "exports", "opfs", "recovered"];
