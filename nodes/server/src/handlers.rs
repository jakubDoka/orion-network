pub use {chat::*, profile::*, replicated::*};
use {
    chat_logic::{Protocol, ProtocolResult, Subscribe},
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
    ($cond:expr, Ok($resp:expr)) => {
        if !$cond {
            return Ok(Err($resp));
        }
    };

    (let $var:pat = $expr:expr, Ok($resp:expr)) => {
        let $var = $expr else {
            return Ok(Err($resp));
        };
    };
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
                    ${ignore($handler)}
                    $crate::handlers::HandlerNest::default(),
                )*)
            }
        }

        impl $name {
            pub fn execute(
                &mut self,
                mut cx: $crate::Context<'_>,
                req: $crate::handlers::Request<'_>,
                bp: &mut impl component_utils::codec::Buffer,
            ) -> Result<$crate::handlers::ExitedEarly, $crate::handlers::HandlerExecError>
            {
                $(if <<$handler as Handler>::Protocol as Protocol>::PREFIX == req.prefix
                    { return self.${index(0)}.execute($crate::extract_ctx!(cx), req, bp) })*
                Err($crate::handlers::HandlerExecError::UnknownPrefix)
            }

            pub fn try_complete<E>(
                &mut self,
                mut cx: $crate::Context<'_>,
                mut event: E,
                bp: &mut impl component_utils::codec::Buffer,
            ) -> Result<(RequestOrigin, CallId), E>
            where
                $(
                    E: $crate::handlers::TryUnwrap<<$handler as Handler>::Event>,
                    E: From<<$handler as Handler>::Event>,
                )*
            {
                $(
                    ${ignore($handler)}
                    match self.${index(0)}.try_complete($crate::extract_ctx!(cx), event, bp) {
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
mod populating;
mod profile;
mod replicated;
mod retry;

impl SyncHandler for Subscribe {
    fn execute<'a>(mut sc: Scope<'a>, req: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        if let RequestOrigin::Client(path) = sc.origin {
            sc.cx.subscribe(req, sc.call_id, path);
        }

        Ok(())
    }
}

pub type HandlerResult<'a, H> = Result<
    Result<
        <<H as Handler>::Protocol as Protocol>::Response<'a>,
        <<H as Handler>::Protocol as Protocol>::Error,
    >,
    H,
>;

pub trait Handler: Sized {
    type Protocol: Protocol;
    type Event;

    fn execute<'a>(
        cx: Scope<'a>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self>;

    fn execute_and_encode(
        cx: Scope<'_>,
        req: <Self::Protocol as Protocol>::Request<'_>,
        buffer: &mut impl codec::Buffer,
    ) -> Result<Option<()>, Self> {
        Self::execute(cx, req).map(move |r| r.encode(buffer))
    }

    fn resume<'a>(self, cx: Scope<'a>, enent: &'a Self::Event) -> HandlerResult<'a, Self>;

    fn resume_and_encode(
        self,
        cx: Scope<'_>,
        enent: &Self::Event,
        buffer: &mut impl codec::Buffer,
    ) -> Result<Option<()>, Self> {
        self.resume(cx, enent).map(move |r| r.encode(buffer))
    }
}

pub trait SyncHandler: Protocol {
    fn execute<'a>(cx: Scope<'a>, req: Self::Request<'_>) -> ProtocolResult<'a, Self>;
}

pub struct Sync<T>(T);

impl<H: SyncHandler> Handler for Sync<H> {
    type Event = Infallible;
    type Protocol = H;

    fn execute<'a>(
        cx: Scope<'a>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        Ok(H::execute(cx, req))
    }

    fn resume<'a>(self, _: Scope<'a>, e: &'a Self::Event) -> HandlerResult<'a, Self> {
        match e {
            &i => match i {},
        }
    }
}

pub struct Scope<'a> {
    pub cx: crate::Context<'a>,
    pub origin: RequestOrigin,
    pub call_id: CallId,
    pub prefix: u8,
}

impl<'a> Scope<'a> {
    fn reborrow(&mut self) -> Scope<'_> {
        Scope {
            cx: crate::extract_ctx!(self.cx),
            origin: self.origin,
            call_id: self.call_id,
            prefix: self.prefix,
        }
    }
}

impl<'a> Deref for Scope<'a> {
    type Target = crate::Context<'a>;

    fn deref(&self) -> &Self::Target {
        &self.cx
    }
}

impl<'a> DerefMut for Scope<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cx
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

impl<H: Handler> HandlerNest<H> {
    pub fn execute(
        &mut self,
        cx: crate::Context<'_>,
        req: Request<'_>,
        bp: &mut impl component_utils::codec::Buffer,
    ) -> Result<ExitedEarly, HandlerExecError> {
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

    pub fn try_complete<E: TryUnwrap<H::Event> + From<H::Event>>(
        &mut self,
        mut cx: crate::Context<'_>,
        event: E,
        bp: &mut impl codec::Buffer,
    ) -> Result<(RequestOrigin, CallId), E> {
        let event = event.try_unwrap()?;

        let (i, res, origin, id) = self
            .handlers
            .iter_mut()
            .enumerate()
            .find_map(|(i, h)| {
                let read = unsafe { std::ptr::read(&h.handler) };
                match read.resume_and_encode(
                    Scope {
                        cx: crate::extract_ctx!(cx),
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

component_utils::protocol! {'a:
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RequestOrigin {
        Client: PathId,
        Server: PeerId,
    }
}
