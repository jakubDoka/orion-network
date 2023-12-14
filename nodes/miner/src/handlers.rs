pub use {chat::*, profile::*};
use {
    chat_logic::{Protocol, ProtocolResult, Topic},
    component_utils::{codec, Codec},
    libp2p::PeerId,
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
    ($name:ident {$(
        $handler:ty,
    )*}) => {
        pub struct $name($(
           $crate::extractors::HandlerNest<$handler>,
        )*);

        impl Default for $name {
            fn default() -> Self {
                Self($(
                    ${ignore(handler)}
                    $crate::extractors::HandlerNest::default(),
                ),*)
            }
        }

        impl $name {
            pub fn execute<C>(
                &mut self,
                cx: &mut C,
                req: $crate::handlers::Request<'_>,
                bp: &mut impl $crate::handlers::ProvideRequestBuffer,
            ) -> Result<(), $crate::handlers::HandlerExecError>
                where $($handler: $crate::handlers::Handler<C>,)*
            {

                match req.prefix {
                    $(<<$handler as Handler<C>>::Protocol as Protocol>::PREFIX => self.${index(0)}.execute(cx, req, bp),)*
                    _ => Err($crate::handlers::HandlerExecError::UnknownPrefix),
                }
            }

            pub fn try_complete<C, E>(
                &mut self,
                cx: &mut C,
                event: &E,
                bp: &mut impl $crate::handlers::ProvideRequestBuffer,
            ) -> Option<()>
            where
                $(
                    E: $crate::extractors::TryUnwrap<<$handler as Handler<C>>::Event>,
                    $handler: Handler<C>,
                )*
            {

                (false $(
                    ${ignore(handler)}
                    || self.${index(0)}.try_complete(cx, event, bp).is_some()
                )*).then_some(())
            }
        }
    };
}

mod chat;
mod peer_search;
mod profile;
//mod replicated;

pub trait ProvideStorage {
    fn store_mut(&mut self) -> &mut crate::Storage;
}

pub trait EventEmmiter<T: Topic> {
    fn push<'a>(&mut self, topic: T, event: T::Event<'a>);
}

pub type HandlerResult<'a, H, C> = Result<
    Result<
        <<H as Handler<C>>::Protocol as Protocol>::Response<'a>,
        <<H as Handler<C>>::Protocol as Protocol>::Error,
    >,
    H,
>;

pub trait Handler<C>: Sized {
    type Protocol: Protocol;
    type Event;

    fn execute<'a>(
        cx: Scope<'a, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self, C>;

    fn execute_and_encode(
        cx: Scope<'_, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
        buffer: &mut impl codec::Buffer,
    ) -> Result<Option<()>, Self> {
        Self::execute(cx, req).map(move |r| r.encode(buffer))
    }

    fn resume<'a>(self, cx: Scope<'a, C>, enent: &'a Self::Event) -> HandlerResult<'a, Self, C>;

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

impl<C, H: SyncHandler<C>> Handler<C> for H {
    type Event = Infallible;
    type Protocol = H;

    fn execute<'a>(
        cx: Scope<'a, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self, C> {
        Ok(Self::execute(cx, req))
    }

    fn resume<'a>(self, _: Scope<'a, C>, e: &'a Self::Event) -> HandlerResult<'a, Self, C> {
        match *e {}
    }
}

pub struct Scope<'a, C> {
    pub cx: &'a mut C,
    pub origin: RequestOrigin,
    pub call_id: CallId,
    pub prefix: u8,
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
    fn try_unwrap(&self) -> Option<&T>;
}

impl<T> TryUnwrap<T> for T {
    fn try_unwrap(&self) -> Option<&T> {
        Some(self)
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

impl<H> HandlerNest<H> {
    pub fn execute<C>(
        &mut self,
        cx: &mut C,
        req: Request<'_>,
        bp: &mut impl ProvideRequestBuffer,
    ) -> Result<(), HandlerExecError>
    where
        H: Handler<C>,
    {
        let mut buffer = bp.request_buffer(req.id, req.origin);
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
            &mut buffer,
        ) {
            self.handlers.push(HandlerInstance {
                prefix: req.prefix,
                id: req.id,
                origin: req.origin,
                handler: con,
            });
        }

        Ok(())
    }

    pub fn try_complete<C, E: TryUnwrap<H::Event>>(
        &mut self,
        cx: &mut C,
        event: &E,
        bp: &mut impl ProvideRequestBuffer,
    ) -> Option<()>
    where
        H: Handler<C>,
    {
        let event = event.try_unwrap()?;

        let (i, res) = self.handlers.iter_mut().enumerate().find_map(|(i, h)| {
            let read = unsafe { std::ptr::read(&h.handler) };
            let mut buffer = bp.request_buffer(h.id, h.origin);
            match read.resume_and_encode(
                Scope {
                    cx,
                    origin: h.origin,
                    call_id: h.id,
                    prefix: h.prefix,
                },
                event,
                &mut buffer,
            ) {
                Ok(res) => Some((i, res)),
                Err(new_handler) => unsafe {
                    std::ptr::write(&mut h.handler, new_handler);
                    None
                },
            }
        })?;

        std::mem::forget(self.handlers.swap_remove(i));

        if res.is_none() {
            log::info!("the response buffer is owerwhelmed");
        }

        Some(())
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

pub struct Request<'a> {
    pub origin: RequestOrigin,
    pub id: CallId,
    pub prefix: u8,
    pub body: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub enum RequestOrigin {
    Client(PathId),
    Miner(PeerId),
    NotImportant,
}
