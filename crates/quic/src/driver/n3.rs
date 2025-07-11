use crate::driver::QuicDriver;

/// The multi-thread implementation for [`QuicDriver`]
pub struct N3QuicDriver {}

#[allow(unused)]
impl QuicDriver for N3QuicDriver {
    fn bind(&self, socket: mio::net::UdpSocket) -> crate::errors::Result<super::QuicSocket> {
        todo!()
    }

    fn connect_to(
        &self,
        socket: mio::net::UdpSocket,
        raddr: std::net::SocketAddr,
    ) -> crate::errors::Result<super::QuicSocket> {
        todo!()
    }

    fn close(&self, socket: super::QuicSocket) -> crate::errors::Result<()> {
        todo!()
    }

    fn poll_connected(
        &self,
        cx: &mut std::task::Context<'_>,
        conn: super::QuicSocket,
    ) -> std::task::Poll<crate::errors::Result<()>> {
        todo!()
    }

    fn poll_accept(
        &self,
        cx: &mut std::task::Context<'_>,
        socket: super::QuicSocket,
    ) -> std::task::Poll<crate::errors::Result<Option<super::QuicSocket>>> {
        todo!()
    }

    fn poll_write(
        &self,
        cx: &mut std::task::Context<'_>,
        stream: super::QuicSocket,
        buf: &[u8],
        fin: bool,
    ) -> std::task::Poll<crate::errors::Result<usize>> {
        todo!()
    }

    fn poll_read(
        &self,
        cx: &mut std::task::Context<'_>,
        stream: super::QuicSocket,
        buf: &mut [u8],
    ) -> std::task::Poll<crate::errors::Result<usize>> {
        todo!()
    }
}
