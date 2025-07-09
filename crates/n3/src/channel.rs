use std::collections::HashMap;

use mio::Token;

use crate::transfer::TransferBuf;

/// A channel group is a bound between quic connection and tcp streams.
pub struct ChannelGroup {
    /// bound quic connection.
    quic_conn: quiche::Connection,
    /// channel from `quic stream` => `tcp stream`.
    quic_stream_to_tcp_stream: HashMap<u64, (Token, TransferBuf)>,
    /// Channel from `tcp stream` => `quic stream`
    tcp_stream_to_quic_stream: HashMap<Token, (u64, TransferBuf)>,
}
