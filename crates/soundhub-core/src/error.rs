use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("library error: {0}")]
    Library(String),

    #[error("library not initialized at {0}")]
    LibraryNotFound(String),

    #[error("library already exists at {0}")]
    LibraryExists(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid asset id: {0}")]
    InvalidAssetId(String),

    #[error("asset not found: {0}")]
    AssetNotFound(String),

    #[error("import job not found: {0}")]
    ImportJobNotFound(String),

    #[error("duplicate asset hash {hash} (existing asset {existing_id})")]
    DuplicateAsset { hash: String, existing_id: String },

    #[error("integrity check failed for {path}: expected hash {expected}, got {actual}")]
    IntegrityMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),

    #[error("metadata parse warning for {path}: {detail}")]
    MetadataParse { path: String, detail: String },

    #[error("import cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn library(msg: impl Into<String>) -> Self {
        Self::Library(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}
