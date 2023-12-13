pub struct RequestDispatch<S> {
    buffer: Vec<u8>,
    sink: libp2p::futures::channel::mpsc::Sender<RequestInit>,
    phantom: std::marker::PhantomData<S>,
}

impl<S> Clone for RequestDispatch<S> {
    fn clone(&self) -> Self {
        Self {
            buffer: Vec::new(),
            sink: self.sink.clone(),
            phantom: std::marker::PhantomData,
        }
    }
}

impl<S> RequestDispatch<S> {
    pub fn new() -> (Self, RequestStream) {
        let (sink, stream) = libp2p::futures::channel::mpsc::channel(5);
        (
            Self {
                buffer: Vec::new(),
                sink,
                phantom: std::marker::PhantomData,
            },
            stream,
        )
    }

    pub async fn dispatch<P: Protocol>(
        &mut self,
        request: P::Request<'_>,
    ) -> Result<P::Response<'_>, RequestError<P>>
    where
        S: Dispatches<P>,
    {
        let id = CallId::new();
        let (tx, rx) = libp2p::futures::channel::oneshot::channel();
        use libp2p::futures::SinkExt;
        self.sink
            .send(RequestInit::Request(RawRequest {
                id,
                topic: todo!(),
                payload: (S::PREFIX, id, request).to_bytes(),
                channel: tx,
            }))
            .await
            .map_err(|_| RequestError::ChannelClosed)?;
        self.buffer = rx.await.map_err(|_| RequestError::ChannelClosed)?;
        Self::parse_response::<P>(&self.buffer)
    }

    pub async fn dispatch_direct<H: Protocol>(
        &mut self,
        stream: &mut EncryptedStream,
        request: &H::Request<'_>,
    ) -> Result<H::Response<'_>, RequestError<H>>
    where
        S: Dispatches<H>,
    {
        stream
            .write(&(S::PREFIX, CallId::whatever(), request))
            .ok_or(RequestError::ServerIsOwervhelmed)?;

        self.buffer = stream
            .next()
            .await
            .ok_or(RequestError::ChannelClosed)?
            .map_err(|_| RequestError::ChannelClosed)?;

        Self::parse_response::<H>(&self.buffer)
    }

    pub fn parse_response<H: Protocol>(response: &[u8]) -> Result<H::Response<'_>, RequestError<H>>
    where
        S: Dispatches<H>,
    {
        DispatchResponse::<HandlerResult<'_, H>>::decode(&mut &response[..])
            .ok_or(RequestError::InvalidResponse)
            .and_then(|r| r.response.map_err(RequestError::Handler))
    }

    pub async fn dispatch_direct_batch<'a, 'b, H: Protocol>(
        &'a mut self,
        stream: &mut EncryptedStream,
        requests: impl Iterator<Item = H::Request<'b>>,
    ) -> Result<impl Iterator<Item = (ProtocolResult<'a, H>, H::Request<'b>)>, RequestError<H>>
    where
        S: Dispatches<H>,
    {
        let mut mapping = HashMap::new();
        for request in requests {
            let id = CallId::new();
            stream
                .write(&(S::PREFIX, id, &request))
                .ok_or(RequestError::ServerIsOwervhelmed)?;
            mapping.insert(id, request);
        }

        let mut stream = stream.by_ref().take(mapping.len());
        while let Some(packet) = stream.next().await {
            self.buffer
                .extend(packet.map_err(|_| RequestError::ChannelClosed)?);
        }

        Ok(
            (0..mapping.len()).scan(self.buffer.as_slice(), move |b, _| {
                DispatchResponse::<HandlerResult<'_, H>>::decode(b)
                    .and_then(|r| Some((r.response, mapping.remove(&r.request_id)?)))
            }),
        )
    }

    pub fn subscribe<H: Topic>(
        &mut self,
        topic: H,
    ) -> Result<(Subscription<H>, SubsOwner<H>), RequestError<H>>
    where
        S: Dispatches<H>,
    {
        let (tx, rx) = libp2p::futures::channel::mpsc::channel(0);
        let request_id = CallId::new();
        self.sink
            .try_send(RequestInit::Subscription(SubscriptionInit {
                request_id,
                topic: topic.to_bytes(),
                payload: (S::PREFIX | 0x80, request_id, topic).to_bytes(),
                channel: tx,
            }))
            .map_err(|_| RequestError::ChannelClosed)?;

        Ok((
            Subscription {
                buffer: Vec::new(),
                events: rx,
                phantom: std::marker::PhantomData,
            },
            SubsOwner {
                id: request_id,
                send_back: self.sink.clone(),
                phantom: std::marker::PhantomData,
            },
        ))
    }
}

