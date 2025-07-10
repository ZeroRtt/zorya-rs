//! Error types used by n3-quic.

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
