use std::{collections::VecDeque, io, pin::Pin, task::Poll};

use futures::Future;

use crate::{decode_len, encode_len};

type PacketSize = u32;
const PACKET_SIZE_WIDTH: usize = std::mem::size_of::<PacketSize>();

#[derive(Debug, Default)]
pub struct PacketReader {
    read_buffer: Vec<u8>,
    read_offset: usize,
}

impl PacketReader {
    fn poll_read_exact(
        &mut self,
        cx: &mut std::task::Context<'_>,
        stream: &mut (impl futures::AsyncRead + Unpin),
        amount: usize,
    ) -> Poll<Result<(), io::Error>> {
        if self.read_offset >= amount {
            return Poll::Ready(Ok(()));
        }

        if self.read_buffer.len() < amount {
            self.read_buffer.resize(amount, 0);
        }

        while self.read_offset < amount {
            let n = futures::ready!(Pin::new(&mut *stream)
                .poll_read(cx, &mut self.read_buffer[self.read_offset..amount]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
            }
            self.read_offset += n;
        }

        Poll::Ready(Ok(()))
    }

    pub fn poll_packet<'a>(
        &'a mut self,
        cx: &mut std::task::Context<'_>,
        stream: &mut (impl futures::AsyncRead + Unpin),
    ) -> Poll<Result<&'a mut [u8], io::Error>> {
        futures::ready!(self.poll_read_exact(cx, stream, PACKET_SIZE_WIDTH))?;

        let packet_size = decode_len(self.read_buffer[..PACKET_SIZE_WIDTH].try_into().unwrap());

        futures::ready!(self.poll_read_exact(cx, stream, packet_size + PACKET_SIZE_WIDTH))?;

        let packet = &mut self.read_buffer[PACKET_SIZE_WIDTH..packet_size + PACKET_SIZE_WIDTH];
        self.read_offset = 0;
        Poll::Ready(Ok(packet))
    }
}

#[derive(Debug)]
pub struct PacketWriter {
    queue: VecDeque<u8>,
    waker: Option<std::task::Waker>,
}

impl PacketWriter {
    pub fn new(cap: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(cap),
            waker: None,
        }
    }

    pub fn packet<T: IntoIterator>(&mut self, values: T) -> bool
    where
        VecDeque<u8>: Extend<T::Item>,
        VecDeque<u8>: Extend<u8>,
    {
        let Some(free_space) =
            (self.queue.capacity() - self.queue.len()).checked_sub(PACKET_SIZE_WIDTH)
        else {
            return false;
        };
        let prev_len = self.queue.len();
        self.queue.extend([0u8; 4]);

        let mut iter = values.into_iter();
        self.queue.extend(iter.by_ref().take(free_space));

        if iter.next().is_some() {
            self.queue.truncate(prev_len);
            return false;
        }

        let packet_size = self.queue.len() - prev_len - PACKET_SIZE_WIDTH;
        assert!(packet_size != 0);
        self.queue
            .iter_mut()
            .skip(prev_len)
            .zip(encode_len(packet_size))
            .for_each(|(a, b)| *a = b);

        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        true
    }

    pub fn write(&mut self, buf: &[u8]) -> bool {
        let fits = self.queue.capacity() - self.queue.len() >= buf.len() + PACKET_SIZE_WIDTH;
        if fits {
            if let Some(waker) = self.waker.take() {
                waker.wake();
            }
            self.queue.extend(buf);
        }
        fits
    }

    pub fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
        dest: &mut (impl futures::AsyncWrite + Unpin),
    ) -> Poll<Result<(), io::Error>> {
        self.waker = Some(cx.waker().clone());
        loop {
            let (left, riht) = self.queue.as_slices();
            let Some(some_bytes) = [left, riht].into_iter().find(|s| !s.is_empty()) else {
                return Poll::Ready(Ok(()));
            };

            let n = futures::ready!(Pin::new(&mut *dest).poll_write(cx, some_bytes))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.queue.drain(..n);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

pub struct ClosingStream<S> {
    stream: S,
    writer: PacketWriter,
}

impl<S> ClosingStream<S> {
    pub fn new(stream: S, writer: PacketWriter) -> Self {
        Self { stream, writer }
    }
}

impl<S: futures::AsyncWrite + Unpin> Future for ClosingStream<S> {
    type Output = Result<(), io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        futures::ready!(this.writer.poll(cx, &mut this.stream))?;
        Pin::new(&mut this.stream).poll_close(cx)
    }
}
