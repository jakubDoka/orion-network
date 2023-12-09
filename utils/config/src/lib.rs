#![feature(array_chunks)]
use std::str::FromStr;

#[macro_export]
macro_rules! env_config {
    ($(
        $key:ident: $ty:ty $(= $default:expr)?,
    )*) => {
        $(

            #[allow(non_snake_case)]
            let $key = std::env::var(stringify!($key))
            $(.or(Ok::<_, std::env::VarError>({
                let default: $ty = $default;
                default
                }.to_string())))?
            ;
        )*

        if false $(|| $key.is_err())* {
            eprintln!("Missing environment variables:");
            $(
                if let Err(_) = $key {
                    eprintln!("\t{}", stringify!($key));
                }
            )*
            std::process::exit(1);
        }

        $(
            #[allow(non_snake_case)]
            let $key = $key.unwrap().parse::<$ty>();
        )*

        if false $(|| $key.is_err())* {
            eprintln!("Invalid environment variables:");
            $(
                if let Err(e) = $key {
                    eprintln!("\t{}: {e}", stringify!($key));
                }
            )*
            std::process::exit(1);
        }

        $(
            #[allow(non_snake_case)]
            let $key = $key.unwrap();
        )*
    };
}

pub struct List<T>(pub Vec<T>);

impl<T: FromStr> FromStr for List<T> {
    type Err = <T as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut peers = Vec::new();
        for peer in s.split(',') {
            peers.push(peer.parse::<T>()?);
        }
        Ok(Self(peers))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Key([u8; 32]);

impl Key {
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<[u8]> for Key {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl FromStr for Key {
    type Err = SecretKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(SecretKeyError::InvalidLength(s.len()));
        }

        let mut bytes = [0; 32];
        for (&[a, b], dest) in s.as_bytes().array_chunks().zip(bytes.iter_mut()) {
            fn hex_to_u8(e: u8) -> Result<u8, SecretKeyError> {
                Ok(match e {
                    b'0'..=b'9' => e - b'0',
                    b'a'..=b'f' => e - b'a' + 10,
                    b'A'..=b'F' => e - b'A' + 10,
                    _ => return Err(SecretKeyError::InvalidHex),
                })
            }

            *dest = hex_to_u8(a)? << 4 | hex_to_u8(b)?;
        }

        Ok(Self(bytes))
    }
}

#[derive(Debug)]
pub enum SecretKeyError {
    InvalidLength(usize),
    InvalidHex,
}

impl std::fmt::Display for SecretKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretKeyError::InvalidLength(len) => {
                write!(f, "invalid length (expected 64): {}", len)
            }
            SecretKeyError::InvalidHex => write!(f, "invalid hex (expected [0-9a-fA-F])"),
        }
    }
}

impl std::error::Error for SecretKeyError {}
