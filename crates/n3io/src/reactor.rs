//! Reactor pattern based on mio.

#[cfg(feature = "global_reactor")]
use std::sync::OnceLock;
use std::{
    collections::VecDeque,
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

#[derive(Debug)]
enum IoState {
    Timeout,
    Deadline(Instant),
    Timer(u64, Waker),
    Waker(Waker),
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
    group_children: DashMap<Token, Token>,
    /// Group token set.
    group_token_set: DashMap<Token, VecDeque<Token>>,
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
            group_children: Default::default(),
            group_token_set: Default::default(),
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

                let (token, is_group) = if let Some(group_token) = self.0.group_children.get(&token)
                {
                    (*group_token, true)
                } else {
                    (token, false)
                };

                log::trace!(
                    "Reactor(background) rasied event, token={:?}, is_group={}, origin_token={:?}, readable={}, writable={}",
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
                            }
                            _ => {}
                        }

                        if is_group {
                            if let Some(mut ready_queue) = self.0.group_token_set.get_mut(&token) {
                                log::trace!(
                                    "Reactor(background) append ready events to group, group={:?}, token={:?}, interest=Readable",
                                    token,
                                    event.token(),
                                );

                                ready_queue.push_back(event.token());
                            }
                        }
                    }
                }

                if event.is_writable() {
                    if let Some(mut v) = self.0.io_writable_stats.get_mut(&token) {
                        match std::mem::take(&mut *v) {
                            IoState::Waker(waker) => {
                                wakers.push(waker);
                            }
                            _ => {}
                        }

                        if is_group {
                            if let Some(mut ready_queue) = self.0.group_token_set.get_mut(&token) {
                                log::trace!(
                                    "Reactor(background) append ready events to group, group={:?}, token={:?}, interest=Writable",
                                    token,
                                    event.token(),
                                );

                                ready_queue.push_back(event.token());
                            }
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
        F: FnMut(Token) -> Result<T>,
    {
        let (stats, is_read) = if interest.is_readable() {
            (&self.0.io_readable_stats, true)
        } else {
            (&self.0.io_writable_stats, false)
        };

        log::trace!("Reactor(poll_io): poll resource, token={:?}", io,);

        if let Some(mut stat) = stats.get_mut(&io) {
            if let Some(mut ready_queue) = self.0.group_token_set.get_mut(&io) {
                log::trace!(
                    "Reactor(poll_io): poll group resource, token={:?}, readiness={}",
                    io,
                    ready_queue.len()
                );

                assert_eq!(deadline, None, "Call `poll_io` on group with `deadline`.");

                while let Some(next) = ready_queue.pop_front() {
                    match io_f(next) {
                        Ok(r) => return Poll::Ready(Ok(r)),
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(err) => return Poll::Ready(Err(err)),
                    }
                }

                log::trace!(
                    "Reactor(poll_io): poll group resource, token={:?}, pending",
                    io,
                );

                *stat = IoState::Waker(cx.waker().clone());
                return Poll::Pending;
            }

            match std::mem::take(&mut *stat) {
                IoState::Timeout => {
                    return Poll::Ready(Err(Error::new(
                        ErrorKind::TimedOut,
                        format!("Reactor(poll_io): io timeout, token={:?}", io),
                    )));
                }
                IoState::Timer(timer, waker) => match io_f(io) {
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        *stat = IoState::Timer(timer, waker);
                        return Poll::Pending;
                    }
                    r => return Poll::Ready(r),
                },
                // may wakeup by timer.
                IoState::Waker(_) | IoState::None => match io_f(io) {
                    Err(err) if err.kind() == ErrorKind::WouldBlock => {
                        if let Some(deadline) = deadline {
                            match self
                                .0
                                .timing_wheel
                                .lock()
                                .unwrap()
                                .deadline(deadline, (io, is_read))
                            {
                                Some(v) => {
                                    *stat = IoState::Timer(v, cx.waker().clone());
                                    return Poll::Pending;
                                }
                                None => {
                                    return Poll::Ready(Err(Error::new(
                                        ErrorKind::TimedOut,
                                        format!("Reactor(poll_io): io timeout, token={:?}", io),
                                    )));
                                }
                            }
                        }

                        *stat = IoState::Waker(cx.waker().clone());
                        return Poll::Pending;
                    }
                    r => return Poll::Ready(r),
                },
                stat => {
                    unreachable!("Reactor(poll_io): unhandle state, state={:?}", stat);
                }
            }
        }

        log::error!("Reactor(poll_io): resource is not found, token={:?}", io);

        return Poll::Ready(Err(Error::new(
            ErrorKind::NotFound,
            format!("poll_io: resource is not found"),
        )));
    }

    /// Group a set of tokens.
    pub fn group<'a, G: IntoIterator<Item = &'a Token>>(&self, tokens: G) -> Result<Token> {
        let group_token = self.0.next_token(Interest::READABLE);

        self.0
            .group_token_set
            .insert(group_token, Default::default());

        for token in tokens.into_iter() {
            assert!(
                self.0.group_children.insert(*token, group_token).is_none(),
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

        self.0.group_token_set.remove(&group);

        for token in tokens.into_iter() {
            assert!(
                self.0.group_children.remove(token).is_some(),
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
