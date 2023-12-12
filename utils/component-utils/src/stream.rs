use {
    crate::{decode_len, encode_len, Buffer, Codec, Reminder},
    core::ops::Range,
    futures::Future,
    std::{collections::VecDeque, io, pin::Pin, task::Poll},
};

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

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.values.iter_mut().map(|(k, v)| (&*k, v))
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

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.values.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.values.iter_mut().map(|(_, v)| v)
    }
}

impl<'a, K: Codec<'a>, V: Codec<'a>> Codec<'a> for LinearMap<K, V> {
    fn encode(&self, buf: &mut impl Buffer) -> Option<()> {
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
        log::info!("poll_packet");
        futures::ready!(self.poll_read_exact(cx, stream, PACKET_SIZE_WIDTH))?;

        let packet_size = decode_len(self.read_buffer[..PACKET_SIZE_WIDTH].try_into().unwrap());
        log::info!("packet_size: {}", packet_size);

        futures::ready!(self.poll_read_exact(cx, stream, packet_size + PACKET_SIZE_WIDTH))?;
        log::info!("read_offset: {}", self.read_offset);

        let packet = &mut self.read_buffer[PACKET_SIZE_WIDTH..packet_size + PACKET_SIZE_WIDTH];
        self.read_offset = 0;
        Poll::Ready(Ok(packet))
    }
}

struct NoCapOverflow<'a> {
    vec: &'a mut Vec<u8>,
}

impl Buffer for NoCapOverflow<'_> {
    fn extend_from_slice(&mut self, slice: &[u8]) -> Option<()> {
        if self.vec.len() + slice.len() > self.vec.capacity() {
            return None;
        }
        self.vec.extend_from_slice(slice);
        Some(())
    }

    fn push(&mut self, byte: u8) -> Option<()> {
        if self.vec.len() == self.vec.capacity() {
            return None;
        }
        self.vec.push(byte);
        Some(())
    }
}

#[derive(Debug)]
pub struct PacketWriter {
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    waker: Option<core::task::Waker>,
}

impl PacketWriter {
    pub fn new(cap: usize) -> Self {
        Self {
            buffer: vec![0; cap],
            start: 0,
            end: 0,
            waker: None,
        }
    }

    pub fn write_packet<'a>(&mut self, message: &impl Codec<'a>) -> Option<&mut [u8]> {
        let reserved = self.write_with_range(&[0u8; PACKET_SIZE_WIDTH])?;
        let written = self.write_with_range(message)?;
        // SAFETY: we do not reallocate the buffer, ever
        self.slice(reserved)
            .copy_from_slice(&encode_len(written.len()));
        Some(self.slice(written))
    }

    pub fn slice(&mut self, range: Range<usize>) -> &mut [u8] {
        &mut self.buffer[range]
    }

    pub fn write_with_range<'a>(&mut self, message: &impl Codec<'a>) -> Option<Range<usize>> {
        let res = self.write(message)?.len();
        Some(self.end - res..self.end)
    }

    #[must_use = "handle the buffer overflow"]
    pub fn write<'a>(&mut self, buf: &impl Codec<'a>) -> Option<&mut [u8]> {
        let free_cap = self.buffer.capacity() - self.buffer.len();
        let mut space = self.in_buffer_space();
        if free_cap < space.len() {
            let prev_ptr = space.as_mut_ptr();
            let prev_len = space.len();
            buf.encode(&mut space)?;
            let writtern = prev_len - space.len();
            self.end += writtern;
            self.end -= self.buffer.len() * (self.end >= self.buffer.len()) as usize;
            Some(unsafe { core::slice::from_raw_parts_mut(prev_ptr, writtern) })
        } else {
            let prev_len = self.buffer.len();
            buf.encode(&mut NoCapOverflow {
                vec: &mut self.buffer,
            })?;
            let writtern = self.buffer.len() - prev_len;
            self.end += writtern;
            Some(&mut self.buffer[prev_len..])
        }
    }

    #[must_use = "handle the buffer overflow"]
    pub fn write_bytes(&mut self, buf: &[u8]) -> Option<&mut [u8]> {
        self.write(&Reminder(buf))
    }

    /// this can panic if poll was called inbetween, the sole purpose is to revert writes
    pub fn revert(&mut self, snapshot: PacketWriterSnapshot) {
        self.buffer.truncate(snapshot.len);
        self.start = snapshot.start;
        self.end = snapshot.end;
    }

    pub fn take_snapshot(&self) -> PacketWriterSnapshot {
        PacketWriterSnapshot {
            len: self.buffer.len(),
            start: self.start,
            end: self.end,
        }
    }

    pub fn poll(
        &mut self,
        cx: &mut core::task::Context<'_>,
        dest: &mut (impl futures::AsyncWrite + Unpin),
    ) -> Poll<Result<(), io::Error>> {
        loop {
            let (left, riht) = self.writable_parts();
            let Some(some_bytes) = [left, riht].into_iter().find(|s| !s.is_empty()) else {
                crate::set_waker(&mut self.waker, cx.waker());
                if self.start == self.end {
                    self.start = 0;
                    self.end = 0;
                }
                return Poll::Ready(Ok(()));
            };

            let n = futures::ready!(Pin::new(&mut *dest).poll_write(cx, some_bytes))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.start += n;
            self.start -= self.buffer.len() * (self.start >= self.buffer.len()) as usize;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn writable_parts(&mut self) -> (&mut [u8], &mut [u8]) {
        if self.start > self.end {
            let (rest, second) = self.buffer.split_at_mut(self.start);
            let (first, _) = rest.split_at_mut(self.end);
            (first, second)
        } else {
            (&mut self.buffer[self.start..self.end], &mut [])
        }
    }

    fn in_buffer_space(&mut self) -> &mut [u8] {
        if self.start > self.end {
            &mut self.buffer[self.end..self.start]
        } else {
            &mut self.buffer[..self.start]
        }
    }
}

pub struct PacketWriterSnapshot {
    len: usize,
    start: usize,
    end: usize,
}

pub struct ClosingStream<S> {
    stream: S,
    error: u8,
}

impl<S> ClosingStream<S> {
    pub fn new(stream: S, error: u8) -> Self {
        Self { stream, error }
    }
}

impl<S: futures::AsyncWrite + Unpin> Future for ClosingStream<S> {
    type Output = Result<(), io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        match futures::ready!(Pin::new(&mut this.stream).poll_write(cx, &[this.error]))? {
            0 => Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
            _ => Poll::Ready(Ok(())),
        }
    }
}
