use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum H5iError {
    /// 1. Temporal Dimension (History): Git operations
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    /// Intentional Dimension (Spirit): Metadata and AI provenance
    #[error("Metadata error: {0}")]
    Metadata(String),

    /// 4. Empirical Dimension (Quality): Tests and coverage
    #[error("Quality tracking error: {0}")]
    Quality(String),

    /// Standard I/O error (Enables use of '?' on std::io::Result)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Contextual I/O error (For when we want to track the specific file path)
    #[error("I/O error at {path}: {source}")]
    IoWithContext {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid repository path or structure: {0}")]
    InvalidPath(String),

    #[error("H5i record not found for commit: {0}")]
    RecordNotFound(String),

    #[error("Internal h5i error: {0}")]
    Internal(String),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
}

impl H5iError {
    /// Helper to attach path context to an I/O error
    pub fn with_path(source: std::io::Error, path: impl Into<PathBuf>) -> Self {
        H5iError::IoWithContext {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, H5iError>;
