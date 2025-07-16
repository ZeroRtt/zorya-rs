use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, VecDeque},
    io::{Error, ErrorKind, Result},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use futures::{AsyncRead, AsyncWrite};
use n3io::{mio::Token, reactor::Reactor};
use quiche::{RecvInfo, SendInfo};

struct QuicConnState {
    /// reactor for IOs.
    reactor: Reactor,
    /// underlying quiche connection object.
    quiche_conn: quiche::Connection,
    /// generator for outbound bidirectional stream id.
    outbound_bidi_stream_id_next: u64,
    /// The biggest inbound stream ID currently seen.
    inbound_stream_id_current: u64,
    /// fifo queue for first seen inbound stream IDs.
    incoming_stream_id_fifo: VecDeque<u64>,
    /// wakers for stream reading events.
    stream_readable_wakers: HashMap<u64, Waker>,
    /// wakers for stream writting events.
    stream_writable_wakers: HashMap<u64, Waker>,
    /// waker for `poll_send`
    send_waker: Option<Waker>,
    /// waker for fifo receiver.
    fifo_waker: Option<Waker>,
    /// open stream waker.
    open_stream_waker: Option<Waker>,
    /// wait for calling on_timeout.
    on_timeout_timer: Option<Token>,
}

/// Returns true if the stream was created locally.
fn is_local(stream_id: u64, is_server: bool) -> bool {
    (stream_id & 0x1) == (is_server as u64)
}

/// Returns true if the stream is bidirectional.
fn is_bidi(stream_id: u64) -> bool {
    (stream_id & 0x2) == 0
}

/// Quic connection api.
#[derive(Clone)]
pub struct QuicConnDispatcher(Arc<Mutex<QuicConnState>>);

impl QuicConnDispatcher {
    /// Writes a single QUIC packet to be sent to the peer.
    ///
    /// This func transfer error [`quiche::Error::Done`] to [`Poll::Pending`]
    pub fn poll_send(
        &self,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<Result<(usize, SendInfo)>> {
        let mut state = self.0.lock().unwrap();

        if let Some(timer) = state.on_timeout_timer.take() {
            match state.reactor.poll_timeout(cx, timer) {
                Poll::Ready(_) => {
                    log::trace!(
                        "QuicConn(poll_send) call on_timeout, trace_id={}",
                        state.quiche_conn.trace_id()
                    );
                    state.quiche_conn.on_timeout();
                }
                Poll::Pending => {}
            }

            state.reactor.deregister_timer(timer)?;
            state.send_waker = None;
        }

        loop {
            match state.quiche_conn.send(out) {
                Ok((send_size, send_info)) => {
                    log::trace!(
                        "QuicConn(poll_send) send, send_size={}, send_info={:?}, trace_id={}",
                        send_size,
                        send_info,
                        state.quiche_conn.trace_id()
                    );

                    let wakers = Self::poll_conn_stat_events(&mut state);

                    drop(state);

                    for waker in wakers {
                        waker.wake();
                    }

                    return Poll::Ready(Ok((send_size, send_info)));
                }
                Err(quiche::Error::Done) => {
                    if state.quiche_conn.is_draining() {
                        log::trace!(
                            "QuicConn(poll_send) is draining, trace_id={}",
                            state.quiche_conn.trace_id()
                        );

                        return Poll::Ready(Err(Error::new(
                            ErrorKind::BrokenPipe,
                            "QuicConn(poll_send) is draining",
                        )));
                    }

                    if let Some(deadline) = state.quiche_conn.timeout_instant() {
                        let timer = state.reactor.deadline(deadline);

                        match state.reactor.poll_timeout(cx, timer) {
                            Poll::Ready(_) => {
                                // The deadline has expired.
                                state.quiche_conn.on_timeout();
                                continue;
                            }
                            Poll::Pending => {}
                        }
                    }

                    state.send_waker = Some(cx.waker().clone());

                    return Poll::Pending;
                }
                Err(err) => {
                    log::error!(
                        "QuicConn(poll_send), trace_id={:?}, err={}",
                        state.quiche_conn.trace_id(),
                        err
                    );
                    return Poll::Ready(Err(Error::other(err)));
                }
            }
        }
    }

    /// Processes QUIC packets received from the peer.
    pub fn poll_recv(
        &self,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
        info: RecvInfo,
    ) -> Poll<Result<usize>> {
        let mut state = self.0.lock().unwrap();
        let recv_size = state
            .quiche_conn
            .recv(buf, info)
            .map_err(|err| Error::other(err))?;

        let wakers = Self::poll_conn_stat_events(&mut state);

        drop(state);

        for waker in wakers {
            waker.wake();
        }

        Poll::Ready(Ok(recv_size))
    }

    fn poll_conn_stat_events(state: &mut QuicConnState) -> Vec<Waker> {
        let mut wakers = vec![];
        let mut ordering_readable_id_set = BinaryHeap::new();

        while let Some(id) = state.quiche_conn.stream_readable_next() {
            ordering_readable_id_set.push(Reverse(id));
        }

        while let Some(Reverse(id)) = ordering_readable_id_set.pop() {
            if is_bidi(id)
                && is_local(id, state.quiche_conn.is_server())
                && state.inbound_stream_id_current < id
            {
                state.inbound_stream_id_current = id;
                state.incoming_stream_id_fifo.push_back(id);

                log::trace!(
                    "New incoming stream, id={},trace_id={}",
                    id,
                    state.quiche_conn.trace_id()
                );
            }

            if let Some(waker) = state.stream_readable_wakers.remove(&id) {
                log::trace!(
                    "Wakeup stream readable, id={},trace_id={}",
                    id,
                    state.quiche_conn.trace_id()
                );

                wakers.push(waker);
            }
        }

        if !state.incoming_stream_id_fifo.is_empty() {
            if let Some(waker) = state.fifo_waker.take() {
                log::trace!(
                    "Wakeup stream incoming, trace_id={}",
                    state.quiche_conn.trace_id()
                );
                wakers.push(waker);
            }
        }

        while let Some(id) = state.quiche_conn.stream_writable_next() {
            if let Some(waker) = state.stream_writable_wakers.remove(&id) {
                log::trace!(
                    "Wakeup stream writable, id={},trace_id={}",
                    id,
                    state.quiche_conn.trace_id()
                );
                wakers.push(waker);
            }
        }

        if let Some(waker) = state.open_stream_waker.take() {
            if state.quiche_conn.peer_streams_left_bidi() > 0 {
                log::trace!(
                    "Wakeup open stream, trace_id={}",
                    state.quiche_conn.trace_id()
                );
                wakers.push(waker);
            }
        }

        wakers
    }
}

/// An extension trait for `QuicConnDispatcher` that provides a variety of convenient combinator functions.
pub trait QuicConnDispatcherExt {
    fn recv<'a>(&'a self, buf: &'a mut [u8], info: RecvInfo) -> ConnRecv<'a>;
    fn send<'a>(&'a self, buf: &'a mut [u8]) -> ConnSend<'a>;
}

impl QuicConnDispatcherExt for QuicConnDispatcher {
    fn recv<'a>(&'a self, buf: &'a mut [u8], info: RecvInfo) -> ConnRecv<'a> {
        ConnRecv {
            dispatcher: self,
            buf,
            info,
        }
    }

