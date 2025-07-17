//! Group a set of `IO` objects.

use std::{
    io::Result,
    task::{Context, Poll},
    time::Instant,
};

use mio::{Interest, Token};

use crate::reactor::Reactor;

/// A set of io items.
#[derive(Debug)]
pub struct Group {
    pub group_token: Token,
    children: Vec<Token>,
    pub reactor: Reactor,
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Err(err) = self.reactor.ungroup(self.group_token, &self.children) {
            log::error!("failed to ungroup {:?}, {}", self.group_token, err);
        }
    }
}

impl Group {
    /// Create a group from `Vec<Token>`
    pub fn new(children: Vec<Token>, reactor: Reactor) -> Result<Self> {
        let group_token = reactor.group(&children)?;

        Ok(Self {
            group_token,
            children,
            reactor,
        })
    }

    /// A wrapper for [`Reactor::poll_io`]
    pub fn poll_io<F, T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        deadline: Option<Instant>,
        io_f: F,
    ) -> Poll<Result<T>>
    where
        F: FnMut(Token) -> Result<T>,
    {
        self.reactor
            .poll_io(cx, self.group_token, interest, deadline, io_f)
    }
}
