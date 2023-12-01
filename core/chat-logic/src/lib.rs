#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
#![feature(specialization)]
#![allow(incomplete_features)]
use {
    component_utils::{Codec, Reminder},
    libp2p::{futures::StreamExt, swarm::NetworkBehaviour},
    onion::{EncryptedStream, PathId},
    std::{
        collections::{hash_map, HashMap},
        convert::Infallible,
        iter,
    },
};

#[macro_export]
macro_rules! ensure {
    ($cond:expr, $resp:expr) => {
        if !$cond {
            return Err($resp);
        }
    };

    (let $var:pat = $expr:expr, $resp:expr) => {
        let $var = $expr else {
            return Err($resp);
        };
    };
}

#[macro_export]
macro_rules! compose_handlers {
    ($name:ident {$(
        $handler:ident: $handler_ty:ty,
    )*}) => {
        #[derive(Default)]
        pub struct $name {$(
            pub $handler: $crate::HandlerNest<$handler_ty>,
        )*}

        impl $name {
            pub fn dispatch<T>(&mut self, context: &mut T, message: $crate::DispatchMessage<'_>,  pid: Option<onion::PathId>, buffer: &mut $crate::RootPacketBuffer) -> Result<(), $crate::DispatchError>
            where
                $(T: $crate::SubContext<<$handler_ty as $crate::Handler>::Context>,
                T::ToSwarm: From<<<$handler_ty as $crate::Handler>::Context as $crate::Context>::ToSwarm>,)*
            {
                if message.prefix >> 7 == 1 && pid.is_some() {
                    return self.try_handle_subscription(pid.unwrap(), message);
                }

                match message.prefix {
                    $(${index()} => self.$handler.dispatch(context, pid, message, buffer),)*
                    p => Err($crate::DispatchError::InvalidPrefix(p)),
                }
            }

            pub fn dispatch_local<'a, H: $crate::SyncHandler>(&'a mut self, context: &'a mut H::Context, request: H::Request<'a>) -> H::Response<'a>
            where
                Self: $crate::Dispatches<H>,
            {
                use $crate::Dispatches;
                H::execute(context, &request, &mut self.fetch_nest().dispatch, (Self::PREFIX, RequestId::new()))
            }

            fn try_handle_subscription(&mut self, pid: onion::PathId, message: $crate::DispatchMessage<'_>)
                -> Result<(), $crate::DispatchError>
            {
                let prefix = message.prefix & 0b0111_1111;
                match prefix {
                    $(${index()} => self.$handler.subscribe(pid, message.request_id, message.payload),)*
                    p => return Err($crate::DispatchError::InvalidPrefix(p)),
                }

                Ok(())
            }

            pub fn disconnected(&mut self, pid: onion::PathId) {
                $(self.$handler.disconnected(pid);)*
            }

            pub fn try_handle_event<T: $crate::Context>(&mut self, context: &mut T, event: T::ToSwarm, buffer: &mut $crate::RootPacketBuffer)
                -> Result<(), <T as $crate::Context>::ToSwarm>
            where
                $(T: $crate::SubContext<<$handler_ty as $crate::Handler>::Context>,
                T::ToSwarm: From<<<$handler_ty as $crate::Handler>::Context as $crate::Context>::ToSwarm>,)*
            {
                $(let Err(event) = self.$handler.try_handle_event(context, event, buffer) else {
                    return Ok(());
                };)*
                Err(event)
            }
        }

        $(impl $crate::Dispatches<$handler_ty> for $name {
            const PREFIX: u8 = ${index()};
            fn fetch_nest(&mut self) -> &mut $crate::HandlerNest<$handler_ty> {
                &mut self.$handler
            }
        })*
    };
}

pub struct HandlerNest<H: Handler> {
    handlers: Vec<ActiveHandler<H>>,
    sub_mapping: HashMap<H::Topic, HashMap<PathId, RequestId>>,
    dispatch: EventDispatch<H>,
}

impl<H: Handler> HandlerNest<H> {
    pub fn try_handle_event<T: Context>(
        &mut self,
        context: &mut T,
        event: T::ToSwarm,
        buffer: &mut RootPacketBuffer,
    ) -> Result<(), T::ToSwarm>
    where
        T::ToSwarm: From<<H::Context as Context>::ToSwarm>,
        T: SubContext<H::Context>,
    {
        let e = T::try_unpack_event(event)?;
        drain_filter(&mut self.handlers, |handler| {
            match handler
                .handler
                .try_complete(context.fragment(), &mut self.dispatch, &e)
            {
                Err(h) => Err(ActiveHandler {
                    handler: h,
                    path_id: handler.path_id,
                    request_id: handler.request_id,
                }),
                Ok(r) => {
                    if let Some(pid) = handler.path_id {
                        buffer.push(&r, (handler.request_id, pid))
                    }
                    Ok(())
                }
            }
        })
        .for_each(drop);
        Ok(())
    }

    pub fn dispatch<T: Context>(
        &mut self,
        context: &mut T,
        path_id: Option<PathId>,
        message: DispatchMessage<'_>,
        buffer: &mut RootPacketBuffer,
    ) -> Result<(), DispatchError>
    where
        T::ToSwarm: From<<H::Context as Context>::ToSwarm>,
        T: SubContext<H::Context>,
    {
        let request =
            H::Request::decode(&mut &*message.payload.0).ok_or(DispatchError::InvalidRequest)?;
        match H::spawn(
            context.fragment(),
            &request,
            &mut self.dispatch,
            (message.prefix, message.request_id),
        ) {
            Err(handler) => {
                self.handlers.push(ActiveHandler {
                    handler,
                    path_id,
                    request_id: message.request_id,
                });
            }
            Ok(response) => {
                if let Some(pid) = path_id {
                    buffer.push(&response, (message.request_id, pid))
                }
            }
        }

        Ok(())
    }

    pub fn subscribe(&mut self, pid: PathId, rid: RequestId, Reminder(topic): Reminder) {
        let Some(topic) = H::Topic::decode(&mut &topic[..]) else {
            return;
        };

        self.sub_mapping.entry(topic).or_default().insert(pid, rid);
    }

    pub fn disconnected(&mut self, pid: PathId) {
        // TODO: could be slow
        for (_, pids) in self.sub_mapping.iter_mut() {
            pids.remove(&pid);
        }
    }

    pub fn drain_events(
        &mut self,
    ) -> impl Iterator<
        Item = (
            iter::Map<
                hash_map::Iter<'_, PathId, RequestId>,
                fn((&PathId, &RequestId)) -> (PathId, RequestId),
            >,
            &mut [u8],
        ),
    > {
        fn map((pid, rid): (&PathId, &RequestId)) -> (PathId, RequestId) {
            (*pid, *rid)
        }
        let mapping = &self.sub_mapping;
        self.dispatch.drain().filter_map(move |(topic, buffer)| {
            let pids = mapping.get(&topic)?.iter().map(map as _);
            Some((pids, buffer))
        })
    }
}

impl<H: Handler> Default for HandlerNest<H> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            dispatch: EventDispatch::default(),
            sub_mapping: HashMap::new(),
        }
    }
}

