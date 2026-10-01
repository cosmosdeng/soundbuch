//! soundbuch core library.
//!
//! Architecture (PRD §52): **Original Asset → Metadata → Relationships → Index**
//! are kept separate. Virtual Collections never copy files. Filename is never
//! the asset identity.

pub mod audio;
pub mod collections;
pub mod db;
pub mod duplicates;
pub mod error;
pub mod ids;
pub mod import;
pub mod library;
pub mod metadata;
pub mod models;
pub mod search;
pub mod undo;

pub use error::{Error, Result};
pub use ids::{AssetId, RowId};
pub use library::Library;
pub use models::*;
