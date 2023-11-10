use std::{io, sync::Arc, u32, usize};

use arrayvec::{ArrayString, ArrayVec};
use futures::{AsyncRead, AsyncReadExt};
#[cfg(feature = "kad")]
use libp2p_core::multihash::Multihash;
#[cfg(feature = "kad")]
use libp2p_identity::PeerId;

pub fn encode_len(len: usize) -> [u8; 4] {
    (len as u32).to_be_bytes()
}

pub fn decode_len(bytes: [u8; 4]) -> usize {
    u32::from_be_bytes(bytes) as usize
}

#[macro_export]
macro_rules! protocol {
    (@low $(#[$meta:meta])* enum $lt:lifetime $name:ident$(<$lt2:lifetime>)? {$(
        $variant:ident $(: $value:ty)? => $id:literal,
    )*}) => {
       $(#[$meta])*
        pub enum $name$(<$lt2>)? {
            $($variant$(($value))?,)*
        }

        impl<$lt> $crate::codec::Codec<$lt> for $name$(<$lt2>)? {
            fn encode(&self, buffer: &mut Vec<u8>) {
                match self {
                    $($crate::protocol!(@pattern $variant value $($value)?) => {
                        ($id as u8).encode(buffer);
                        $(<$value as $crate::codec::Codec<$lt>>::encode(value, buffer);)?
                    })*
                }
            }

            fn decode(buffer: &mut &$lt [u8]) -> Option<Self> {
                let value = <u8>::decode(buffer)?;
                match value {
                    $( $id => {
                        Some(Self::$variant$((<$value as $crate::codec::Codec<$lt>>::decode(buffer)?))?)
                    })*
                    _ => None,
                }
            }
        }
    };

    (@pattern $variant:ident $var:ident) => {Self::$variant};
    (@pattern $variant:ident $var:ident $value:ty) => {Self::$variant($var)};

    (@low $(#[$meta:meta])* struct $lt:lifetime $name:ident$(<$lt2:lifetime>)? {$(
        $field:ident: $ty:ty,
    )*}) => {
        $(#[$meta])*
        pub struct $name$(<$lt2>)? {
            $(pub $field: $ty,)*
        }

        impl<$lt> $crate::codec::Codec<$lt> for $name$(<$lt2>)? {
            fn encode(&self, buffer: &mut Vec<u8>) {
                $(<$ty as $crate::codec::Codec<$lt>>::encode(&self.$field, buffer);)*
            }

            fn decode(buffer: &mut &$lt [u8]) -> Option<Self> {
                Some(Self {
                    $($field: <$ty as $crate::codec::Codec<$lt>>::decode(buffer)?,)*
                })
            }
        }
    };

    ($lt:lifetime: $($(#[$meta:meta])* $keyword:ident $name:ident$(<$lt2:lifetime>)? {$(
        $field:ident$(: $ty:ty)? $(=> $id:literal)?,
    )*})*) => {
        $($crate::protocol!(@low $(#[$meta])* $keyword $lt $name$(<$lt2>)? {$(
            $field $(: $ty)? $(=> $id)?,
        )*});)*
    };
}

pub trait Codec<'a>: Sized {
    fn encode(&self, buffer: &mut Vec<u8>);
    fn decode(buffer: &mut &'a [u8]) -> Option<Self>;

    fn to_bytes(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.encode(&mut buffer);
        buffer
    }

    fn to_packet(&self) -> Vec<u8> {
        let mut buffer = vec![0; 4];
        self.encode(&mut buffer);
        buffer.splice(..4, encode_len(buffer.len() - 4));
        buffer
    }
}

pub trait CodecExt: Codec<'static> {
    #[allow(async_fn_in_trait)]
    async fn from_stream(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        let mut len = [0; 4];
        stream.read_exact(&mut len).await?;
        let len = decode_len(len);
        let mut buffer = vec![0; len];
        stream.read_exact(&mut buffer).await?;
        // SAFETY: compiler is stupid, we implement Codec<'static>
        Self::decode(&mut unsafe { std::mem::transmute(buffer.as_slice()) })
            .ok_or_else(|| io::ErrorKind::InvalidData.into())
    }
}

impl<T: Codec<'static>> CodecExt for T {}

impl Codec<'_> for () {
    fn encode(&self, _buffer: &mut Vec<u8>) {}

    fn decode(_buffer: &mut &[u8]) -> Option<Self> {
        Some(())
    }
}

pub struct Base128Bytes(u64, bool);

impl Base128Bytes {
    pub fn new(value: u64) -> Self {
        Self(value, true)
    }
}

impl Iterator for Base128Bytes {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0 == 0 && !std::mem::take(&mut self.1) {
            return None;
        }

        let byte = (self.0 & 0b0111_1111) as u8;
        self.0 >>= 7;
        if self.0 != 0 {
            self.0 |= 0b1000_0000;
        }
        Some(byte)
    }
}

