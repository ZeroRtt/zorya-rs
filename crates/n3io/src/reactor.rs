//! Reactor pattern based on mio.

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
    thread::sleep,
    time::{Duration, Instant},
};

use dashmap::DashMap;
use mio::{Events, Interest, Registry, Token, event::Source};
use timing_wheel::TimeWheel;

#[derive(Debug)]
enum IoState {
    Timeout,
    Deadline(Instant),
    Timer(Waker),
    Waker(Waker),
    Ready,
    None,
    Shutdown,
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
    /// timing-wheel
    timing_wheel: Mutex<TimeWheel<Token>>,
    /// mio registry.
    registry: Registry,
}

impl ReactorImpl {
    fn new(registry: Registry, tick_interval: Duration) -> Self {
        Self {
            token_gen: Default::default(),
            io_readable_stats: Default::default(),
            io_writable_stats: Default::default(),
            timing_wheel: Mutex::new(TimeWheel::new(tick_interval)),
            registry,
        }
    }

    fn drop_token(&self, token: Token, interests: Interest) {
        if interests.is_readable() {
            self.io_readable_stats.remove(&token);
        }

        if interests.is_writable() {
            self.io_writable_stats.remove(&token);
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

#[allow(unused)]
impl Reactor {
    /// Create a reactor with minimum timer interval resolution `tick_interval`.
    pub fn new(max_poll_events: usize, tick_interval: Duration) -> Result<Self> {
        let poll = mio::Poll::new()?;

        let reactor = Arc::new(ReactorImpl::new(
            poll.registry().try_clone()?,
            tick_interval,
        ));

        let this = reactor.clone();

        std::thread::spawn(move || {
            if let Err(err) = Self::io_events_loop(this, poll, max_poll_events) {
                log::error!("io_events_loop stopped,{}", err);
            }
        });

        let this = reactor.clone();

        std::thread::spawn(move || Self::timing_wheel_ticks(this, tick_interval));

        Ok(Self(reactor))
    }

    fn io_events_loop(
        this: Arc<ReactorImpl>,
        mut poll: mio::Poll,
        max_poll_events: usize,
    ) -> Result<()> {
        let mut events = Events::with_capacity(max_poll_events);
        loop {
            poll.poll(&mut events, None)?;

            for event in events.iter() {
                log::trace!(
                    "event raised, token={:?}, readable={}, writable={}, read_closed={}, write_closed={}",
                    event.token(),
                    event.is_readable(),
                    event.is_writable(),
                    event.is_read_closed(),
                    event.is_write_closed(),
                );

                if event.is_read_closed() || event.is_readable() {
                    if let Some(mut stat) = this.io_readable_stats.get_mut(&event.token()) {
                        match &*stat {
                            IoState::Waker(waker) => {
                                log::trace!(
                                    "call waker::wake(), token={:?}, readable",
                                    event.token(),
                                );
                                waker.wake_by_ref();
                                *stat = IoState::Ready;
                                drop(stat);
                            }
                            IoState::None => {
                                *stat = IoState::Ready;
                            }
                            IoState::Shutdown | IoState::Ready => {}
                            _ => {
                                unreachable!(
                                    "Invalid readable state, token={:?}, stat={:?}",
                                    event.token(),
                                    *stat
                                );
                            }
                        }
                    } else {
                        log::warn!("readable source is deregister, token={:?}", event.token());
                    }
                }

                if event.is_write_closed() || event.is_writable() {
                    if let Some(mut stat) = this.io_writable_stats.get_mut(&event.token()) {
                        match &*stat {
                            IoState::Waker(waker) => {
                                log::trace!(
                                    "call waker::wake(), token={:?}, writable",
                                    event.token(),
                                );

                                waker.wake_by_ref();
                                *stat = IoState::Ready;
                                drop(stat);
                            }
                            IoState::None => {
                                *stat = IoState::Ready;
                            }
                            IoState::Shutdown | IoState::Ready => {}
                            _ => {
                                unreachable!(
                                    "Invalid writable state, token={:?}, stat={:?}",
                                    event.token(),
                                    *stat
                                );
                            }
                        }
                    } else {
                        log::warn!("writable source is deregister, token={:?}", event.token());
                    }
                }
            }

            events.clear();
        }
    }

    fn timing_wheel_ticks(this: Arc<ReactorImpl>, tick_interval: Duration) {
        let mut wakers = vec![];
        loop {
            this.timing_wheel.lock().unwrap().spin(&mut wakers);

            for token in wakers.drain(..) {
                if let Some(mut stat) = this.io_readable_stats.get_mut(&token) {
                    let old = std::mem::replace(&mut *stat, IoState::Timeout);
                    drop(stat);

                    match old {
                        IoState::Timer(waker) => {
                            log::trace!("raise timeout event, token={:?}", token);
                            waker.wake();
                        }
                        _ => {
                            unreachable!("Invalid timer state, token={:?}, stat={:?}", token, old)
                        }
                    }
                }
            }

            sleep(tick_interval);
        }
    }

    /// See [`Source::register`]
    pub fn register<S>(&self, source: &mut S, interests: Interest) -> Result<Token>
    where
        S: Source,
    {
        let token = self.0.next_token(interests);

        match self.0.registry.register(source, token, interests) {
            Ok(_) => Ok(token),
            Err(err) => {
                self.0.drop_token(token, interests);
                Err(err)
            }
        }
    }

    /// See [`Source::reregister`]
    pub fn reregister<S>(&self, source: &mut S, token: Token, interests: Interest) -> Result<()>
    where
        S: Source,
    {
        self.0.registry.reregister(source, token, interests)
    }

    /// See [`Source::deregister`]
    pub fn deregister<S>(&self, source: &mut S, token: Token) -> Result<()>
    where
        S: Source,
    {
        self.0
            .drop_token(token, Interest::READABLE.add(Interest::WRITABLE));
        self.0.registry.deregister(source)
    }

    /// Create a new `deadline` timer.
    pub fn deadline(&self, deadline: Instant) -> Token {
        let token = self.0.next_token(Interest::READABLE);

        *self
            .0
            .io_readable_stats
            .get_mut(&token)
            .expect("readable stat") = IoState::Deadline(deadline);

        token
    }

    /// Deregister `deadline` timer from the given instance.
    pub fn deregister_timer(&self, timer: Token) {
        self.0.drop_token(timer, Interest::READABLE);
    }

    /// poll the timeout stats of one `deadline` timer.
    pub fn poll_timeout(&self, cx: &mut Context<'_>, timer: Token) -> Poll<Result<()>> {
        if let Some(mut state) = self.0.io_readable_stats.get_mut(&timer) {
            match &mut *state {
                IoState::Timeout => {
                    // don't change the state.
                    return Poll::Ready(Ok(()));
                }
                IoState::Deadline(instant) => {
                    let deadline = *instant;

                    *state = IoState::Timer(cx.waker().clone());

                    drop(state);

                    let ticks = self
                        .0
                        .timing_wheel
                        .lock()
                        .unwrap()
                        .deadline(deadline, timer);

                    let mut state = self
                        .0
                        .io_readable_stats
                        .get_mut(&timer)
                        .expect("multi-thread call same timer");

                    if ticks.is_none() {
                        log::trace!("directly timeout, token={:?}", timer);
                        *state = IoState::Timeout;
                        return Poll::Ready(Ok(()));
                    }

                    match &*state {
                        IoState::Timeout => {
                            log::trace!("timeout, token={:?}", timer);
                            // timer is already expired.
                            return Poll::Ready(Ok(()));
                        }
                        IoState::Timer(_) => {
                            return Poll::Pending;
                        }
                        _ => {
                            unreachable!(
                                "multi-thread call same time, with invalid state({:?})",
                                *state
                            );
                        }
                    }
                }
                IoState::Timer(waker) => {
                    // update waker.
                    *waker = cx.waker().clone();
                    return Poll::Pending;
                }
                _ => {
                    unreachable!("call `poll_timeout` with invalid state({:?})", *state);
                }
            }
        }

        return Poll::Ready(Err(Error::new(
            ErrorKind::NotFound,
            format!("timer({:?}) is not found.", timer),
        )));
    }

    /// Shutdown the read of this io.
    pub fn shutdown(&self, io: Token, interests: Interest) -> Result<()> {
        if interests.is_readable() {
            let mut state = self.0.io_readable_stats.get_mut(&io).ok_or_else(|| {
                Error::new(ErrorKind::NotFound, format!("InvalidReadState({:?})", io))
            })?;

            match std::mem::replace(&mut *state, IoState::Shutdown) {
                IoState::Waker(waker) => {
                    drop(state);
                    waker.wake();
                }
                IoState::Ready | IoState::None | IoState::Shutdown => {}
                _ => {}
            }
        }

        if interests.is_writable() {
            let mut state = self.0.io_writable_stats.get_mut(&io).ok_or_else(|| {
                Error::new(ErrorKind::NotFound, format!("InvalidWriteState({:?})", io))
            })?;

            match std::mem::replace(&mut *state, IoState::Shutdown) {
                IoState::Waker(waker) => {
                    drop(state);
                    waker.wake();
                }
                IoState::Ready | IoState::None | IoState::Shutdown => {}
                _ => {}
            }
        }

        Ok(())
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
        mut io_f: F,
    ) -> Poll<Result<T>>
    where
        F: FnMut(Token) -> Result<T>,
    {
        log::trace!("poll_io, token={:?}, interest={:?}", io, interest);

        let stats = if interest.is_readable() {
            &self.0.io_readable_stats
        } else if interest.is_writable() {
            &self.0.io_writable_stats
        } else {
            unreachable!("poll_io must with Interest(READABLE/WRITABLE).");
        };

        let mut state = stats
            .get_mut(&io)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("InvalidState({:?})", io)))?;

        match &*state {
            IoState::Ready | IoState::Waker(_) | IoState::None => {
                *state = IoState::None;
                // unlock state first.
                drop(state);

                loop {
                    match io_f(io) {
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            // lock state.
                            let mut state = stats.get_mut(&io).expect("multi-thread call same io");

                            match &*state {
                                IoState::None => {
                                    log::trace!(
                                        "poll_io, token={:?}, interest={:?}, would_block",
                                        io,
                                        interest
                                    );
                                    *state = IoState::Waker(cx.waker().clone());
                                    return Poll::Pending;
                                }
                                IoState::Shutdown => {
                                    log::trace!(
                                        "poll_io, token={:?}, interest={:?}, shutdown",
                                        io,
                                        interest
                                    );
                                    return Poll::Ready(Err(Error::new(
                                        ErrorKind::BrokenPipe,
                                        if interest.is_readable() {
                                            format!("Shutdown readable({:?})", io)
                                        } else {
                                            format!("Shutdown writable({:?})", io)
                                        },
                                    )));
                                }
                                IoState::Ready => {
                                    log::trace!(
                                        "poll_io, token={:?}, interest={:?}, retry",
                                        io,
                                        interest
                                    );
                                    // retry io
                                    *state = IoState::None;

                                    drop(state);

                                    continue;
                                }
                                _ => {
                                    unreachable!(
                                        "multi-thread call `poll_io` on same io, token={:?}, state={:?}, interest={:?}",
                                        io, *state, interest
                                    );
                                }
                            }
                        }
                        r => {
                            log::trace!("poll_io, token={:?}, interest={:?}, ready", io, interest);
                            return Poll::Ready(r);
                        }
                    }
                }
            }
            IoState::Shutdown => {
                log::trace!("poll_io, token={:?}, interest={:?}, shutdown", io, interest);
                return Poll::Ready(Err(Error::new(
                    ErrorKind::BrokenPipe,
                    if interest.is_readable() {
                        format!("Shutdown readable({:?})", io)
                    } else {
                        format!("Shutdown writable({:?})", io)
                    },
                )));
            }
            _ => {
                unreachable!("Invalid state, token={:?}, state={:?}", io, *state);
            }
        }

        todo!()
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

            Reactor::new(1024, Duration::from_millis(200)).unwrap()
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

    #[test]
    fn test_deadline() {
        let timer = global_reactor().deadline(Instant::now());

        sleep(Duration::from_millis(200));

        assert!(
            global_reactor()
                .poll_timeout(&mut noop_context(), timer)
                .is_ready()
        );

        let timer = global_reactor().deadline(Instant::now() + Duration::from_millis(200));

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
