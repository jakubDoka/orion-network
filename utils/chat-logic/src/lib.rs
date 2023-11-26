#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
#![feature(specialization)]
#![allow(incomplete_features)]
use {
    component_utils::{Codec, Reminder},
    libp2p::swarm::NetworkBehaviour,
    onion::PathId,
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
            $handler: $crate::HandlerNest<$handler_ty>,
        )*}

        impl $name {
            pub fn dispatch<T>(&mut self, context: &mut T, message: $crate::DispatchMessage<'_>, buffet: &mut $crate::PacketBuffer<$crate::RequestId>) -> Result<(), $crate::DispatchError>
            where
                $(T: $crate::SubContext<<$handler_ty as $crate::Handler>::Context>,
                T::ToSwarm: From<<<$handler_ty as $crate::Handler>::Context as $crate::Context>::ToSwarm>,)*
            {
                match message.prefix {
                    $(${index()} => self.$handler.dispatch(context, message, buffet),)*
                    p => Err($crate::DispatchError::InvalidPrefix(p)),
                }
            }

            pub fn try_handle_event<T: $crate::Context>(&mut self, context: &mut T, event: T::ToSwarm, buffer: &mut $crate::PacketBuffer<$crate::RequestId>)
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
        })*
    };
}

pub struct HandlerNest<H: Handler> {
    handlers: Vec<ActiveHandler<H>>,
    dispatch: EventDispatch<H>,
}

impl<H: Handler> HandlerNest<H> {
    pub fn try_handle_event<T: Context>(
        &mut self,
        context: &mut T,
        event: T::ToSwarm,
        buffer: &mut PacketBuffer<RequestId>,
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
                    request_id: handler.request_id,
                }),
                Ok(r) => {
                    buffer.push(&r, handler.request_id);
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
        message: DispatchMessage<'_>,
        buffer: &mut PacketBuffer<RequestId>,
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
                    request_id: message.request_id,
                });
            }
            Ok(response) => buffer.push(&response, message.request_id),
        }

        Ok(())
    }

    pub fn drain_events(&mut self) -> impl Iterator<Item = (H::Topic, &mut [u8])> {
        self.dispatch.drain()
    }
}

impl<H: Handler> Default for HandlerNest<H> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            dispatch: EventDispatch::default(),
        }
    }
}

mod impls;

use std::convert::Infallible;

pub use impls::*;

pub struct ActiveHandler<H: Handler> {
    pub request_id: RequestId,
    pub handler: H,
}

component_utils::gen_unique_id!(RequestId);

impl Codec<'_> for RequestId {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.0.encode(buffer);
    }

    fn decode(buffer: &mut &[u8]) -> Option<Self> {
        usize::decode(buffer).map(Self)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("invalid prefix: {0}")]
    InvalidPrefix(u8),
    #[error("invalid request")]
    InvalidRequest,
}

type RequestMeta = (u8, RequestId);

pub trait Handler: Sized {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Event<'a>: Codec<'a> = Infallible;
    type Context: Context;
    type Topic: Eq + std::hash::Hash + Codec<'static> = Infallible;

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
    type Topic: Eq + std::hash::Hash + Codec<'static> = Infallible;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        dispatch: &mut EventDispatch<Self>,
        meta: RequestMeta,
    ) -> Self::Response<'a>;
}

impl<T: SyncHandler> Handler for T {
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;
    type Event<'a> = T::Event<'a>;
    type Context = T::Context;
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
        stream: PathId,
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
                request_id: RequestId::new(),
                stream,
                payload: Vec::new(),
                channel: tx,
            }))
            .await
            .map_err(|_| RequestError::ChannelClosed)?;
        self.buffer = rx.await.map_err(|_| RequestError::ChannelClosed)?;
        H::Response::decode(&mut &self.buffer[..]).ok_or(RequestError::InvalidResponse)
    }

    pub fn subscribe<H: Handler>(
        &mut self,
        topic: H::Topic,
    ) -> Result<Subscription<H>, RequestError>
    where
        S: Dispatches<H>,
    {
        let (tx, rx) = libp2p::futures::channel::mpsc::channel(0);
        self.sink
            .try_send(RequestInit::Subscription(SubscriptionInit {
                topic: topic.to_bytes(),
                channel: tx,
            }))
            .map_err(|_| RequestError::ChannelClosed)?;

        Ok(Subscription {
            buffer: Vec::new(),
            events: rx,
            phantom: std::marker::PhantomData,
        })
    }
}

pub type RequestStream = libp2p::futures::channel::mpsc::Receiver<RequestInit>;

pub enum RequestInit {
    Request(RawRequest),
    Subscription(SubscriptionInit),
}

pub struct Subscription<H> {
    buffer: Vec<u8>,
    events: libp2p::futures::channel::mpsc::Receiver<SubscriptionMessage>,
    phantom: std::marker::PhantomData<H>,
}

impl<H: Handler> Subscription<H> {
    pub async fn next(&mut self) -> Option<H::Event<'_>> {
        use libp2p::futures::StreamExt;
        self.buffer = self.events.next().await?;
        H::Event::decode(&mut &self.buffer[..])
    }
}

pub struct SubscriptionInit {
    pub topic: Vec<u8>,
    pub channel: libp2p::futures::channel::mpsc::Sender<SubscriptionMessage>,
}

pub type SubscriptionMessage = Vec<u8>;

pub struct RawRequest {
    pub request_id: RequestId,
    pub stream: PathId,
    pub payload: Vec<u8>,
    pub channel: libp2p::futures::channel::oneshot::Sender<RawResponse>,
}

pub type RawResponse = Vec<u8>;

pub enum RequestError {
    InvalidResponse,
    ChannelClosed,
}

pub struct PacketBuffer<M> {
    packets: Vec<u8>,
    bunds: Vec<(usize, M)>,
}

impl<M> PacketBuffer<M> {
    pub fn new() -> Self {
        Self {
            packets: Vec::new(),
            bunds: Vec::new(),
        }
    }

    fn push<'a>(&mut self, packet: &impl Codec<'a>, id: M) {
        self.bunds.push((self.packets.len(), id));
        packet.encode(&mut self.packets);
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (M, &mut [u8])> {
        let slice = unsafe { std::mem::transmute::<_, &mut [u8]>(self.packets.as_mut_slice()) };
        unsafe { self.packets.set_len(0) }
        self.bunds.drain(..).scan(slice, move |slice, (bund, req)| {
            let (head, tail) = std::mem::take(slice).split_at_mut(bund);
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
