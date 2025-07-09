//! n3 reverse proxy implementation.

use std::{collections::HashMap, os::unix::net::UnixStream, usize};

use mio::{
    Events, Token,
    net::{TcpStream, UdpSocket},
};
use quiche::{ConnectionId, Header, Type};

use crate::{
    channel::ChannelGroup,
    errors::{N3Error, Result},
    quic::AddressValidator,
    token::LocalTokenGenerator,
};

type ChannelGroupToken = Token;

/// N3 reverse proxy implementation.
#[allow(unused)]
pub struct N3ReverseProxy {
    /// tcp token generator.
    tcp_token_gen: LocalTokenGenerator,
    /// channel token generator.
    channel_token_gen: LocalTokenGenerator,
    /// io events poll.
    poll: mio::Poll,
    /// frontend udp sockets.
    udp_socket: (Token, UdpSocket),
    /// tcp sockets.
    tcp_sockets: HashMap<Token, (ChannelGroupToken, TcpStream)>,
    /// connection id => group token.
    quic_conns: HashMap<ConnectionId<'static>, ChannelGroupToken>,
    /// Groups of channels.
    channel_groups: HashMap<ChannelGroupToken, ChannelGroup>,
    /// quiche config.
    config: quiche::Config,
    /// quic remote address validator, used by rety packet.
    address_validator: Box<dyn AddressValidator>,
}

impl N3ReverseProxy {
    /// Consume and run self.
    pub fn run(mut self) -> Result<()> {
        let mut events = Events::with_capacity(1024);
        let mut buf = vec![0; 65535];
        loop {
            self.run_once(&mut events, &mut buf)?;
        }
    }

    fn run_once(&mut self, events: &mut Events, buf: &mut [u8]) -> Result<()> {
        events.clear();

        self.poll.poll(events, None)?;

        for event in events.iter() {
            // udp socket.
            if event.token() == Token(0) {
                if event.is_readable() {
                    self.handle_udp_socket_readable(buf)?;
                }
                if event.is_writable() {
                    self.handle_udp_socket_writable()?;
                }
            } else {
                if event.is_readable() {
                    self.handle_tcp_socket_readable(event.token())?;
                }
                if event.is_writable() {
                    self.handle_tcp_socket_writable(event.token())?;
                }
            }
        }

        Ok(())
    }

    fn handle_udp_socket_readable(&mut self, buf: &mut [u8]) -> Result<()> {
        let (read_size, from) = self.udp_socket.1.recv_from(buf)?;

        log::trace!("socket(udp): recv data({}) from {}", read_size, from);

        match self.handle_quiche_packet(buf, read_size) {
            Ok(write_size) => {
                todo!()
            }
            Err(err) => {
                // Silently drop error packet.
                // TODO: Denial-of-service attack (DoS)

                log::error!("quic: recv data({}) from {}, {}", read_size, from, err);
            }
        }

        Ok(())
    }

    fn handle_quiche_packet(&mut self, buf: &mut [u8], read_size: usize) -> Result<usize> {
        let header = Header::from_slice(&mut buf[..read_size], quiche::MAX_CONN_ID_LEN)?;

        if !quiche::version_is_supported(header.version) {
            return Err(N3Error::UnsupportQuicVersion(header.version));
        }

        match header.ty {
            Type::Initial => self.handle_quiche_initial(buf, header),
            Type::Short => {
                todo!()
            }
            ty => Err(N3Error::UnsupportQuicPacket(ty)),
        }
    }

    fn handle_quiche_initial<'a>(&mut self, buf: &mut [u8], header: Header<'_>) -> Result<usize> {
        todo!()
    }

    fn handle_udp_socket_writable(&mut self) -> Result<()> {
        Ok(())
    }

    fn handle_tcp_socket_readable(&mut self, token: Token) -> Result<()> {
        Ok(())
    }

    fn handle_tcp_socket_writable(&mut self, token: Token) -> Result<()> {
        Ok(())
    }
}
