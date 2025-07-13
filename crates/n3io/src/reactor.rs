//! Reactor for mio sources.

#[cfg(feature = "global_reactor")]
use std::sync::OnceLock;
use std::{
    fmt::Debug,
    io::{Error, ErrorKind, Result},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use mio::{Events, Interest, Registry, Token, event::Source};
use timing_wheel::TimeWheel;

enum IoState {
    Timeout,
    Deadline(Instant),
    Timer(u64, Waker),
    Waker(Waker),
    GroupReady(Vec<Token>),
    None,
}

impl Default for IoState {
    fn default() -> Self {
        Self::None
    }
}

struct ReactorImpl {
    /// mio `Token` generator.
    token_gen: AtomicUsize,
    /// stats for io reading ops.
    io_readable_stats: DashMap<Token, IoState>,
    /// stats for io writting ops.
    io_writable_stats: DashMap<Token, IoState>,
    /// mapping io_token => group_io_token.
    group_io_mapping: DashMap<Token, Token>,
    /// timing-wheel
    timing_wheel: Mutex<TimeWheel<(Token, bool)>>,
    /// mio registry.
    registry: Registry,
}

impl ReactorImpl {
    fn new(registry: Registry, tick_interval: Duration) -> Self {
        Self {
            token_gen: Default::default(),
            io_readable_stats: Default::default(),
            io_writable_stats: Default::default(),
            group_io_mapping: Default::default(),
            timing_wheel: Mutex::new(TimeWheel::new(tick_interval)),
            registry,
        }
    }

    fn next_token(&self, interests: Interest) -> Token {
        loop {
            let token = Token(self.token_gen.fetch_add(1, Ordering::SeqCst));

            if self.io_readable_stats.contains_key(&token)
                || self.io_writable_stats.contains_key(&token)
            {
                continue;
            }

            if interests.is_readable() {
                assert!(
                    self.io_readable_stats
                        .insert(token, IoState::None)
                        .is_none(),
                    "token will not overflow quickly."
                );
            }

            if interests.is_writable() {
                assert!(
                    self.io_writable_stats
                        .insert(token, IoState::None)
                        .is_none(),
                    "token will not overflow quickly."
                );
            }

            return token;
        }
    }
}

/// Reactor for mio sources.
#[derive(Clone)]
pub struct Reactor(Arc<ReactorImpl>);

impl Debug for Reactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reactor")
            .field("io_readable_stats", &self.0.io_readable_stats.len())
            .field("io_writable_stats", &self.0.io_writable_stats.len())
            .finish()
    }
}

impl Reactor {
    /// Create a reactor with minimum timer interval resolution `tick_interval`.
    pub fn new(tick_interval: Duration) -> Result<(Self, mio::Poll)> {
        let poll = mio::Poll::new()?;

        Ok((
            Self(Arc::new(ReactorImpl::new(
                poll.registry().try_clone()?,
                tick_interval,
            ))),
            poll,
        ))
    }

    /// Start a background poll thread.
    #[cfg(feature = "background_poll")]
    pub fn with_background_thread(
        tick_interval: Duration,
        max_poll_events: usize,
    ) -> Result<(Self, JoinHandle<Result<()>>)> {
        let (reactor, poll) = Self::new(tick_interval)?;

        let background = reactor.clone();

        let join_handle =
            std::thread::spawn(move || background.run(poll, max_poll_events, tick_interval));

        Ok((reactor, join_handle))
    }

