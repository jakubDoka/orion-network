pub use {chat::*, peer_search::*, profile::*, replicated::*};
use {
    chat_logic::{PossibleTopic, Protocol, ProtocolResult, Subscribe, Topic},
    component_utils::{codec, Codec},
    libp2p::{kad::store::RecordStore, PeerId},
    onion::PathId,
    rpc::CallId,
    std::{
        convert::Infallible,
        ops::{Deref, DerefMut},
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
    ($($name:ident {$(
        $handler:ty,
    )*})*) => {$(
        pub struct $name($(
           $crate::handlers::HandlerNest<$handler>,
        )*);

        impl Default for $name {
            fn default() -> Self {
                Self($(
                    ${ignore(handler)}
                    $crate::handlers::HandlerNest::default(),
                )*)
            }
        }

        impl $name {
            pub fn execute<C>(
                &mut self,
                cx: &mut C,
                req: $crate::handlers::Request<'_>,
                bp: &mut impl component_utils::codec::Buffer,
            ) -> Result<$crate::handlers::ExitedEarly, $crate::handlers::HandlerExecError>
                where $($handler: $crate::handlers::Handler<C>,)*
            {
                $(if <<$handler as HandlerTypes>::Protocol as Protocol>::PREFIX == req.prefix { return self.${index(0)}.execute(cx, req, bp) })*
                Err($crate::handlers::HandlerExecError::UnknownPrefix)
            }

            pub fn try_complete<C, E>(
                &mut self,
                cx: &mut C,
                mut event: E,
                bp: &mut impl component_utils::codec::Buffer,
            ) -> Result<(RequestOrigin, CallId), E>
            where
                $(
                    E: $crate::handlers::TryUnwrap<<$handler as HandlerTypes>::Event>,
                    E: From<<$handler as HandlerTypes>::Event>,
                    $handler: Handler<C>,
                )*
            {
                $(
                    ${ignore(handler)}
                    match self.${index(0)}.try_complete(cx, event, bp) {
                        Ok(res) => return Ok(res),
                        Err(e) => event = e,
                    }
                )*
                Err(event)
            }
        }
    )*};
}

mod chat;
mod peer_search;
mod profile;
mod replicated;

pub trait ProvideKad {
    fn kad_mut(&mut self) -> &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>;
}

pub trait ProvideRpc {
    fn rpc_mut(&mut self) -> &mut rpc::Behaviour;
}

pub trait ProvideKadAndRpc {
    fn kad_and_rpc_mut(
        &mut self,
    ) -> (
        &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
        &mut rpc::Behaviour,
    );
}

impl<T: ProvideKadAndRpc> ProvideKad for T {
    fn kad_mut(&mut self) -> &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static> {
        self.kad_and_rpc_mut().0
    }
}

impl<T: ProvideKadAndRpc> ProvideRpc for T {
    fn rpc_mut(&mut self) -> &mut rpc::Behaviour {
        self.kad_and_rpc_mut().1
    }
}

pub trait ProvideStorage {
    fn store_mut(&mut self) -> &mut crate::Storage;
}

pub trait EventEmmiter<T: Topic> {
    fn push(&mut self, topic: T, event: T::Event<'_>);
}

pub trait ProvideSubscription {
    fn subscribe(&mut self, topic: PossibleTopic, id: CallId, origin: PathId);
}

impl<C: ProvideSubscription> SyncHandler<C> for Subscribe {
    fn execute<'a>(sc: Scope<'a, C>, req: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        if let RequestOrigin::Client(path) = sc.origin {
            sc.cx.subscribe(req, sc.call_id, path);
        }

        Ok(())
    }
}

pub type HandlerResult<'a, H> = Result<
    Result<
        <<H as HandlerTypes>::Protocol as Protocol>::Response<'a>,
        <<H as HandlerTypes>::Protocol as Protocol>::Error,
    >,
    H,
>;

pub trait HandlerTypes {
    type Protocol: Protocol;
    type Event;
}

pub trait Handler<C>: HandlerTypes + Sized {
    fn execute<'a>(
        cx: Scope<'a, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self>;

    fn execute_and_encode(
        cx: Scope<'_, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
        buffer: &mut impl codec::Buffer,
    ) -> Result<Option<()>, Self> {
        Self::execute(cx, req).map(move |r| r.encode(buffer))
    }

    fn resume<'a>(self, cx: Scope<'a, C>, enent: &'a Self::Event) -> HandlerResult<'a, Self>;

    fn resume_and_encode(
        self,
        cx: Scope<'_, C>,
        enent: &Self::Event,
        buffer: &mut impl codec::Buffer,
    ) -> Result<Option<()>, Self> {
        self.resume(cx, enent).map(move |r| r.encode(buffer))
    }
}

pub trait SyncHandler<C>: Protocol {
    fn execute<'a>(cx: Scope<'a, C>, req: Self::Request<'_>) -> ProtocolResult<'a, Self>;
}

pub struct Sync<T>(T);

impl<T: Protocol> HandlerTypes for Sync<T> {
    type Event = Infallible;
    type Protocol = T;
}

impl<C, H: SyncHandler<C>> Handler<C> for Sync<H> {
    fn execute<'a>(
        cx: Scope<'a, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        Ok(H::execute(cx, req))
    }

    fn resume<'a>(self, _: Scope<'a, C>, e: &'a Self::Event) -> HandlerResult<'a, Self> {
        match *e {}
    }
}

