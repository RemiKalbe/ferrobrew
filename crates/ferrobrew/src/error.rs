//! ferrobrew's error type.

use std::fmt;
use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, FerroError>;

/// All the ways a ferrobrew operation can fail.
#[derive(Debug)]
pub enum FerroError {
    /// An I/O error, annotated with the path it concerned where known.
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
    /// A required environment variable was missing.
    MissingEnv(&'static str),
    /// A downloaded artifact's checksum did not match the expected value.
    ChecksumMismatch { expected: String, actual: String },
    /// The requested formula or resource could not be found.
    NotFound(String),
    /// ferrobrew does not yet implement this path; carries a human-readable reason.
    ///
    /// Unlike the abandoned Ruby-frontend design, this does not silently defer to `brew` — it
    /// surfaces to the user so missing parity is visible rather than hidden.
    Unsupported(String),
    /// A catch-all for higher-level failures with a message.
    Other(String),
}

impl FerroError {
    /// Attach a path to an I/O error for friendlier messages.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        FerroError::Io {
            path: Some(path.into()),
            source,
        }
    }
}

impl fmt::Display for FerroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FerroError::Io {
                path: Some(p),
                source,
            } => write!(f, "{}: {source}", p.display()),
            FerroError::Io { path: None, source } => write!(f, "{source}"),
            FerroError::MissingEnv(var) => {
                write!(f, "required environment variable {var} is not set")
            }
            FerroError::ChecksumMismatch { expected, actual } => {
                write!(f, "checksum mismatch: expected {expected}, got {actual}")
            }
            FerroError::NotFound(what) => write!(f, "not found: {what}"),
            FerroError::Unsupported(reason) => {
                write!(f, "not yet supported by ferrobrew: {reason}")
            }
            FerroError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for FerroError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FerroError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for FerroError {
    fn from(source: std::io::Error) -> Self {
        FerroError::Io { path: None, source }
    }
}