    /// Consume and run this reactor.
    pub fn run(
        self,
        mut poll: mio::Poll,
        max_poll_events: usize,
        tick_interval: Duration,
    ) -> Result<()> {
        let mut events = Events::with_capacity(max_poll_events);

        let mut wakers = vec![];
        let mut timers = vec![];

        loop {
            events.clear();

            poll.poll(&mut events, Some(tick_interval))?;

            for event in events.iter() {
                let token = event.token();

                let (token, is_group) =
                    if let Some(group_token) = self.0.group_io_mapping.get(&token) {
                        (*group_token, true)
                    } else {
                        (token, false)
                    };

                log::trace!(
                    "poll token={:?}, is_group={}, origin_token={:?}, readable={}, writable={}",
                    token,
                    is_group,
                    event.token(),
                    event.is_readable(),
                    event.is_writable(),
                );

                if event.is_readable() {
                    if let Some(mut v) = self.0.io_readable_stats.get_mut(&token) {
                        match std::mem::take(&mut *v) {
                            IoState::Waker(waker) | IoState::Timer(_, waker) => {
                                wakers.push(waker);
                                if is_group {
                                    *v = IoState::GroupReady(vec![event.token()]);
                                }
                            }
                            IoState::GroupReady(mut tokens) => {
                                assert!(is_group);
                                tokens.push(event.token());
                                *v = IoState::GroupReady(tokens);
                            }
                            _ => {}
                        }
                    }
                }

                if event.is_writable() {
                    if let Some(mut v) = self.0.io_writable_stats.get_mut(&token) {
                        match std::mem::take(&mut *v) {
                            IoState::Waker(waker) => {
                                wakers.push(waker);

                                if is_group {
                                    *v = IoState::GroupReady(vec![event.token()]);
                                }
                            }
                            IoState::GroupReady(mut tokens) => {
                                assert!(is_group);
                                tokens.push(event.token());
                                *v = IoState::GroupReady(tokens);
                            }
                            _ => {}
                        }
                    }
                }
            }

            self.0.timing_wheel.lock().unwrap().spin(&mut timers);

            for (timer, (token, is_read)) in timers.drain(..) {
                let _wakers = if is_read {
                    &self.0.io_readable_stats
                } else {
                    &self.0.io_writable_stats
                };

                if let Some(mut v) = _wakers.get_mut(&token) {
                    match std::mem::take(&mut *v) {
                        IoState::Timer(target_timer, waker) if target_timer == timer => {
                            wakers.push(waker);
                            *v = IoState::Timeout;
                        }
                        _ => {}
                    }
                }
            }

            for waker in wakers.drain(..) {
                waker.wake();
            }
        }
    }

    /// See [`Source::register`]
    pub fn register<S>(&self, source: &mut S, intrests: Interest) -> Result<Token>
    where
        S: Source,
    {
        let token = self.0.next_token(intrests);

        source
            .register(&self.0.registry, token, intrests)
            .map(|_| token)
    }

    /// See [`Source::reregister`]
    pub fn reregister<S>(&self, source: &mut S, token: Token, intrests: Interest) -> Result<()>
    where
        S: Source,
    {
        source.reregister(&self.0.registry, token, intrests)
    }

    /// See [`Source::deregister`]
    pub fn deregister<S>(&self, source: &mut S, token: Token) -> Result<()>
    where
        S: Source,
    {
        self.0.io_readable_stats.remove(&token);
        self.0.io_writable_stats.remove(&token);
        source.deregister(&self.0.registry)
    }

    /// Create a new `deadline` timer.
    pub fn deadline(&self, deadline: Instant) -> Token {
        let token = self.0.next_token(Interest::READABLE);

        if let Some(mut stat) = self.0.io_readable_stats.get_mut(&token) {
            *stat = IoState::Deadline(deadline);
        } else {
            unreachable!("deadline token");
        }

        token
    }

    /// Deregister `deadline` timer from the given instance.
    pub fn deregister_timer(&self, timer: Token) -> Result<()> {
        self.0.io_readable_stats.remove(&timer);
        Ok(())
    }

