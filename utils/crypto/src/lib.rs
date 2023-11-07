use aes_gcm::{
    aead::{generic_array::GenericArray, OsRng, Tag},
    aes::cipher::Unsigned,
    AeadCore, AeadInPlace, Aes256Gcm, KeyInit, KeySizeUser, Nonce,
};
use pqc_kyber::RngCore;

use self::enc::ASOC_DATA;

#[macro_export]
macro_rules! impl_transmute {
    ($($type:ty, $size_const:ident, $serialized_alias:ident;)*) => {$(
        pub const $size_const: usize = std::mem::size_of::<$type>();
        pub type $serialized_alias = [u8; $size_const];

        impl From<$serialized_alias> for $type {
            fn from(bytes: $serialized_alias) -> Self {
                unsafe { std::mem::transmute(bytes) }
            }
        }

        impl From<$type> for $serialized_alias {
            fn from(signature: $type) -> Self {
                unsafe { std::mem::transmute(signature) }
            }
        }

        impl $type {
            #[allow(dead_code)]
            pub fn into_bytes(self) -> $serialized_alias {
                self.into()
            }
        }
    )*};
}

pub mod enc;
pub mod sign;

pub fn new_secret() -> SharedSecret {
    let mut secret = [0; SHARED_SECRET_SIZE];
    OsRng.fill_bytes(&mut secret);
    secret
}

pub fn decrypt(data: &mut [u8], secret: SharedSecret) -> Option<&mut [u8]> {
    if data.len() < NONCE_SIZE + TAG_SIZE {
        return None;
    }

    let (data, prefix) = data.split_at_mut(data.len() - NONCE_SIZE + TAG_SIZE);
    let nonce = <Nonce<<Aes256Gcm as AeadCore>::NonceSize>>::from_slice(&prefix[..TAG_SIZE]);
    let tag = <Tag<Aes256Gcm>>::from_slice(&prefix[TAG_SIZE..]);
    let cipher = Aes256Gcm::new(&GenericArray::from(secret));
    cipher
        .decrypt_in_place_detached(nonce, ASOC_DATA, data, tag)
        .ok()
        .map(|()| data)
}

pub fn encrypt(data: &mut Vec<u8>, secret: SharedSecret) {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let cipher = Aes256Gcm::new(&GenericArray::from(secret));
    let tag = cipher
        .encrypt_in_place_detached(&nonce, ASOC_DATA, data)
        .unwrap();

    data.extend_from_slice(tag.as_slice());
    data.extend_from_slice(nonce.as_slice());
}

const NONCE_SIZE: usize = <<Aes256Gcm as AeadCore>::NonceSize as Unsigned>::USIZE;
const TAG_SIZE: usize = <<Aes256Gcm as AeadCore>::TagSize as Unsigned>::USIZE;

#[derive(Debug, Clone, Copy)]
pub struct FixedAesPayload<const SIZE: usize> {
    data: [u8; SIZE],
    tag: Tag<Aes256Gcm>,
    nonce: Nonce<<Aes256Gcm as AeadCore>::NonceSize>,
}

impl<const SIZE: usize> FixedAesPayload<SIZE> {
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
