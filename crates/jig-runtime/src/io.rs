//! The partial-read helper skein's `AsyncReadExt` does not provide.
//!
//! jig's hand-rolled HTTP readers loop on *partial* reads (`read` returning
//! whatever is available, `0` at EOF). skein's extension trait only offers
//! `read_exact` / `read_to_end`, so this adapts `AsyncRead::poll_read` into
//! the familiar `read(&mut buf) -> usize` shape.

use std::io;
use std::pin::Pin;
use std::task::Poll;

use skein::io::{AsyncRead, ReadBuf};

/// Read whatever is available into `buf`, returning the number of bytes read
/// (`0` means EOF). The skein analogue of `tokio::io::AsyncReadExt::read`.
pub async fn read_some<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    std::future::poll_fn(|task_cx| {
        let mut read_buf = ReadBuf::new(buf);
        match Pin::new(&mut *reader).poll_read(task_cx, &mut read_buf) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