    /// poll the timeout stats of one `deadline` timer.
    pub fn poll_timeout(&self, cx: &mut Context<'_>, timer: Token) -> Poll<Result<()>> {
        if let Some(mut stat) = self.0.io_readable_stats.get_mut(&timer) {
            match *stat {
                IoState::Deadline(deadline) => {
                    match self
                        .0
                        .timing_wheel
                        .lock()
                        .unwrap()
                        .deadline(deadline, (timer, true))
                    {
                        Some(v) => {
                            *stat = IoState::Timer(v, cx.waker().clone());
                            Poll::Pending
                        }
                        None => {
                            *stat = IoState::None;
                            Poll::Ready(Ok(()))
                        }
                    }
                }
                IoState::Timer(_, _) => Poll::Pending,
                IoState::Timeout => {
                    *stat = IoState::None;
                    Poll::Ready(Ok(()))
                }
                _ => {
                    unreachable!("call `poll_timeout` on `io source`.");
                }
            }
        } else {
            Poll::Ready(Err(Error::new(
                ErrorKind::NotFound,
                format!("can't found deadline({:?})", timer),
            )))
        }
    }

    /// Attempt to execute an operator on `io`.
    ///
    /// On success, returns Poll::Ready(Ok(num_bytes_read)).
    ///
    /// If the operator returns Error(`ErrorKind::WouldBlock`), the method returns Poll::Pending and arranges for
    /// the current task (via cx.waker().wake_by_ref()) to receive a notification when the
    /// object becomes readable or is closed.
    pub fn poll_io<F, T>(
        &self,
        cx: &mut Context<'_>,
        io: Token,
        interest: Interest,
        deadline: Option<Instant>,
        mut io_f: F,
    ) -> Poll<Result<T>>
    where
        F: FnMut(Option<Token>) -> Result<T>,
    {
        log::trace!(
            "poll_io token={:?}, interest={:?}, deadline={:?}",
            io,
            interest,
            deadline
        );

        let (wakers, is_read) = if interest.is_readable() {
            (&self.0.io_readable_stats, true)
        } else {
            (&self.0.io_writable_stats, false)
        };

        // When calling the `get_mut` method with the same key value at the same time, the later calls will be blocked.
        if let Some(mut op) = wakers.get_mut(&io) {
            loop {
                let mut wake_from = None;
                match std::mem::take(&mut *op) {
                    IoState::Timeout => {
                        return Poll::Ready(Err(Error::new(
                            ErrorKind::TimedOut,
                            format!("io({:?},{:?}) timeout.", io, interest),
                        )));
                    }
                    IoState::Timer(timer, waker) => {
                        // re-assign stat.
                        *op = IoState::Timer(timer, waker);
                    }
                    IoState::GroupReady(mut tokens) => {
                        if let Some(token) = tokens.pop() {
                            wake_from = Some(token);
                        }

                        if tokens.is_empty() {
                            *op = IoState::None;
                        } else {
                            *op = IoState::GroupReady(tokens);
                        }
                    }
                    IoState::Deadline(_) => {
                        unreachable!("call `poll_io` on `deadline`");
                    }
                    _ => {}
                }

                match io_f(wake_from) {
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        if let Some(deadline) = deadline {
                            match &*op {
                                // has pending readiness.
                                IoState::GroupReady(_) => {
                                    continue;
                                }
                                // update waker.
                                IoState::Timer(timer, _) => {
                                    *op = IoState::Timer(*timer, cx.waker().clone());
                                }
                                IoState::None => {
                                    match self
                                        .0
                                        .timing_wheel
                                        .lock()
                                        .unwrap()
                                        .deadline(deadline, (io, is_read))
                                    {
                                        Some(timer) => {
                                            *op = IoState::Timer(timer, cx.waker().clone());
                                        }
                                        None => {
                                            return Poll::Ready(Err(Error::new(
                                                ErrorKind::TimedOut,
                                                format!("io({:?},{:?}) timeout.", io, interest),
                                            )));
                                        }
                                    }
                                }
                                _ => {
                                    unreachable!("poll_io with deadline in invalid state.");
                                }
                            }
                        } else {
                            *op = IoState::Waker(cx.waker().clone());
                            log::trace!("register waker, token={:?}, wait_read_op={}", io, is_read);
                        }

                        return Poll::Pending;
                    }
                    r => return Poll::Ready(r),
                }
            }
        } else {
            Poll::Ready(Err(Error::new(
                ErrorKind::NotFound,
                format!("The io resource({:?}) is not found.", io),
            )))
        }
    }

    /// Group a set of tokens.
    pub fn group<'a, G: IntoIterator<Item = &'a Token>>(&self, tokens: G) -> Result<Token> {
        let group_token = self.0.next_token(Interest::READABLE);

        for token in tokens.into_iter() {
            assert!(
                self.0
                    .group_io_mapping
                    .insert(*token, group_token)
                    .is_none(),
                "token({:?}) group by twice.",
                token
            );
        }

        Ok(group_token)
    }

    /// ungroup a group.
    pub fn ungroup<'a, G: IntoIterator<Item = &'a Token>>(
        &self,
        group: Token,
        tokens: G,
    ) -> Result<()> {
        self.0.io_readable_stats.remove(&group);
        self.0.io_writable_stats.remove(&group);

        for token in tokens.into_iter() {
            assert!(
                self.0.group_io_mapping.remove(token).is_some(),
                "token({:?}) is not group({:?}).",
                token,
                group
            );
        }
        Ok(())
    }
}

