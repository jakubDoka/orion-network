#![cfg_attr(not(feature = "std"), no_std)]
use aes_gcm::{
    aead::{generic_array::GenericArray, Tag},
    aes::cipher::Unsigned,
    AeadCore, AeadInPlace, Aes256Gcm, KeyInit, KeySizeUser, Nonce,
};

#[cfg(feature = "getrandom")]
use {aes_gcm::aead::OsRng, pqc_kyber::RngCore};

#[macro_export]
macro_rules! impl_transmute {
    ($($type:ty,)*) => {$(
        impl $crate::TransmutationCircle for $type {
            type Serialized = [u8; core::mem::size_of::<Self>()];

            fn into_bytes(self) -> Self::Serialized {
                unsafe { core::mem::transmute(self) }
            }

            fn from_bytes(bytes: Self::Serialized) -> Self {
                unsafe { core::mem::transmute(bytes) }
            }

            fn as_bytes(&self) -> &Self::Serialized {
                unsafe { core::mem::transmute(self) }
            }

            fn from_ref(bytes: &Self::Serialized) -> &Self {
                unsafe { core::mem::transmute(bytes) }
            }

            fn try_from_slice(slice: &[u8]) -> Option<&Self> {
                if slice.len() == core::mem::size_of::<Self>() {
                    Some(unsafe { core::mem::transmute(slice.as_ptr()) })
                } else {
                    None
                }
            }
        }
    )*};
}

pub trait TransmutationCircle: Sized {
    type Serialized: AsRef<[u8]>;

    fn into_bytes(self) -> Self::Serialized;
    fn from_bytes(bytes: Self::Serialized) -> Self;
    fn as_bytes(&self) -> &Self::Serialized;
    fn from_ref(bytes: &Self::Serialized) -> &Self;
    fn try_from_slice(slice: &[u8]) -> Option<&Self>;
}

pub type Serialized<T> = <T as TransmutationCircle>::Serialized;

pub mod enc;
pub mod hash;
pub mod sign;

pub use hash::{AnyHash, Hash};

#[cfg(all(feature = "getrandom", feature = "std"))]
pub fn new_secret() -> SharedSecret {
    let mut secret = [0; SHARED_SECRET_SIZE];
    OsRng.fill_bytes(&mut secret);
    secret
}

#[cfg(all(feature = "getrandom", feature = "std"))]
pub fn decrypt(data: &mut [u8], secret: SharedSecret) -> Option<&mut [u8]> {
    if data.len() < NONCE_SIZE + TAG_SIZE {
        return None;
    }

    let (data, postfix) = data.split_at_mut(data.len() - NONCE_SIZE - TAG_SIZE);
    let nonce = <Nonce<<Aes256Gcm as AeadCore>::NonceSize>>::from_slice(&postfix[TAG_SIZE..]);
    let tag = <Tag<Aes256Gcm>>::from_slice(&postfix[..TAG_SIZE]);
    let cipher = Aes256Gcm::new(&GenericArray::from(secret));
    cipher
        .decrypt_in_place_detached(nonce, enc::ASOC_DATA, data, tag)
        .ok()
        .map(|()| data)
}

#[cfg(all(feature = "getrandom", feature = "std"))]
pub fn encrypt(data: &mut Vec<u8>, secret: SharedSecret) {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let cipher = Aes256Gcm::new(&GenericArray::from(secret));
    let tag = cipher
        .encrypt_in_place_detached(&nonce, enc::ASOC_DATA, data)
        .unwrap();

    data.extend_from_slice(tag.as_slice());
    data.extend_from_slice(nonce.as_slice());
}

#[cfg(all(feature = "getrandom", feature = "std"))]
const NONCE_SIZE: usize = <<Aes256Gcm as AeadCore>::NonceSize as Unsigned>::USIZE;
#[cfg(all(feature = "getrandom", feature = "std"))]
const TAG_SIZE: usize = <<Aes256Gcm as AeadCore>::TagSize as Unsigned>::USIZE;

#[derive(Debug, Clone, Copy)]
pub struct FixedAesPayload<const SIZE: usize> {
    data: [u8; SIZE],
    tag: Tag<Aes256Gcm>,
    nonce: Nonce<<Aes256Gcm as AeadCore>::NonceSize>,
}

impl<const SIZE: usize> FixedAesPayload<SIZE> {
    #[cfg(feature = "getrandom")]
    fn new(mut data: [u8; SIZE], key: SharedSecret, asoc_data: &[u8]) -> Self {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let cipher = Aes256Gcm::new(&GenericArray::from(key));
        let tag = cipher
            .encrypt_in_place_detached(&nonce, asoc_data, &mut data)
            .expect("cannot fail from the implementation");
        Self { data, tag, nonce }
    }

    fn decrypt(self, key: SharedSecret, asoc_data: &[u8]) -> Result<[u8; SIZE], aes_gcm::Error> {
        let mut data = self.data;
        let cipher = Aes256Gcm::new(&GenericArray::from(key));
        cipher
            .decrypt_in_place_detached(&self.nonce, asoc_data, &mut data, &self.tag)
            .map(|()| data)
    }
}

pub const SHARED_SECRET_SIZE: usize = <<Aes256Gcm as KeySizeUser>::KeySize as Unsigned>::USIZE;

pub type SharedSecret = [u8; SHARED_SECRET_SIZE];