    fn send<'a>(&'a self, buf: &'a mut [u8]) -> ConnSend<'a> {
        ConnSend {
            dispatcher: self,
            buf,
        }
    }
}

/// A future created by [`recv`](QuicConnExt::recv) func.
pub struct ConnRecv<'a> {
    dispatcher: &'a QuicConnDispatcher,
    buf: &'a mut [u8],
    info: RecvInfo,
}

impl<'a> Future for ConnRecv<'a> {
    type Output = Result<usize>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let info = self.info.clone();

        self.dispatcher.poll_recv(cx, self.buf, info)
    }
}

/// A future created by [`send`](QuicConnExt::send) func.
pub struct ConnSend<'a> {
    dispatcher: &'a QuicConnDispatcher,
    buf: &'a mut [u8],
}

impl<'a> Future for ConnSend<'a> {
    type Output = Result<(usize, SendInfo)>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.dispatcher.poll_send(cx, self.buf)
    }
}

/// Quic connection api.
pub struct QuicConn(Arc<Mutex<QuicConnState>>);

impl Drop for QuicConn {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();

        if let Err(err) = state.quiche_conn.close(false, 0x0, b"") {
            log::trace!(
                "Close quic_conn, trace_id={}, err={}",
                state.quiche_conn.trace_id(),
                err
            );
        }
    }
}

impl QuicConn {
    /// Create new `QuicConn` from raw `quiche::Connection.`
    pub fn new(
        quiche_conn: quiche::Connection,
        reactor: Reactor,
    ) -> (QuicConn, QuicConnDispatcher) {
        let outbound_bidi_stream_id_next = if quiche_conn.is_server() { 5 } else { 4 };

        let state = Arc::new(Mutex::new(QuicConnState {
            reactor,
            quiche_conn,
            outbound_bidi_stream_id_next,
            inbound_stream_id_current: 0,
            incoming_stream_id_fifo: Default::default(),
            stream_readable_wakers: Default::default(),
            stream_writable_wakers: Default::default(),
            send_waker: Default::default(),
            fifo_waker: Default::default(),
            on_timeout_timer: Default::default(),
            open_stream_waker: Default::default(),
        }));

        (QuicConn(state.clone()), QuicConnDispatcher(state))
    }

    /// Accepts a new `QUIC` stream.
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<Result<QuicStream>> {
        let mut state = self.0.lock().unwrap();

        if state.quiche_conn.is_closed() {
            return Poll::Ready(Err(Error::new(
                ErrorKind::BrokenPipe,
                format!(
                    "quic connection is closed, id={}",
                    state.quiche_conn.trace_id()
                ),
            )));
        }

        if let Some(stream_id) = state.incoming_stream_id_fifo.pop_front() {
            return Poll::Ready(Ok(QuicStream(stream_id, self.0.clone())));
        }

        log::trace!(
            "Accept new incoming, trace_id={}, pending=true",
            state.quiche_conn.trace_id()
        );

        state.fifo_waker = Some(cx.waker().clone());

        Poll::Pending
    }

