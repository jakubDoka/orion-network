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
            <$lt:lifetime $(, $generic:ident $(: $trait:path)?)* $(,)?>
            ($($req:ty),*) -> Result<$resp:ty, $error:ty>
            $(where Topic($topic:ty): |$topic_arg:pat_param| $topic_extractor:expr)?;
    )*) => {$(
        #[allow(unused_parens)]
        pub struct $for<$($generic)*>(std::marker::PhantomData<($($generic),*)>, std::convert::Infallible);
        impl<$($generic $(: $trait)?)*> $crate::extractors::Protocol for $for<$($generic),*> {
            const PREFIX: u8 = ${index(0)};
            type Error = $error;
            #[allow(unused_parens)]
            type Request<$lt> = ($($req),*);
            type Response<$lt> = $resp;
        }
        $crate::compose_protocols!(@topic
            fn $for
                <$lt $(, $generic $(: $trait)?)*>
                ($($req),*) -> Result<$resp, $error>
                $(where Topic($topic): |$topic_arg| $topic_extractor)?
        );
    )*};

    (@topic
        fn $for:ident
            <$lt:lifetime $(, $generic:ident $(: $trait:path)?)* $(,)?>
            ($($req:ty),*) -> Result<$resp:ty, $error:ty>
            where Topic($topic:ty): |$topic_arg:pat_param| $topic_extractor:expr
    ) => {
        impl<$($generic $(: $trait)?)*> $crate::extractors::ExtractTopic for $for<$($generic),*> {
            type Topic = $topic;
            fn extract_topic($topic_arg: &Self::Request<'_>) -> Self::Topic {
                $topic_extractor
            }
        }
    };

    (@topic
        fn $for:ident
            <$lt:lifetime $(, $generic:ident $(: $trait:path)?)* $(,)?>
            ($($req:ty),*) -> Result<$resp:ty, $error:ty>
    ) => {

    };
}

mod extractors;
mod impls;

pub use {extractors::*, impls::*, rpc::CallId};