#[cfg(feature = "global_reactor")]
mod global {
    use super::*;

    static REACTOR: OnceLock<Reactor> = OnceLock::new();
    static REACTOR_F: OnceLock<Box<dyn Fn() -> Reactor + Send + Sync + 'static>> = OnceLock::new();

    /// Fetch the global reactor instance.
    pub fn global_reactor() -> &'static Reactor {
        REACTOR.get_or_init(|| {
            if let Some(f) = REACTOR_F.get() {
                return f();
            }

            let (reactor, _) =
                Reactor::with_background_thread(Duration::from_millis(50), 1024).unwrap();

            reactor
        })
    }

    /// Set the global reactor instance.
    ///
    /// To enable this config, must call it before calling `global_reactor` for the first time.
    pub fn set_global_reactor<F>(f: F)
    where
        F: Fn() -> Reactor + Send + Sync + 'static,
    {
        assert!(
            REACTOR_F.set(Box::new(f)).is_ok(),
            "call `set_global_reactor` more than once."
        );
    }
}

#[cfg(feature = "global_reactor")]
pub use global::*;

#[cfg(feature = "global_reactor")]
#[cfg(test)]
mod tests {

    use futures_test::task::noop_context;

    use super::*;

    use std::thread::sleep;

    use mio::net::UdpSocket;

    #[test]
    fn test_timeout() {
        let mut socket = UdpSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();

        let token = global_reactor()
            .register(&mut socket, Interest::READABLE.add(Interest::WRITABLE))
            .unwrap();

        assert!(
            global_reactor()
                .poll_io(
                    &mut noop_context(),
                    token,
                    Interest::READABLE,
                    Some(Instant::now() + Duration::from_millis(100)),
                    |_| -> Result<()> { Err(Error::new(ErrorKind::WouldBlock, "")) },
                )
                .is_pending(),
        );

        sleep(Duration::from_millis(200));

        let poll = global_reactor().poll_io(
            &mut noop_context(),
            token,
            Interest::READABLE,
            Some(Instant::now() + Duration::from_millis(100)),
            |_| -> Result<()> { Err(Error::new(ErrorKind::WouldBlock, "")) },
        );

        assert!(poll.is_ready());

        if let Poll::Ready(Err(err)) = poll {
            assert_eq!(err.kind(), ErrorKind::TimedOut);
        } else {
            panic!("expect timeout");
        }
    }

    #[test]
    fn test_deadline() {
        let timer = global_reactor().deadline(Instant::now());

        assert!(
            global_reactor()
                .poll_timeout(&mut noop_context(), timer)
                .is_ready()
        );

        let timer = global_reactor().deadline(Instant::now() + Duration::from_millis(100));

        assert!(
            global_reactor()
                .poll_timeout(&mut noop_context(), timer)
                .is_pending()
        );

        sleep(Duration::from_millis(400));

        assert!(
            global_reactor()
                .poll_timeout(&mut noop_context(), timer)
                .is_ready()
        );
    }
}
