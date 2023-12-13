#![feature(iter_next_chunk)]
#![feature(slice_take)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
#![feature(impl_trait_in_assoc_type)]
#![feature(extract_if)]

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
macro_rules! compose_protocols {
    ($(fn $for:ident<$lt:lifetime>($($req:ty),*) -> Result<$resp:ty, $error:ty>;)*) => {$(
        pub enum $for {}
        impl $crate::extractors::Protocol for $for {
            const PREFIX: u8 = ${index(0)};
            type Error = $error;
            #[allow(unused_parens)]
            type Request<$lt> = ($($req),*);
            type Response<$lt> = $resp;
        }
    )*};
}

mod extractors;
mod impls;

pub use {extractors::*, impls::*, rpc::CallId};