fn base128_encode(mut value: u64, buffer: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0b0111_1111) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0b1000_0000;
        }
        buffer.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn base128_decode(buffer: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    let mut shift = 0;
    let worst_case_size = 10;
    for (advanced, byte) in (*buffer).iter().take(worst_case_size).copied().enumerate() {
        value |= ((byte & 0b0111_1111) as u64) << shift;
        shift += 7;
        if byte & 0b1000_0000 == 0 {
            *buffer = &buffer[advanced + 1..];
            return Some(value);
        }
    }
    None
}

macro_rules! impl_int {
    ($($t:ty),*) => {
        $(
            impl<'a> Codec<'a> for $t {
                fn encode(&self, buffer: &mut Vec<u8>) {
                    base128_encode(*self as u64, buffer);
                }

                fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                    base128_decode(buffer).map(|v| v as $t)
                }
            }
        )*
    };
}

impl_int!(u16, u32, u64, u128, usize);

impl<'a> Codec<'a> for bool {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.push(*self as u8);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let Some((&bool_byte @ (0 | 1), rest)) = buffer.split_first() else {
            return None;
        };
        *buffer = rest;
        Some(bool_byte == 1)
    }
}

impl<'a> Codec<'a> for u8 {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.push(*self);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let (&byte, rest) = buffer.split_first()?;
        *buffer = rest;
        Some(byte)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Reminder<'a>(pub &'a [u8]);

impl<'a> Codec<'a> for Reminder<'a> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(self.0);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(Self(std::mem::take(buffer)))
    }
}

/// Some structures suport optimized codec when placed as the last part of the message
#[derive(Debug, Clone, Copy)]
pub struct Unbound<T>(pub T);

impl<'a> Codec<'a> for Unbound<Vec<u8>> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.0);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(Self(std::mem::take(buffer).to_vec()))
    }
}

impl<'a> Codec<'a> for String {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.as_str().encode(buffer);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let str = <&str>::decode(buffer)?;
        Some(str.to_string())
    }
}

impl<'a> Codec<'a> for &'a str {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.as_bytes().encode(buffer);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let bytes = <&[u8]>::decode(buffer)?;
        std::str::from_utf8(bytes).ok()
    }
}

impl<'a, T: Codec<'a>, const LEN: usize> Codec<'a> for ArrayVec<T, LEN> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.len().encode(buffer);
        for i in self {
            i.encode(buffer);
        }
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let len = <usize>::decode(buffer)?;
        if len > LEN {
            return None;
        }
        let mut s = Self::default();
        for _ in 0..len {
            s.push(<T>::decode(buffer)?);
        }
        Some(s)
    }
}

impl<'a, const LEN: usize> Codec<'a> for ArrayString<LEN> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.as_bytes().encode(buffer);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let bytes = <&[u8]>::decode(buffer)?;
        let str = std::str::from_utf8(bytes).ok()?;
        Self::from(str).ok()
    }
}

impl<'a, T: Codec<'a>> Codec<'a> for Vec<T> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.len().encode(buffer);
        for i in self {
            i.encode(buffer);
        }
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let len = <usize>::decode(buffer)?;
        let mut s = Self::with_capacity(len);
        for _ in 0..len {
            s.push(<T>::decode(buffer)?);
        }
        Some(s)
    }
}

impl<'a, T: Codec<'a>> Codec<'a> for Arc<[T]> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.len().encode(buffer);
        for i in self.iter() {
            i.encode(buffer);
        }
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(<Vec<T>>::decode(buffer)?.into())
    }
}

impl<'a> Codec<'a> for &'a [u8] {
    fn encode(&self, buffer: &mut Vec<u8>) {
        self.len().encode(buffer);
        buffer.extend_from_slice(self);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let len = <usize>::decode(buffer)?;
        if buffer.len() < len {
            return None;
        }

        let (bytes, rest) = buffer.split_at(len);
        *buffer = rest;
        Some(bytes)
    }
}

impl<'a, T: Codec<'a>, const SIZE: usize> Codec<'a> for [T; SIZE] {
    fn encode(&self, buffer: &mut Vec<u8>) {
        for i in self {
            i.encode(buffer);
        }
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let tries = [(); SIZE].map(|_| <T>::decode(buffer));
        if tries.iter().any(|t| t.is_none()) {
            return None;
        }
        Some(tries.map(|t| t.unwrap()))
    }
}

#[cfg(feature = "kad")]
impl<'a> Codec<'a> for PeerId {
    fn encode(&self, buffer: &mut Vec<u8>) {
        let mh = Multihash::from(*self);
        mh.write(buffer).expect("unreachable");
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Multihash::<64>::read(buffer)
            .ok()
            .and_then(|mh| PeerId::from_multihash(mh).ok())
    }
}

impl<'a, T: Codec<'a>> Codec<'a> for Option<T> {
    fn encode(&self, buffer: &mut Vec<u8>) {
        match self {
            Some(t) => {
                true.encode(buffer);
                t.encode(buffer);
            }
            None => false.encode(buffer),
        }
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        let is_some = <bool>::decode(buffer)?;
        Some(if is_some {
            Some(<T>::decode(buffer)?)
        } else {
            None
        })
    }
}
