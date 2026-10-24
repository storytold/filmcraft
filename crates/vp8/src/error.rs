/// Errors produced by the decoder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A syntax element had a value outside its legal range, or a size did not fit the data.
    #[error("invalid bitstream: {0}")]
    Invalid(String),
    /// An inter frame came before any key frame (decoding must start at a key frame).
    #[error("missing reference frame: {0}")]
    MissingReference(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `ensure!(cond, "message")` returns `Error::Invalid` when `cond` is false.
macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::error::Error::Invalid(format!($($arg)+)));
        }
    };
}
pub(crate) use ensure;
