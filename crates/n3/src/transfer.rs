//! in-memory caching structures used by tunnel data transferring.

use std::{
    io::{ErrorKind, Read, Write},
    num::NonZeroUsize,
};

use crate::errors::{N3Error, Result};

/// Buf for unidirectional data transmission
pub struct TransferBuf {
    /// fixed allocated buf block.
    buf: Vec<u8>,
    /// written data length.
    written_len: usize,
    /// reading cursor offset.
    read_offset: usize,
}

impl TransferBuf {
    /// Create a new `TransferBuf` with provided capacity.
    pub fn with_capacity(len: NonZeroUsize) -> Self {
        Self {
            buf: vec![0; len.into()],
            written_len: 0,
            read_offset: 0,
        }
    }
    /// Write new data into this buf.
    ///
    /// This func will panic, if the buf still has unread data: `read_buf_len > 0`.
    pub fn write_buf<R>(&mut self, r: R) -> Result<usize>
    where
        R: FnOnce(&mut [u8]) -> Result<usize>,
    {
        assert_eq!(
            self.written_len, self.read_offset,
            "The buf still has unread data."
        );

        let written_len = r(&mut self.buf)?;

        assert!(
            written_len <= self.buf.len(),
            "write_buf: the w func returned illegal written length value({}).",
            written_len
        );

        self.written_len = written_len;
        self.read_offset = 0;

        Ok(written_len)
    }

    /// Returns the unread data length.
    pub fn read_buf_len(&self) -> usize {
        self.written_len - self.read_offset
    }

    /// Read new data from this buf.
    pub fn read_buf<W>(&mut self, w: W) -> Result<usize>
    where
        W: FnOnce(&[u8]) -> Result<usize>,
    {
        let read_len = w(&self.buf[self.read_offset..self.written_len])?;
        assert!(
            read_len + self.read_offset <= self.written_len,
            "read_buf: the w func returned illegal read length value({}).",
            read_len
        );

        self.read_offset += read_len;

        Ok(read_len)
    }

    /// Transfer data via this buf.
    pub fn transfer<F, T>(&mut self, debug: Option<&str>, mut from: F, mut to: T) -> Result<usize>
    where
        F: Read,
        T: Write,
    {
        let mut transferred = 0;

        loop {
            // transfer caching data first.
            if self.read_buf_len() > 0 {
                match self.read_buf(|buf| {
                    to.write_all(buf)?;

                    Ok(buf.len())
                }) {
                    Ok(read) => {
                        transferred += read;
                    }
                    Err(N3Error::Io(err)) if err.kind() == ErrorKind::WriteZero => {
                        return Err(N3Error::BrokenPipe(transferred));
                    }
                    Err(N3Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                        return Ok(transferred);
                    }
                    Err(err) => {
                        log::error!("broken pipe(to,{}): {}", debug.unwrap_or(""), err);
                        return Err(err);
                    }
                }
            }

            assert!(self.read_buf_len() == 0, "write_all: inner error.");

            match self.write_buf(|buf| Ok(from.read(buf)?)) {
                Ok(written) => {
                    // reach the end of `from`
                    if written == 0 {
                        return Err(N3Error::BrokenPipe(transferred));
                    }
                }
                Err(N3Error::Io(err)) if err.kind() == ErrorKind::WouldBlock => {
                    return Ok(transferred);
                }
                Err(err) => {
                    log::error!("broken pipe(from,{}): {}", debug.unwrap_or(""), err);
                    return Err(N3Error::BrokenPipe(transferred));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, ErrorKind, Read, Write},
        num::NonZeroUsize,
        panic::catch_unwind,
    };

    use crate::{errors::N3Error, transfer::TransferBuf};

    #[test]
    fn transfer_buf() {
        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        assert_eq!(buf.write_buf(|_| Ok(10)).unwrap(), 10);

        assert_eq!(buf.read_buf_len(), 10);

        assert_eq!(buf.read_buf(|_| Ok(10)).unwrap(), 10);

        assert_eq!(buf.read_buf_len(), 0);
    }

    #[test]
    fn write_buf_guard() {
        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        assert_eq!(buf.write_buf(|_| Ok(10)).unwrap(), 10);

        catch_unwind(move || buf.write_buf(|_| Ok(10))).expect_err("write_buf guard");
    }

    #[test]
    fn write_buf_return_value_guard() {
        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        catch_unwind(move || buf.write_buf(|_| Ok(20))).expect_err("write_buf_return_value_guard");
    }

    #[test]
    fn read_buf_return_value_guard() {
        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        catch_unwind(move || buf.read_buf(|_| Ok(10))).expect_err("read_buf_return_value_guard");
    }

    #[test]
    fn test_transfer() {
        let mut from = Cursor::new(b"hello world");
        let mut to = vec![];

        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        assert_eq!(
            buf.transfer(None, &mut from, &mut to),
            Err(N3Error::BrokenPipe(11))
        );

        assert_eq!(to, b"hello world");
    }

    #[test]
    fn test_would_block() {
        struct WoldBlock;

        impl Write for WoldBlock {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(ErrorKind::WouldBlock, "would block"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::new(ErrorKind::WouldBlock, "would block"))
            }
        }

        impl Read for WoldBlock {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(ErrorKind::WouldBlock, "would block"))
            }
        }

        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        let mut from = Cursor::new(b"hello world");
        let mut to = WoldBlock;

        assert_eq!(buf.transfer(None, &mut from, &mut to), Ok(0));

        let mut buf = TransferBuf::with_capacity(NonZeroUsize::new(10).unwrap());

        let mut from = WoldBlock;
        let mut to = Vec::new();

        assert_eq!(buf.transfer(None, &mut from, &mut to), Ok(0));
    }
}