    /// Open a new outbound stream.
    pub fn poll_stream_open(&self, cx: &mut Context<'_>) -> Poll<Result<QuicStream>> {
        let mut state = self.0.lock().unwrap();

        if state.quiche_conn.peer_streams_left_bidi() > 0 {
            let stream_id = state.outbound_bidi_stream_id_next;
            state.outbound_bidi_stream_id_next += 4;

            // this a trick, func `stream_priority` will created the target if did not exist.
            state
                .quiche_conn
                .stream_priority(stream_id, 255, true)
                .map_err(|err| Error::other(err))?;

            Poll::Ready(Ok(QuicStream(stream_id, self.0.clone())))
        } else {
            log::trace!(
                "Open new outbound, trace_id={}, pending=true",
                state.quiche_conn.trace_id()
            );
            state.open_stream_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// An extension trait for `QuicConn` that provides a variety of convenient combinator functions.
pub trait QuicConnExt {
    /// Accept a new incoming QuicStream.
    fn accept(&self) -> AcceptStream<'_>;

    /// Open a new outbound stream.
    fn open(&self) -> OpenStream<'_>;
}

impl QuicConnExt for QuicConn {
    fn accept(&self) -> AcceptStream<'_> {
        AcceptStream(self)
    }

    fn open(&self) -> OpenStream<'_> {
        OpenStream(self)
    }
}

/// A future created by [`accept`](QuicConnExt::accept) func.
pub struct AcceptStream<'a>(&'a QuicConn);

impl<'a> Future for AcceptStream<'a> {
    type Output = Result<QuicStream>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_accept(cx)
    }
}

/// A future created by [`open`](QuicConnExt::open) func.
pub struct OpenStream<'a>(&'a QuicConn);

impl<'a> Future for OpenStream<'a> {
    type Output = Result<QuicStream>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_stream_open(cx)
    }
}

/// The quic stream socket.
pub struct QuicStream(u64, Arc<Mutex<QuicConnState>>);

impl Drop for QuicStream {
    fn drop(&mut self) {
        _ = self.close_stream();
    }
}

impl QuicStream {
    fn close_stream(&self) -> Result<()> {
        let mut state = self.1.lock().unwrap();

        if !state.quiche_conn.stream_finished(self.0) {
            log::trace!(
                "Close stream with unread data, id={}, trace_id={}",
                self.0,
                state.quiche_conn.trace_id(),
            );
            return Ok(());
        }

        if let Err(err) = state.quiche_conn.stream_send(self.0, b"", true) {
            log::error!(
                "Failed to close stream, id={}, trace_id={}, err={}",
                self.0,
                state.quiche_conn.trace_id(),
                err
            );
        }

        Ok(())
    }
    /// Attempt to write bytes from `buf` into the `stream_id`.
    pub fn poll_stream_write(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        fin: bool,
    ) -> Poll<Result<usize>> {
        let mut state = self.1.lock().unwrap();

        match state.quiche_conn.stream_send(self.0, buf, fin) {
            Ok(written_size) => {
                if let Some(waker) = state.send_waker.take() {
                    drop(state);
                    waker.wake();
                }

                return Poll::Ready(Ok(written_size));
            }
            Err(quiche::Error::Done) => {
                state
                    .stream_writable_wakers
                    .insert(self.0, cx.waker().clone());

                return Poll::Pending;
            }
            Err(err) => {
                return Poll::Ready(Err(Error::other(err)));
            }
        }
    }

    /// Attempt to read bytes the `stream_id` into the `buf`.
    pub fn poll_stream_read(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<(usize, bool)>> {
        let mut state = self.1.lock().unwrap();

        // when `stream_finished` is true, always returns (0,true)
        if state.quiche_conn.stream_finished(self.0) {
            return Poll::Ready(Ok((0, true)));
        }

        match state.quiche_conn.stream_recv(self.0, buf) {
            Ok(written_size) => {
                if let Some(waker) = state.send_waker.take() {
                    drop(state);
                    waker.wake();
                }

                return Poll::Ready(Ok(written_size));
            }
            Err(quiche::Error::Done) => {
                state
                    .stream_readable_wakers
                    .insert(self.0, cx.waker().clone());

                return Poll::Pending;
            }
            Err(err) => {
                return Poll::Ready(Err(Error::other(err)));
            }
        }
    }
}

impl AsyncWrite for &QuicStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        self.poll_stream_write(cx, buf, false)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_stream_write(cx, b"", true).map_ok(|_| ())
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        self.poll_stream_write(cx, buf, false)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_stream_write(cx, b"", true).map_ok(|_| ())
    }
}

impl AsyncRead for &QuicStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize>> {
        self.poll_stream_read(cx, buf).map_ok(|(len, _)| len)
    }
}
