use {
    chat_logic::*,
    component_utils::Codec,
    libp2p::futures::StreamExt,
    onion::EncryptedStream,
    std::{collections::HashMap, convert::Infallible},
};

pub struct RequestDispatch {
    buffer: Vec<u8>,
    sink: libp2p::futures::channel::mpsc::Sender<RequestInit>,
}

impl Clone for RequestDispatch {
    fn clone(&self) -> Self {
        Self {
            buffer: Vec::new(),
            sink: self.sink.clone(),
        }
    }
}

impl RequestDispatch {
    pub fn new() -> (Self, RequestStream) {
        let (sink, stream) = libp2p::futures::channel::mpsc::channel(5);
        (
            Self {
                buffer: Vec::new(),
                sink,
            },
            stream,
        )
    }

    pub async fn dispatch<P: Protocol>(
        &mut self,
        topic: impl Into<Option<PossibleTopic>>,
        request: P::Request<'_>,
    ) -> Result<P::Response<'_>, RequestError<P>> {
        let id = CallId::new();
        let (tx, rx) = libp2p::futures::channel::oneshot::channel();
        use libp2p::futures::SinkExt;
        self.sink
            .send(RequestInit::Request(RawRequest {
                id,
                topic: topic.into(),
                payload: (P::PREFIX, id, request).to_bytes(),
                channel: tx,
            }))
            .await
            .map_err(|_| RequestError::ChannelClosed)?;
        self.buffer = rx.await.map_err(|_| RequestError::ChannelClosed)?;
        Self::parse_response::<P>(&self.buffer)
    }

    pub async fn dispatch_direct<P: Protocol>(
        &mut self,
        stream: &mut EncryptedStream,
        request: &P::Request<'_>,
    ) -> Result<P::Response<'_>, RequestError<P>> {
        stream
            .write(&(P::PREFIX, CallId::whatever(), request))
            .ok_or(RequestError::ServerIsOwervhelmed)?;

        self.buffer = stream
            .next()
            .await
            .ok_or(RequestError::ChannelClosed)?
            .map_err(|_| RequestError::ChannelClosed)?;

        Self::parse_response::<P>(&self.buffer)
    }

    pub fn parse_response<P: Protocol>(
        response: &[u8],
    ) -> Result<P::Response<'_>, RequestError<P>> {
        <(CallId, _)>::decode(&mut &response[..])
            .ok_or(RequestError::InvalidResponse)
            .and_then(|r| r.response.map_err(RequestError::Handler))
    }

    pub async fn dispatch_direct_batch<'a, 'b, P: Protocol>(
        &'a mut self,
        stream: &mut EncryptedStream,
        requests: impl Iterator<Item = P::Request<'b>>,
    ) -> Result<impl Iterator<Item = (ProtocolResult<'a, P>, P::Request<'b>)>, RequestError<P>>
    {
        let mut mapping = HashMap::new();
        for request in requests {
            let id = CallId::new();
            stream
                .write((P::PREFIX, id, &request))
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
                <(CallId, ProtocolResult<'_, P>)>::decode(b)
                    .and_then(|(id, body)| Some((body, mapping.remove(&id)?)))
            }),
        )
    }

    pub fn subscribe<P: Topic>(
        &mut self,
        topic: P,
    ) -> Result<(Subscription<P>, SubsOwner<P>), RequestError<Infallible>> {
        let (tx, rx) = libp2p::futures::channel::mpsc::channel(0);
        let request_id = CallId::new();
        self.sink
            .try_send(RequestInit::Subscription(SubscriptionInit {
                request_id,
                topic: topic.to_bytes(),
                payload: (P::PREFIX | 0x80, request_id, topic).to_bytes(),
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
        <H::Event<'_> as Codec<'_>>::decode(&mut &self.buffer[..])
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
    pub topic: Option<PossibleTopic>,
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