mod impls;

pub use impls::*;

pub struct ActiveHandler<H: Handler> {
    pub request_id: RequestId,
    pub path_id: Option<PathId>,
    pub handler: H,
}

component_utils::gen_unique_id!(RequestId);

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("invalid prefix: {0}")]
    InvalidPrefix(u8),
    #[error("invalid request")]
    InvalidRequest,
}

type RequestMeta = (u8, RequestId);

pub trait Handler: Sized {
    type Context: Context;
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Event<'a>: Codec<'a> = Infallible;
    type Topic: Eq + std::hash::Hash + for<'a> Codec<'a>;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        dispatch: &mut EventDispatch<Self>,
        meta: RequestMeta,
    ) -> Result<Self::Response<'a>, Self>;
    fn try_complete<'a>(
        self,
        context: &'a mut Self::Context,
        dispatch: &mut EventDispatch<Self>,
        event: &'a <Self::Context as Context>::ToSwarm,
    ) -> Result<Self::Response<'a>, Self>;
}

pub trait SyncHandler: Sized {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Event<'a>: Codec<'a> = Infallible;
    type Context: Context;
    type Topic: Eq + std::hash::Hash + for<'a> Codec<'a>;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        dispatch: &mut EventDispatch<Self>,
        meta: RequestMeta,
    ) -> Self::Response<'a>;
}