pub struct Scope<'a, C> {
    pub cx: &'a mut C,
    pub origin: RequestOrigin,
    pub call_id: CallId,
    pub prefix: u8,
}

impl<'a, C> Scope<'a, C> {
    fn reborrow(&mut self) -> Scope<'_, C> {
        Scope {
            cx: &mut *self.cx,
            origin: self.origin,
            call_id: self.call_id,
            prefix: self.prefix,
        }
    }
}

impl<'a, C> Deref for Scope<'a, C> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        self.cx
    }
}

impl<'a, C> DerefMut for Scope<'a, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.cx
    }
}

pub trait TryUnwrap<T>: Sized {
    fn try_unwrap(self) -> Result<T, Self>;
}

impl<T> TryUnwrap<T> for T {
    fn try_unwrap(self) -> Result<T, Self> {
        Ok(self)
    }
}

pub trait ProvideRequestBuffer {
    fn request_buffer(&mut self, id: CallId, origin: RequestOrigin) -> impl codec::Buffer + '_;
}

pub struct HandlerNest<H> {
    handlers: Vec<HandlerInstance<H>>,
}

impl<H> Default for HandlerNest<H> {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }
}

pub type ExitedEarly = bool;

impl<H> HandlerNest<H> {
    pub fn execute<C>(
        &mut self,
        cx: &mut C,
        req: Request<'_>,
        bp: &mut impl component_utils::codec::Buffer,
    ) -> Result<ExitedEarly, HandlerExecError>
    where
        H: Handler<C>,
    {
        let decoded = <H::Protocol as Protocol>::Request::decode(&mut &*req.body)
            .ok_or(HandlerExecError::DecodeRequest)?;
        if let Err(con) = H::execute_and_encode(
            Scope {
                cx,
                origin: req.origin,
                call_id: req.id,
                prefix: req.prefix,
            },
            decoded,
            bp,
        ) {
            self.handlers.push(HandlerInstance {
                prefix: req.prefix,
                id: req.id,
                origin: req.origin,
                handler: con,
            });

            Ok(false)
        } else {
            Ok(true)
        }
    }

    pub fn try_complete<C, E: TryUnwrap<H::Event> + From<H::Event>>(
        &mut self,
        cx: &mut C,
        event: E,
        bp: &mut impl codec::Buffer,
    ) -> Result<(RequestOrigin, CallId), E>
    where
        H: Handler<C>,
    {
        let event = event.try_unwrap()?;

        let (i, res, origin, id) = self
            .handlers
            .iter_mut()
            .enumerate()
            .find_map(|(i, h)| {
                let read = unsafe { std::ptr::read(&h.handler) };
                match read.resume_and_encode(
                    Scope {
                        cx,
                        origin: h.origin,
                        call_id: h.id,
                        prefix: h.prefix,
                    },
                    &event,
                    bp,
                ) {
                    Ok(res) => Some((i, res, h.origin, h.id)),
                    Err(new_handler) => unsafe {
                        std::ptr::write(&mut h.handler, new_handler);
                        None
                    },
                }
            })
            .ok_or(event)?;

        std::mem::forget(self.handlers.swap_remove(i));

        if res.is_none() {
            log::info!("the response buffer is owerwhelmed");
        }

        Ok((origin, id))
    }
}

component_utils::gen_simple_error! {
    error HandlerExecError {
        DecodeRequest => "failed to decode request",
        UnknownPrefix => "unknown prefix",
    }
}

struct HandlerInstance<H> {
    prefix: u8,
    id: CallId,
    origin: RequestOrigin,
    handler: H,
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub prefix: u8,
    pub id: CallId,
    pub origin: RequestOrigin,
    pub body: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub enum RequestOrigin {
    Client(PathId),
    Miner(PeerId),
    NotImportant,
}
