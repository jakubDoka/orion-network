#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
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
        pub struct $name {$(
           $handler: Vec<$crate::ActiveHandler<$handler_ty>>,
        )*}

        impl Default for $name {
            fn default() -> Self {
                Self {$(
                    $handler: Vec::new(),
                )*}
            }
        }

        impl $name {
            pub fn dispatch<T>(&mut self, context: &mut T, message: $crate::DispatchMessage<'_>, buffet: &mut $crate::PacketBuffer) -> Result<(), $crate::DispatchError>
            where
                $(T: $crate::MultiBehavior<<$handler_ty as $crate::Handler>::Context>,
                T::ToSwarm: From<<<$handler_ty as $crate::Handler>::Context as NetworkBehaviour>::ToSwarm>,)*
            {
                match message.prefix {
                    $(${index()} => $crate::dispatch(context, message, buffet, &mut self.$handler),)*
                    p => Err($crate::DispatchError::InvalidPrefix(p)),
                }
            }

            pub fn try_handle_event<T: $crate::MinimalNetworkBehaviour>(&mut self, context: &mut T, event: T::ToSwarm, buffer: &mut $crate::PacketBuffer)
                -> Result<(), <T as $crate::MinimalNetworkBehaviour>::ToSwarm>
            where
                $(T: $crate::MultiBehavior<<$handler_ty as $crate::Handler>::Context>,
                T::ToSwarm: From<<<$handler_ty as $crate::Handler>::Context as NetworkBehaviour>::ToSwarm>,)*
            {
                $(
                    let Err(event) = $crate::try_handle_event(context, event, buffer, &mut self.$handler) else {
                        return Ok(());
                    };
                )*

                Err(event)
            }
        }

        $(impl $crate::Dispatches<$handler_ty> for $name {
            const PREFIX: u8 = ${index()};
        })*
    };
}

pub fn try_handle_event<T: MinimalNetworkBehaviour, H: Handler>(
    context: &mut T,
    event: T::ToSwarm,
    buffer: &mut PacketBuffer,
    handlers: &mut Vec<ActiveHandler<H>>,
) -> Result<(), T::ToSwarm>
where
    T::ToSwarm: From<<H::Context as MinimalNetworkBehaviour>::ToSwarm>,
    T: MultiBehavior<H::Context>,
{
    let e = T::try_unpack_event(event)?;
    drain_filter(handlers, |handler| {
        let buffer = OutPacket::<H>::new(handler.request_id, buffer);
        match H::try_complete(handler.handler, context.fragment(), &e, buffer) {
            Some(h) => Err(ActiveHandler {
                handler: h,
                request_id: handler.request_id,
            }),
            None => Ok(()),
        }
    })
    .for_each(drop);
    Ok(())
}

pub fn dispatch<T: MinimalNetworkBehaviour, H: Handler>(
    context: &mut T,
    message: DispatchMessage<'_>,
    buffer: &mut PacketBuffer,
    handlers: &mut Vec<ActiveHandler<H>>,
) -> Result<(), DispatchError>
where
    T::ToSwarm: From<<H::Context as MinimalNetworkBehaviour>::ToSwarm>,
    T: MultiBehavior<H::Context>,
{
    let request =
        H::Request::decode(&mut &*message.payload.0).ok_or(DispatchError::InvalidRequest)?;
    let buffer = OutPacket::<H>::new(message.request_id, buffer);
    if let Some(handler) = H::spawn(
        context.fragment(),
        request,
        (message.prefix, message.request_id),
        buffer,
    ) {
        handlers.push(ActiveHandler {
            handler,
            request_id: message.request_id,
        });
    }
    Ok(())
}

mod impls;

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
};

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

pub enum DispatchError {
    InvalidPrefix(u8),
    InvalidRequest,
}

type RequestMeta = (u8, RequestId);

pub trait Handler: Sized {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Context: MinimalNetworkBehaviour;
    type Broadcast: for<'a> Broadcast<Event<'a> = Self::Response<'a>> = NoOpBroadcast<Self>;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: Self::Request<'a>,
        meta: RequestMeta,
        resp_buffer: OutPacket<'_, Self>,
    ) -> Option<Self>;
    fn try_complete<'a>(
        self,
        context: &'a mut Self::Context,
        event: &'a <Self::Context as MinimalNetworkBehaviour>::ToSwarm,
        resp_buffer: OutPacket<'_, Self>,
    ) -> Option<Self>;
}

pub trait Broadcast: Sized {
    type Event<'a>: Codec<'a>;
    type Projection<'a>: Codec<'a>;

    fn dispatch<'a>(
        &mut self,
        event: Self::Event<'a>,
        targets: &mut Vec<PathId>,
    ) -> Option<Self::Projection<'a>>;
}

pub struct NoOpBroadcast<T>(std::marker::PhantomData<T>);

impl<T: Handler> Broadcast for NoOpBroadcast<T> {
    type Event<'a> = T::Response<'a>;
    type Projection<'a> = Infallible;