pub struct SubsOwner<H: Topic> {
    id: CallId,
    send_back: libp2p::futures::channel::mpsc::Sender<RequestInit>,
    phantom: std::marker::PhantomData<H>,
}

impl<H: Topic> Clone for SubsOwner<H> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            send_back: self.send_back.clone(),
            phantom: std::marker::PhantomData,
        }
    }
}

impl<H: Topic> Drop for SubsOwner<H> {
    fn drop(&mut self) {
        let _ = self
            .send_back
            .try_send(RequestInit::CloseSubscription(self.id));
    }
}

pub type RequestStream = libp2p::futures::channel::mpsc::Receiver<RequestInit>;

pub enum RequestInit {
    Request(RawRequest),
    Subscription(SubscriptionInit),
    CloseSubscription(CallId),
}

impl RequestInit {
    pub fn topic(&self) -> &[u8] {
        match self {
            RequestInit::Request(r) => r.topic.as_deref().unwrap(),
            RequestInit::Subscription(s) => &s.topic,
            RequestInit::CloseSubscription(_) => unreachable!(),
        }
    }
}

pub struct Subscription<H> {
    buffer: Vec<u8>,
    events: libp2p::futures::channel::mpsc::Receiver<SubscriptionMessage>,
    phantom: std::marker::PhantomData<H>,
}

impl<H: Topic> Subscription<H> {
    pub async fn next(&mut self) -> Option<H::Event<'_>> {
        self.buffer = self.events.next().await?;
        H::Event::decode(&mut &self.buffer[..])
    }
}

pub struct SubscriptionInit {
    pub request_id: CallId,
    pub topic: Vec<u8>,
    pub payload: Vec<u8>,
    pub channel: libp2p::futures::channel::mpsc::Sender<SubscriptionMessage>,
}

pub type SubscriptionMessage = Vec<u8>;

pub struct RawRequest {
    pub id: CallId,
    pub topic: Option<Vec<u8>>,
    pub payload: Vec<u8>,
    pub channel: libp2p::futures::channel::oneshot::Sender<RawResponse>,
}

pub type RawResponse = Vec<u8>;

pub enum RequestError<H: Protocol> {
    InvalidResponse,
    ChannelClosed,
    ServerIsOwervhelmed,
    Handler(H::Error),
}

impl<H: Protocol> std::fmt::Debug for RequestError<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::InvalidResponse => write!(f, "invalid response"),
            RequestError::ChannelClosed => write!(f, "channel closed"),
            RequestError::ServerIsOwervhelmed => write!(f, "server is owervhelmed"),
            RequestError::Handler(e) => write!(f, "handler error: {}", e),
        }
    }
}

impl<H: Protocol> std::fmt::Display for RequestError<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl<H: Protocol> std::error::Error for RequestError<H> {}

pub type RootPacketBuffer = PacketBuffer<(CallId, Result<PathId, PeerId>)>;

pub struct PacketBuffer<M> {
    packets: Vec<u8>,
    bounds: Vec<(usize, M)>,
}

impl<M> Default for PacketBuffer<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> PacketBuffer<M> {
    pub fn new() -> Self {
        Self {
            packets: Vec::new(),
            bounds: Vec::new(),
        }
    }

    fn push<'a>(&mut self, packet: &impl Codec<'a>, id: M) {
        packet.encode(&mut self.packets).expect("packet encode");
        self.bounds.push((self.packets.len(), id));
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (M, &mut [u8])> {
        let total_len = self.packets.len();
        let slice = unsafe { std::mem::transmute::<_, &mut [u8]>(self.packets.as_mut_slice()) };
        unsafe { self.packets.set_len(0) }
        self.bounds
            .drain(..)
            .scan(slice, move |slice, (bound, req)| {
                let current_len = slice.len();
                let (head, tail) =
                    std::mem::take(slice).split_at_mut(bound - (total_len - current_len));
                *slice = tail;
                Some((req, head))
            })
    }
}

