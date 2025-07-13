//! An extension trait to add `timeout[_with]` funcs to `futures`

use std::{
    io::{Error, Result},
    task::Poll,
    time::{Duration, Instant},
};

use futures::future::{BoxFuture, FutureExt};
use mio::Token;

use crate::reactor::Reactor;

/// An extension trait to add `timeout[_with]` funcs to `futures`
pub trait TimeoutExt<'a, T>: Future<Output = Result<T>> + Sized + Send + 'a {
    #[cfg(feature = "global_reactor")]
    /// See [`TimeoutExt::timeout_with`]
    fn timeout(self, duration: Duration) -> impl Future<Output = Self::Output> {
        use crate::reactor::global_reactor;

        Self::timeout_with(self, global_reactor().clone(), duration)
    }

    /// Wrap `self` as a cancelable future based on `deadline` timer.
    fn timeout_with(
        self,
        reactor: Reactor,
        duration: Duration,
    ) -> impl Future<Output = Self::Output> {
        let deadline = Instant::now() + duration;
        let timer = reactor.deadline(deadline);
        TimeoutFuture {
            future: self.boxed(),
            timer,
            reactor,
            deadline,
        }
    }
}

impl<'a, F, T> TimeoutExt<'a, T> for F where F: Future<Output = Result<T>> + Sized + Send + 'a {}

struct TimeoutFuture<'a, T> {
    future: BoxFuture<'a, Result<T>>,
    timer: Token,
    reactor: Reactor,
    deadline: Instant,
}

impl<'a, T> Drop for TimeoutFuture<'a, T> {
    fn drop(&mut self) {
        if let Err(err) = self.reactor.deregister_timer(self.timer) {
            log::error!("failed to deregister timer({:?}).", err);
        }
    }
}

impl<'a, T> Future for TimeoutFuture<'a, T> {
    type Output = Result<T>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use futures::FutureExt;

        match self.future.poll_unpin(cx) {
            std::task::Poll::Pending => match self.reactor.poll_timeout(cx, self.timer) {
                Poll::Ready(_) => Poll::Ready(Err(Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "TimeoutExt: deadline timer({:?}) has expired.",
                        self.deadline
                    ),
                ))),
                Poll::Pending => Poll::Pending,
            },
            poll => poll,
        }
    }
}
