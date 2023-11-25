use core::marker::PhantomData;

use crate::TransmutationCircle;

pub type Hash<T> = (AnyHash, PhantomData<T>);
pub type AnyHash = [u8; 32];

pub fn try_from_slice<T: TransmutationCircle>(slice: &[u8]) -> Option<Hash<T>> {
    Some((slice.try_into().ok()?, PhantomData))
}

pub fn new<T: TransmutationCircle>(data: &T) -> Hash<T> {
    (blake3::hash(data.as_bytes().as_ref()).into(), PhantomData)
}

pub fn verify<T: TransmutationCircle>(data: &T, hash: Hash<T>) -> bool {
    blake3::hash(data.as_bytes().as_ref()) == hash.0
}