pub trait Context {
    type ToSwarm;
    type Borrow<'a>;
}

impl<T: NetworkBehaviour> Context for T {
    type Borrow<'a> = &'a mut T;
    type ToSwarm = T::ToSwarm;
}

pub trait SubContext<F: Context>: Context
where
    Self::ToSwarm: From<F::ToSwarm>,
{
    fn fragment(&mut self) -> F::Borrow<'_>;
    fn try_unpack_event(event: Self::ToSwarm) -> Result<F::ToSwarm, Self::ToSwarm>;
}

impl<T: NetworkBehaviour> SubContext<T> for T {
    fn fragment(&mut self) -> <T as Context>::Borrow<'_> {
        self
    }

    fn try_unpack_event(event: Self::ToSwarm) -> Result<<T as Context>::ToSwarm, Self::ToSwarm> {
        Ok(event)
    }
}

pub trait Dispatches<H: Protocol> {
    const PREFIX: u8;
    fn fetch_nest(&mut self) -> &mut HandlerNest<H>;
}

component_utils::protocol! {'a:
    struct DispatchMessage<'a> {
        prefix: u8,
        request_id: CallId,
        payload: Reminder<'a>,
    }
}

pub struct DispatchResponse<T> {
    pub request_id: CallId,
    pub response: T,
}

impl<'a, T: Codec<'a>> Codec<'a> for DispatchResponse<T> {
    fn encode(&self, buffer: &mut impl codec::Buffer) -> Option<()> {
        self.request_id.encode(buffer)?;
        self.response.encode(buffer)
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(Self {
            request_id: CallId::decode(buffer)?,
            response: T::decode(buffer)?,
        })
    }
}

fn drain_filter<'a, T, O: 'a>(
    v: &'a mut Vec<T>,
    pred: impl FnMut(T) -> Result<O, T> + 'a,
) -> impl Iterator<Item = O> + 'a {
    /// TODO: swap remove might me more feasible if we dont remove often
    use core::ptr;
    struct Iter<'a, F, T, O>
    where
        F: FnMut(T) -> Result<O, T>,
    {
        v: &'a mut Vec<T>,
        pred: F,
        write: *mut T,
        read: *mut T,
        end: *mut T,
    }

    impl<'a, F, T, O> Iterator for Iter<'a, F, T, O>
    where
        F: FnMut(T) -> Result<O, T>,
    {
        type Item = O;

        fn next(&mut self) -> Option<Self::Item> {
            unsafe {
                let check_point = self.read;
                loop {
                    if self.read == self.end {
                        let length = self.read.offset_from(check_point) as usize;
                        if check_point != self.write && length > 0 {
                            ptr::copy(check_point, self.write, length);
                        }
                        self.write = self.write.add(length);
                        return None;
                    }

                    let item = ptr::read(self.read);
                    self.read = self.read.add(1);

                    match (self.pred)(item) {
                        Ok(item) => {
                            let length = self.read.offset_from(check_point) as usize - 1;
                            if check_point != self.write && length > 0 {
                                ptr::copy(check_point, self.write, length);
                            }
                            self.write = self.write.add(length);
                            return Some(item);
                        }
                        Err(item) => ptr::write(self.read.sub(1), item),
                    }
                }
            }
        }
    }

    impl<'a, F, T, O> Drop for Iter<'a, F, T, O>
    where
        F: FnMut(T) -> Result<O, T>,
    {
        fn drop(&mut self) {
            self.for_each(drop);

            unsafe {
                let len = self.write.offset_from(self.v.as_mut_ptr()) as usize;
                self.v.set_len(len);
            }
        }
    }

    Iter {
        pred,
        write: v.as_mut_ptr(),
        read: v.as_mut_ptr(),
        end: unsafe { v.as_mut_ptr().add(v.len()) },
        v: {
            unsafe { v.set_len(0) }
            v
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drain_filter() {
        for _ in 0..100000 {
            let mut v = crypto::new_secret().to_vec();

            let odd =
                drain_filter(&mut v, |x| (x % 2 == 0).then_some(x).ok_or(x)).collect::<Vec<_>>();
            assert_eq!(
                odd,
                odd.iter()
                    .copied()
                    .filter(|x| x % 2 == 0)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                v,
                v.iter().copied().filter(|x| x % 2 == 1).collect::<Vec<_>>()
            );
        }
    }
}