impl<T: SyncHandler> Handler for T {
    type Context = T::Context;
    type Event<'a> = T::Event<'a>;
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;
    type Topic = T::Topic;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        dispatch: &mut EventDispatch<Self>,
        meta: RequestMeta,
    ) -> Result<Self::Response<'a>, Self> {
        Ok(Self::execute(context, request, dispatch, meta))
    }

    fn try_complete<'a>(
        self,
        _: &'a mut Self::Context,
        _: &mut EventDispatch<Self>,
        _: &'a <Self::Context as Context>::ToSwarm,
    ) -> Result<Self::Response<'a>, Self> {
        Err(self)
    }
}

pub struct EventDispatch<H: Handler> {
    inner: PacketBuffer<H::Topic>,
}

impl<H: Handler> EventDispatch<H> {
    pub fn push(&mut self, topic: H::Topic, event: &H::Event<'_>) {
        self.inner.push(event, topic);
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (H::Topic, &mut [u8])> {
        self.inner.drain()
    }
}

impl<H: Handler> Default for EventDispatch<H> {
    fn default() -> Self {
        Self {
            inner: PacketBuffer::new(),
        }
    }
}

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

    pub async fn dispatch<H: Handler>(
        &mut self,
        topic: impl Into<Option<H::Topic>>,
        request: H::Request<'_>,
    ) -> Result<H::Response<'_>, RequestError>
    where
        S: Dispatches<H>,
    {
        let id = RequestId::new();

        self.buffer.clear();
        self.buffer.push(S::PREFIX);
        id.encode(&mut self.buffer);
        request.encode(&mut self.buffer);

        let (tx, rx) = libp2p::futures::channel::oneshot::channel();
        use libp2p::futures::SinkExt;
        self.sink
            .send(RequestInit::Request(RawRequest {
                id,
                topic: topic.into().as_ref().map(Codec::to_bytes),
                payload: std::mem::take(&mut self.buffer),
                channel: tx,
            }))
            .await
            .map_err(|_| RequestError::ChannelClosed)?;
        self.buffer = rx.await.map_err(|_| RequestError::ChannelClosed)?;
        H::Response::decode(&mut &self.buffer[..]).ok_or(RequestError::InvalidResponse)
    }

    pub async fn dispatch_direct<H: Handler>(
        &mut self,
        stream: &mut EncryptedStream,
        request: &H::Request<'_>,
    ) -> Result<H::Response<'_>, RequestError>
    where
        S: Dispatches<H>,
    {
        let id = RequestId::new();

        self.buffer.clear();
        self.buffer.push(S::PREFIX);
        id.encode(&mut self.buffer);
        request.encode(&mut self.buffer);

        stream.write(&mut self.buffer);

        self.buffer = stream
            .next()
            .await
            .ok_or(RequestError::ChannelClosed)?
            .map_err(|_| RequestError::ChannelClosed)?;

        DispatchResponse::<H::Response<'_>>::decode(&mut &self.buffer[..])
            .ok_or(RequestError::InvalidResponse)
            .map(|r| r.response)
    }

    pub fn build_packet<H: Handler>(request: &H::Request<'_>, buffer: &mut Vec<u8>) -> RequestId
    where
        S: Dispatches<H>,
    {
        let id = RequestId::new();

        buffer.clear();
        buffer.push(S::PREFIX);
        id.encode(buffer);
        request.encode(buffer);
        id
    }

    pub fn parse_response<H: Handler>(response: &[u8]) -> Result<H::Response<'_>, RequestError>
    where
        S: Dispatches<H>,
    {
        H::Response::decode(&mut &response[..]).ok_or(RequestError::InvalidResponse)
    }

    pub async fn dispatch_direct_batch<'a, 'b, H: Handler>(
        &'a mut self,
        stream: &mut EncryptedStream,
        requests: impl Iterator<Item = H::Request<'b>>,
    ) -> Result<impl Iterator<Item = (H::Response<'a>, H::Request<'b>)>, RequestError>
    where
        S: Dispatches<H>,
    {
        let mut mapping = HashMap::new();
        for request in requests {
            let id = RequestId::new();

            self.buffer.clear();
            self.buffer.push(S::PREFIX);
            id.encode(&mut self.buffer);
            request.encode(&mut self.buffer);

            stream.write(&mut self.buffer);
            mapping.insert(id, request);
        }

        let mut stream = stream.by_ref().take(mapping.len());
        while let Some(packet) = stream.next().await {
            self.buffer
                .extend(packet.map_err(|_| RequestError::ChannelClosed)?);
        }

        Ok(
            (0..mapping.len()).scan(self.buffer.as_slice(), move |b, _| {
                DispatchResponse::<H::Response<'_>>::decode(b)
                    .and_then(|r| Some((r.response, mapping.remove(&r.request_id)?)))
            }),
        )
    }

    pub fn subscribe<H: Handler>(
        &mut self,
        topic: H::Topic,
    ) -> Result<(Subscription<H>, SubsOwner<H>), RequestError>
    where
        S: Dispatches<H>,
    {
        let (tx, rx) = libp2p::futures::channel::mpsc::channel(0);
        let request_id = RequestId::new();
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

pub struct SubsOwner<H: Handler> {
    id: RequestId,
    send_back: libp2p::futures::channel::mpsc::Sender<RequestInit>,
    phantom: std::marker::PhantomData<H>,
}

impl<H: Handler> Clone for SubsOwner<H> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            send_back: self.send_back.clone(),
            phantom: std::marker::PhantomData,
        }
    }
}

