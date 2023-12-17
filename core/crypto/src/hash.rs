use {
    crate::{Serialized, TransmutationCircle},
    core::marker::PhantomData,
};

pub type Hash<T> = (AnyHash, PhantomData<T>);
pub type AnyHash = [u8; 32];

pub fn try_from_slice<T: TransmutationCircle>(slice: &[u8]) -> Option<Hash<T>> {
    Some((slice.try_into().ok()?, PhantomData))
}

pub fn new<T: TransmutationCircle>(data: &T) -> Hash<T> {
    (blake3::hash(data.as_bytes().as_ref()).into(), PhantomData)
}

pub fn new_slice(data: &[u8]) -> Hash<[u8]> {
    (blake3::hash(data).into(), PhantomData)
}

pub fn new_raw<T: TransmutationCircle>(data: &Serialized<T>) -> Hash<T> {
    (blake3::hash(data.as_ref()).into(), PhantomData)
}

pub fn verify<T: TransmutationCircle>(data: &T, hash: Hash<T>) -> bool {
    blake3::hash(data.as_bytes().as_ref()) == hash.0
}

pub fn new_with_nonce(data: &[u8], nonce: u64) -> AnyHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    hasher.update(&nonce.to_be_bytes());
    hasher.finalize().into()
}
