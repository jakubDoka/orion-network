#![feature(iter_next_chunk)]
#![feature(slice_take)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
#![feature(impl_trait_in_assoc_type)]
#![feature(extract_if)]

#[macro_export]
macro_rules! compose_protocols {
    ($(
        fn $for:ident
            <$lt:lifetime>
            ($($req:ty),*) -> Result<$resp:ty, $error:ty>
            $(where Topic($topic:ty): |$topic_arg:pat_param| $topic_extractor:expr)?;
    )*) => {$(
        #[allow(unused_parens)]
        pub enum $for {}
        impl $crate::extractors::Protocol for $for {
            const PREFIX: u8 = ${index(0)};
            type Error = $error;
            #[allow(unused_parens)]
            type Request<$lt> = ($($req),*);
            type Response<$lt> = $resp;
        }

        $(impl $crate::extractors::TopicProtocol for $for {
            type Topic = $topic;
            fn extract_topic($topic_arg: &Self::Request<'_>) -> Self::Topic {
                $topic_extractor
            }
        })?
    )*};
}

mod extractors;
mod impls;

pub use {extractors::*, impls::*, rpc::CallId};
