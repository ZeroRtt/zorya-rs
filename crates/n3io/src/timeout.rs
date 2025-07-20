//! An extension trait to add `timeout[_with]` funcs to `futures`

use std::{
    io::{Error, Result},
    pin::Pin,
    task::Poll,
    time::{Duration, Instant},
};

use mio::Token;

use crate::reactor::Reactor;

/// An extension trait to add `timeout[_with]` funcs to `futures`
pub trait TimeoutExt: IntoFuture + Sized {
    #[cfg(feature = "global_reactor")]
    /// See [`TimeoutExt::timeout_with`]
    fn timeout(self, duration: Duration) -> Timeout<Self::IntoFuture> {
        use crate::reactor::global_reactor;

        Self::timeout_with(self, duration, global_reactor().clone())
    }

    /// Wrap `self` as a cancelable future based on `deadline` timer.
    fn timeout_with(self, duration: Duration, reactor: Reactor) -> Timeout<Self::IntoFuture> {
        let deadline = Instant::now() + duration;
        let timer = reactor.deadline(deadline);
        Timeout {
            future: Box::pin(self.into_future()),
            timer,
            reactor,
            deadline,
        }
    }
}

impl<F> TimeoutExt for F where F: IntoFuture {}

/// Future returned by [`timeout_with`](TimeoutExt::timeout_with)
pub struct Timeout<Fut> {
    future: Pin<Box<Fut>>,
    timer: Token,
    reactor: Reactor,
    deadline: Instant,
}

impl<Fut> Drop for Timeout<Fut> {
    fn drop(&mut self) {
        if let Err(err) = self.reactor.deregister_timer(self.timer) {
            log::error!("failed to deregister timer({:?}).", err);
        }
    }
}

impl<T, Fut> Future for Timeout<Fut>
where
    Fut: Future<Output = Result<T>>,
{
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
