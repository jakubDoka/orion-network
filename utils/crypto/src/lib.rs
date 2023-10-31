use aes_gcm::{
    aead::{generic_array::GenericArray, OsRng, Tag},
    aes::cipher::Unsigned,
    AeadCore, AeadInPlace, Aes256Gcm, KeyInit, KeySizeUser, Nonce,
};

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
    )*};
}

pub mod enc;
pub mod sign;

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
