//! Failures while reading an MMD file.
//!
//! Counts are rejected before the parser allocates the corresponding buffer.

use std::io;

/// A file could not be parsed into a model or a motion.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("failed to read the file: {0}")]
    Io(#[from] io::Error),
    #[error("the file ended before {context} was complete")]
    Truncated {
        /// What the parser was reading.
        context: &'static str,
    },
    #[error("the file is not a PMX, PMD, or VMD file")]
    BadMagic,
    #[error("unsupported {kind} version {version}")]
    UnsupportedVersion { kind: &'static str, version: String },
    #[error("{context} count {count} exceeds the limit {limit}")]
    AllocationLimit {
        /// Which array was being sized.
        context: &'static str,
        count: u64,
        limit: u64,
    },
    #[error("non-finite value in {0}")]
    NonFinite(&'static str),
    #[error("invalid {context} index {index}")]
    BadIndex {
        /// Which reference was checked.
        context: &'static str,
        index: i64,
    },
    /// Text could not be decoded with the encoding declared by the file.
    #[error("invalid text in {0}")]
    BadText(&'static str),
    #[error("invalid index size {0}")]
    BadIndexSize(u8),
    #[error("{context}: {detail}")]
    Invalid {
        context: &'static str,
        detail: &'static str,
    },
}
