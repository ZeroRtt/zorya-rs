//! Error types used by n3-quic.

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A short for `std::result::Result<T, QuicError>`.
pub type Result<T> = std::result::Result<T, QuicError>;
