//! The single error type shared by every module of the crate.

use thiserror::Error;

/// Everything that can go wrong inside `rustscrapling`.
#[derive(Debug, Error)]
pub enum Error {
    /// A CSS/XPath-like selector could not be parsed.
    #[error("invalid selector `{selector}`: {message}")]
    Selector { selector: String, message: String },

    /// The adaptive-element storage backend failed.
    #[error("storage error: {0}")]
    Storage(String),

    /// An HTTP request failed, timed out, or was rejected.
    #[error("http error: {0}")]
    Http(String),

    /// The browser engine failed to start, navigate, or evaluate.
    #[error("browser error: {0}")]
    Browser(String),

    /// The crawl framework failed (scheduler, session, checkpoint, ...).
    #[error("spider error: {0}")]
    Spider(String),

    /// A filesystem or other I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Anything that does not fit the variants above.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Build a [`Error::Selector`] from a selector string and a message.
    pub fn selector(selector: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Selector {
            selector: selector.into(),
            message: message.into(),
        }
    }

    /// Build a [`Error::Other`] from anything displayable.
    pub fn other(message: impl std::fmt::Display) -> Self {
        Error::Other(message.to_string())
    }
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
