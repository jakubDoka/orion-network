use {
    crate::RequestOrigin,
    component_utils::{codec, Codec},
    rpc::CallId,
    std::convert::Infallible,
};

#[macro_export]
macro_rules! compose_handlers_2 {
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
                req: $crate::extractors::Request<'_>,
                bp: &mut impl $crate::extractors::ProvideRequestBuffer,
            ) -> Result<(), $crate::extractors::HandlerExecError>
                where $($handler: $crate::extractors::Handler<C>,)*
            {

                match req.prefix {
                    $(${ignore(handler)} ${index(0)} => self.${index(0)}.execute(cx, req, bp),)*
                    _ => Err($crate::extractors::HandlerExecError::UnknownPrefix),
                }
            }

            pub fn try_complete<C, E>(
                &mut self,
                cx: &mut C,
                event: &E,
                bp: &mut impl $crate::extractors::ProvideRequestBuffer,
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

        $(impl Dispatches<$handler> for $name {
            const PREFIX: u8 = ${index(0)};
        })*
    };
}

#[macro_export]
macro_rules! impl_protocol {
    ($(fn $for:ident<$lt:lifetime>($req:ty) -> Result<$resp:ty, $error:ty>;)*) => {$(
        pub enum $for {}
        impl $crate::extractors::Protocol for $for {
            type Error = $error;
            type Request<$lt> = $req;
            type Response<$lt> = $resp;
        }
    )*};
}

pub trait Dispatches<T> {
    const PREFIX: u8;
}

pub fn prefix_of<U, T: Dispatches<U>>() -> u8 {
    T::PREFIX
}

pub trait Protocol {
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Error: for<'a> Codec<'a> + std::error::Error;
}

pub trait Topic: for<'a> Codec<'a> + std::hash::Hash + Eq {
    type Event<'a>: Codec<'a>;
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

pub struct Scope<'a, C> {
    pub cx: &'a mut C,
    pub origin: RequestOrigin,
    pub call_id: CallId,
    pub prefix: u8,
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
