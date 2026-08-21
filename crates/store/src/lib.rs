//! The application's durable truth.
//!
//! Rust alone opens this database. The engine is handed what it needs and
//! reports back, and nothing it reports is written here without passing the
//! lifecycle rules in `yarngo-core` first — which is the whole reason the two
//! are separate crates: the rules are testable without a file, and the file
//! cannot be written around them.
//!
//! No repository abstraction. Explicit SQL against `rusqlite` is shorter than
//! the machinery that would hide it, and the queries are the interesting part.

pub mod connection;
pub mod deletion;
pub mod import;
pub mod jobs;
pub mod migrations;
pub mod voices;

pub use connection::Store;
pub use import::{ImportReport, Legacy};
pub use deletion::{Conditioning, Invalidation, Outcome};
pub use voices::VoiceProvenance;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;
