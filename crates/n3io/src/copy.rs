//! A future copy data from `AsyncRead` source to `AsyncWrite` sink directly.

use std::io::Result;

use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Copy data from `AsyncRead` source to `AsyncWrite` sink directly.
pub async fn copy<R, W>(
    debug: Option<&str>,
    mut read: R,
    write: &mut W,
    max_send_payload_size: usize,
) -> Result<usize>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut trans = 0;
    let mut buf = vec![0; max_send_payload_size];

    loop {
        let read_size = read.read(&mut buf).await?;

        if read_size == 0 {
            return Ok(trans);
        }

        write.write_all(&buf[..read_size]).await?;
        log::trace!(
            "`{}` transfer data, len={}",
            debug.unwrap_or("copy"),
            read_size
        );
        trans += read_size;
    }
}