    fn dispatch<'a>(
        &mut self,
        _: Self::Event<'a>,
        _: &mut Vec<PathId>,
    ) -> Option<Self::Projection<'a>> {
        None
    }
}

pub trait AsyncHandler: Sized {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Context: MinimalNetworkBehaviour;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: Self::Request<'a>,
        meta: RequestMeta,
    ) -> Result<Self::Response<'a>, Self>;
    fn try_complete<'a>(
        self,
        context: &'a mut Self::Context,
        event: &'a <Self::Context as MinimalNetworkBehaviour>::ToSwarm,
    ) -> Result<Self::Response<'a>, Self>;
}

impl<T: AsyncHandler> Handler for T {
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;
    type Context = T::Context;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: Self::Request<'a>,
        meta: RequestMeta,
        buffer: OutPacket<'_, Self>,
    ) -> Option<Self> {
        match Self::spawn(context, request, meta) {
            Ok(response) => {
                buffer.push(&response);
                None
            }
            Err(handler) => Some(handler),
        }
    }

    fn try_complete<'a>(
        self,
        context: &'a mut Self::Context,
        event: &'a <Self::Context as MinimalNetworkBehaviour>::ToSwarm,
        resp_buffer: OutPacket<'_, Self>,
    ) -> Option<Self> {
        match Self::try_complete(self, context, event) {
            Ok(response) => {
                resp_buffer.push(&response);
                None
            }
            Err(handler) => Some(handler),
        }
    }
}

pub trait SyncHandler: Sized {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Context: MinimalNetworkBehaviour;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: Self::Request<'a>,
        meta: RequestMeta,
    ) -> Self::Response<'a>;
}

impl<T: SyncHandler> AsyncHandler for T {
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;
    type Context = T::Context;

    fn spawn<'a>(
        context: &'a mut Self::Context,
        request: Self::Request<'a>,
        meta: RequestMeta,
    ) -> Result<Self::Response<'a>, Self> {
        Ok(Self::execute(context, request, meta))
    }

    fn try_complete<'a>(
        self,
        _: &'a mut Self::Context,
        _: &'a <Self::Context as MinimalNetworkBehaviour>::ToSwarm,
    ) -> Result<Self::Response<'a>, Self> {
        Err(self)
    }
}

pub trait Event {
    type Key: Eq + std::hash::Hash;

    fn key(&self) -> Self::Key;
}

pub struct RequestDispatch<S> {
    buffer: Vec<u8>,
    sink: libp2p::futures::channel::mpsc::Sender<RawRequest>,
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
            .send(RawRequest {
                request_id: RequestId::new(),
                stream,
                payload: Vec::new(),
                channel: tx,
            })
            .await
            .map_err(|_| RequestError::ChannelClosed)?;
        self.buffer = rx.await.map_err(|_| RequestError::ChannelClosed)?;
        H::Response::decode(&mut &self.buffer[..]).ok_or(RequestError::InvalidResponse)
    }
}

pub type RequestStream = libp2p::futures::channel::mpsc::Receiver<RawRequest>;

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

pub struct PacketBuffer {
    packets: Vec<u8>,
    bunds: Vec<(usize, RequestId)>,
}

impl PacketBuffer {
    pub fn new() -> Self {
        Self {
            packets: Vec::new(),
            bunds: Vec::new(),
        }
    }

    fn push<'a>(&mut self, packet: &impl Codec<'a>, id: RequestId) {
        self.bunds.push((self.packets.len(), id));
        packet.encode(&mut self.packets);
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (RequestId, &mut [u8])> {
        let slice = unsafe { std::mem::transmute::<_, &mut [u8]>(self.packets.as_mut_slice()) };
        unsafe { self.packets.set_len(0) }
        self.bunds.drain(..).scan(slice, move |slice, (bund, req)| {
            let (head, tail) = std::mem::take(slice).split_at_mut(bund);
            *slice = tail;
            Some((req, head))
        })
    }
}

pub struct OutPacket<'a, T> {
    id: RequestId,
    buff: &'a mut PacketBuffer,
    phantom: std::marker::PhantomData<T>,
}

impl<'a, T> OutPacket<'a, T> {
    pub fn new(id: RequestId, buff: &'a mut PacketBuffer) -> Self {
        Self {
            id,
            buff,
            phantom: std::marker::PhantomData,
        }
    }

    pub fn push<'b>(self, packet: &T::Response<'b>)
    where
        T: Handler,
    {
        self.buff.push(packet, self.id);
    }
}

pub trait MinimalNetworkBehaviour {
    type ToSwarm;
}

impl<T: NetworkBehaviour> MinimalNetworkBehaviour for T {
    type ToSwarm = T::ToSwarm;
}

pub trait MultiBehavior<F: MinimalNetworkBehaviour>: MinimalNetworkBehaviour
where
    Self::ToSwarm: From<F::ToSwarm>,
{
    fn fragment(&mut self) -> &mut F;
    fn try_unpack_event(event: Self::ToSwarm) -> Result<F::ToSwarm, Self::ToSwarm>;
}

impl<T: NetworkBehaviour> MultiBehavior<T> for T {
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
