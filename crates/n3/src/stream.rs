//! The abstraction for mio non-blocking bidirectional stream.

use mio::{Interest, Registry, Token};

use crate::errors::Result;

/// mio `Stream` object.
pub trait Stream {
    /// Returns bound poll token.
    fn token(&self) -> Token;
    /// Register stream with the Poll instance.
    ///
    /// See [`mio doc`](https://docs.rs/mio/1.0.4/mio/struct.Registry.html#method.register) for more information.
    fn register(&mut self, reigster: &Registry, interests: Interest) -> Result<()>;

    /// Re-register an event::Source with the Poll instance.
    ///
    /// See [`mio doc`](https://docs.rs/mio/1.0.4/mio/struct.Registry.html#method.reregister) for more information.
    fn reregister(&mut self, reigster: &Registry, interests: Interest) -> Result<()>;

    /// Deregister stream with the Poll instance.
    ///
    /// See [`mio doc`](https://docs.rs/mio/1.0.4/mio/struct.Registry.html#method.deregister) for more information.
    fn deregister(&mut self, reigster: &Registry) -> Result<()>;
}