impl<H: Handler> Drop for SubsOwner<H> {
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
    CloseSubscription(RequestId),
}

impl RequestInit {
    pub fn topic(&self) -> &[u8] {
        match self {
            RequestInit::Request(r) => r.topic.as_deref().unwrap_or_default(),
            RequestInit::Subscription(s) => &s.topic,
            RequestInit::CloseSubscription(_) => &[],
        }
    }
}

pub struct Subscription<H> {
    buffer: Vec<u8>,
    events: libp2p::futures::channel::mpsc::Receiver<SubscriptionMessage>,
    phantom: std::marker::PhantomData<H>,
}

impl<H: Handler> Subscription<H> {
    pub async fn next(&mut self) -> Option<H::Event<'_>> {
        self.buffer = self.events.next().await?;
        H::Event::decode(&mut &self.buffer[..])
    }
}

pub struct SubscriptionInit {
    pub request_id: RequestId,
    pub topic: Vec<u8>,
    pub payload: Vec<u8>,
    pub channel: libp2p::futures::channel::mpsc::Sender<SubscriptionMessage>,
}

pub type SubscriptionMessage = Vec<u8>;

pub struct RawRequest {
    pub id: RequestId,
    pub topic: Option<Vec<u8>>,
    pub payload: Vec<u8>,
    pub channel: libp2p::futures::channel::oneshot::Sender<RawResponse>,
}

pub type RawResponse = Vec<u8>;

component_utils::gen_simple_error! {
    error RequestError {
        InvalidResponse => "invalid response",
        ChannelClosed => "channel closed",
    }
}

pub type RootPacketBuffer = PacketBuffer<(RequestId, PathId)>;

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
        packet.encode(&mut self.packets);
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
}

impl<T: NetworkBehaviour> Context for T {
    type ToSwarm = T::ToSwarm;
}

pub trait SubContext<F: Context>: Context
where
    Self::ToSwarm: From<F::ToSwarm>,
{
    fn fragment(&mut self) -> &mut F;
    fn try_unpack_event(event: Self::ToSwarm) -> Result<F::ToSwarm, Self::ToSwarm>;
}

impl<T: NetworkBehaviour> SubContext<T> for T {
    fn fragment(&mut self) -> &mut T {
        self
    }

    fn try_unpack_event(event: Self::ToSwarm) -> Result<T::ToSwarm, Self::ToSwarm> {
        Ok(event)
    }
}

pub trait Dispatches<H: Handler> {
    const PREFIX: u8;
    fn fetch_nest(&mut self) -> &mut HandlerNest<H>;
}

component_utils::protocol! {'a:
    struct DispatchMessage<'a> {
        prefix: u8,
        request_id: RequestId,
        payload: Reminder<'a>,
    }
}

pub struct DispatchResponse<T> {
    pub request_id: RequestId,
    pub response: T,
}

impl<'a, T: Codec<'a>> Codec<'a> for DispatchResponse<T> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.request_id.encode(buffer);
        self.response.encode(buffer);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(Self {
            request_id: RequestId::decode(buffer)?,
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
                        Err(item) => ptr::write(self.read, item),
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
        let mut v = vec![1, 2, 3, 4, 5, 6, 7, 8];

        let odd = drain_filter(&mut v, |x| (x % 2 == 0).then_some(x).ok_or(x)).collect::<Vec<_>>();

        assert_eq!(odd, vec![1, 3, 5, 7]);
        assert_eq!(v, vec![2, 4, 6, 8]);
    }
}
