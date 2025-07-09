//! error types used by this crate.

/// Error type used by this crate.
#[derive(Debug, thiserror::Error)]
pub enum N3Error {
    /// A error converted from `std::io::Error`
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// transferring pipe is broken.
    #[error("transfer pipe is broken, and transferred data({0}) before broken.")]
    BrokenPipe(usize),

    /// Error raised by quiche library.
    #[error(transparent)]
    QuicheError(#[from] quiche::Error),

    /// Unsupport quic packet.
    #[error("Unsupport quic packet {0:?}")]
    UnsupportQuicPacket(quiche::Type),

    /// Unsupport quic packet version.
    #[error("Unsupport quic version {0}")]
    UnsupportQuicVersion(u32),
}

impl PartialEq for N3Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (N3Error::Io(lhs), N3Error::Io(rhs)) if lhs.kind() == rhs.kind() => true,
            (N3Error::BrokenPipe(lhs), N3Error::BrokenPipe(rhs)) if *lhs == *rhs => true,
            (N3Error::QuicheError(lhs), N3Error::QuicheError(rhs)) if *lhs == *rhs => true,
            _ => false,
        }
    }
}

/// Result type for n3.
pub type Result<T> = std::result::Result<T, N3Error>;
