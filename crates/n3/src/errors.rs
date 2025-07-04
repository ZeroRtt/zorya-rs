//! error types used by this crate.

/// Error type used by this crate.
#[derive(Debug, thiserror::Error)]
pub enum N3Error {
    /// A error converted from `std::io::Error`
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type for n3.
pub type Result<T> = std::result::Result<T, N3Error>;
