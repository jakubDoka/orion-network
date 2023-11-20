use std::{collections::VecDeque, io, pin::Pin, task::Poll};

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: &[u8]) -> Self {
        Self(fnv_hash(seed))
    }

    pub fn next_u64(&mut self) -> u64 {
        let Self(seed) = self;
        *seed = fnv_hash(&seed.to_le_bytes());
        *seed
    }
}

#[derive(Debug)]
pub struct LinearMap<K, V> {
    values: Vec<(K, V)>,
}

impl<K: Eq, V> LinearMap<K, V> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some((_, current)) = self.values.iter_mut().find(|(k, _)| k == &key) {
            return Some(core::mem::replace(current, value));
        }
        self.values.push((key, value));
        None
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(index) = self.values.iter().position(|(k, _)| k == key) {
            return Some(self.values.swap_remove(index).1);
        }
        None
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        #[allow(clippy::map_identity)]
        self.values.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.values.iter().map(|(k, _)| k)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.values.iter().any(|(k, _)| k == key)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.values
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.values.iter_mut().map(|(_, v)| v)
    }
}

impl<'a, K: Codec<'a>, V: Codec<'a>> Codec<'a> for LinearMap<K, V> {
    fn encode(&self, buf: &mut Vec<u8>) {
        self.values.encode(buf)
    }

    fn decode(buf: &mut &'a [u8]) -> Option<Self> {
        Some(Self {
            values: Vec::decode(buf)?,
        })
    }
}

impl<K, V> Default for LinearMap<K, V> {
    fn default() -> Self {
        Self { values: Vec::new() }
    }
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= *byte as u64;
    }
    hash
}

use futures::Future;

use crate::{decode_len, encode_len, Codec};

type PacketSize = u32;
const PACKET_SIZE_WIDTH: usize = core::mem::size_of::<PacketSize>();

pub struct AsocStream<A, S> {
    pub inner: S,
    pub assoc: A,
}

impl<A, S> AsocStream<A, S> {
    pub fn new(inner: S, assoc: A) -> Self {
        Self { inner, assoc }
    }
}

impl<A: Clone, S: futures::Stream> futures::Stream for AsocStream<A, S> {
    type Item = (A, S::Item);

    fn poll_next(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Option<Self::Item>> {
        unsafe { self.as_mut().map_unchecked_mut(|s| &mut s.inner) }
            .poll_next(cx)
            .map(|opt| opt.map(|item| (self.assoc.clone(), item)))
    }
}

#[derive(Debug, Default)]
pub struct PacketReader {
    read_buffer: Vec<u8>,
    read_offset: usize,
}

impl PacketReader {
    fn poll_read_exact(
        &mut self,
        cx: &mut core::task::Context<'_>,
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
        cx: &mut core::task::Context<'_>,
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
    waker: Option<core::task::Waker>,
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
        cx: &mut core::task::Context<'_>,
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

    fn poll(mut self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        futures::ready!(this.writer.poll(cx, &mut this.stream))?;
        Pin::new(&mut this.stream).poll_close(cx)
    }
}
