//! The updater's error type. Every failure is an `Err` the caller can show; nothing here panics.

/// Why an update step did not happen. The installed program is never touched by a failed step.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// DNS, TLS, HTTP status, timeout or a body that was cut short.
    #[error("network: {0}")]
    Network(String),
    /// The release page or checksum list is not what we expect (captive portal, renamed assets…).
    #[error("release metadata: {0}")]
    Metadata(String),
    /// The downloaded archive does not match the published SHA-256.
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    Checksum { name: String, expected: String, actual: String },
    /// The archive tries to escape its directory, contains links or devices, or is too big.
    #[error("unsafe archive: {0}")]
    UnsafeArchive(String),
    /// Self-update is not possible here (package-managed install, read-only directory, other OS…).
    #[error("{0}")]
    Unsupported(String),
    /// Another FilmCraft instance is already downloading or applying an update.
    #[error("another update is already in progress")]
    Locked,
    /// The caller's progress callback asked to stop.
    #[error("cancelled")]
    Cancelled,
    /// A file operation failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// `Result` with [`UpdateError`].
pub type Result<T> = std::result::Result<T, UpdateError>;
