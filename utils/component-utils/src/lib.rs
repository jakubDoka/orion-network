#![cfg_attr(not(feature = "std"), no_std)]
#![feature(array_chunks)]
#![feature(macro_metavar_expr)]

#[macro_export]
macro_rules! decl_stream_protocol {
    ($decl_name:ident = $name:literal) => {
        pub const $decl_name: StreamProtocol = StreamProtocol::new(concat!(
            "/",
            env!("CARGO_PKG_NAME"),
            "/",
            $name,
            "/",
            env!("CARGO_PKG_VERSION")
        ));
    };
}

#[macro_export]
macro_rules! gen_config {
    (
        $($(#[$comment:meta])* $required_field:ident: $required_ty:ty,)*
        ;;
        $($(#[$comment2:meta])* $field:ident: $ty:ty = $default:expr,)*
    ) => {
        pub struct Config {
            $(
                $(#[$comment])*
                pub $required_field: $required_ty,
            )*
            $(
                $field: $ty,
            )*
        }

        impl Config {
            pub fn new($($required_field: $required_ty),*) -> Self {
                Self {
                    $($required_field,)*
                    $($field: $default,)*
                }
            }

            $(
                $(#[$comment2])*
                #[doc = concat!("Defaults to ", stringify!($default))]
                pub fn $field(mut self, $field: $ty) -> Self {
                    self.$field = $field;
                    self
                }
            )*
        }
    };
}

#[macro_export]
macro_rules! gen_unique_id {
    ($vis:vis $ty:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $ty(usize);

        impl $ty {
            $vis fn new() -> Self {
                static COUNTER: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                Self(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
            }

            $vis fn whatever() -> Self {
                Self(usize::MAX)
            }
        }

        impl $crate::Codec<'_> for $ty {
            fn encode(&self, buffer: &mut Vec<u8>) {
                self.0.encode(buffer);
            }

            fn decode(buffer: &mut &[u8]) -> Option<Self> {
                usize::decode(buffer).map(Self)
            }
        }
    };
}

#[macro_export]
macro_rules! gen_simple_error {
    ($(
        error $name:ident {$(
            $variant:ident$(($ty:ty))? => $message:literal,
        )*}
    )*) => {$(
        #[derive(Debug, Clone, Copy, $crate::thiserror::Error)]
        #[repr(u8)]
        pub enum $name {$(
            #[error($message)]
            $variant$(($ty))?,
        )*}


        impl<'a> $crate::Codec<'a> for $name {
            fn encode(&self, buffer: &mut Vec<u8>) {
                match self {$(
                    Self::$variant$((val) ${ignore(ty)})? => {
                        buffer.push(${index()});
                        $(<$ty as $crate::Codec>::encode(val, buffer);)?
                    }
                )*}
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                let index = buffer.get(0)?;
                *buffer = &buffer[1..];

                match index {$(
                    ${index()} => {
                        $(let val = <$ty as $crate::Codec>::decode(buffer)?;)?
                        Some(Self::$variant$( (val) ${ignore(ty)} )?)
                    }
                )* _ => None, }
            }
        }
    )*};
}

#[cfg(feature = "std")]
pub mod codec;
#[cfg(feature = "std")]
pub mod kad;
#[cfg(feature = "std")]
pub mod stream;

pub use arrayvec;
use core::task::Waker;
#[cfg(feature = "std")]
pub use {codec::*, futures, kad::*, libp2p, stream::*, thiserror};

pub struct DropFn<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> DropFn<F> {
    pub fn new(f: F) -> Self {
        Self(Some(f))
    }
}

impl<F: FnOnce()> Drop for DropFn<F> {
    fn drop(&mut self) {
        self.0.take().expect("we drop only once")()
    }
}

pub fn arrstr_to_array<const SIZE: usize>(s: arrayvec::ArrayString<SIZE>) -> [u8; SIZE] {
    let mut arr = [0xff; SIZE];
    arr[..s.len()].copy_from_slice(s.as_bytes());
    arr
}

pub fn array_to_arrstr<const SIZE: usize>(arr: [u8; SIZE]) -> Option<arrayvec::ArrayString<SIZE>> {
    let mut s = arrayvec::ArrayString::<SIZE>::new();
    let len = arr.iter().rposition(|&x| x != 0xff).map_or(0, |x| x + 1);
    s.push_str(core::str::from_utf8(&arr[..len]).ok()?);
    Some(s)
}

pub trait FindAndRemove<T> {
    fn find_and_remove(&mut self, q: impl FnMut(&T) -> bool) -> Option<T>;
}

#[cfg(feature = "std")]
impl<T> FindAndRemove<T> for Vec<T> {
    fn find_and_remove(&mut self, q: impl FnMut(&T) -> bool) -> Option<T> {
        Some(self.swap_remove(self.iter().position(q)?))
    }
}

pub fn set_waker(old: &mut Option<Waker>, new: &Waker) {
    if let Some(old) = old {
        old.clone_from(new);
    } else {
        *old = Some(new.clone());
    }
}
