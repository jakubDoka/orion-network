use {
    crate::{Serialized, TransmutationCircle},
    core::{marker::PhantomData, ops::Deref},
};

pub const HASH_SIZE: usize = 32;

pub struct Hash<T = ()>([u8; HASH_SIZE], PhantomData<T>);

impl<T: TransmutationCircle> Hash<T> {
    pub fn new(data: &T) -> Self {
        Self(blake3::hash(data.as_bytes().as_ref()).into(), PhantomData)
    }

    pub fn from_raw(data: &Serialized<T>) -> Self {
        Self(blake3::hash(data.as_ref()).into(), PhantomData)
    }
}

impl<T> Hash<T> {
    pub fn combine(left: Self, right: Self) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(left.as_ref());
        hasher.update(right.as_ref());
        Self(hasher.finalize().into(), PhantomData)
    }
}

impl Hash {
    pub fn from_slice(slice: &[u8]) -> Self {
        Self(blake3::hash(slice).into(), PhantomData)
    }

    pub fn with_nonce(data: &[u8], nonce: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        hasher.update(&nonce.to_be_bytes());
        Self(hasher.finalize().into(), PhantomData)
    }
}

impl<T> Default for Hash<T> {
    fn default() -> Self {
        Self([0; HASH_SIZE], PhantomData)
    }
}

impl<T> Clone for Hash<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Hash<T> {}

impl<T> Deref for Hash<T> {
    type Target = [u8; HASH_SIZE];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, T> TryFrom<&'a [u8]> for Hash<T> {
    type Error = <[u8; HASH_SIZE] as TryFrom<&'a [u8]>>::Error;

    fn try_from(slice: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self(slice.try_into()?, PhantomData))
    }
}

impl<T> From<[u8; HASH_SIZE]> for Hash<T> {
    fn from(hash: [u8; HASH_SIZE]) -> Self {
        Self(hash, PhantomData)
    }
}

impl<T> From<Hash<T>> for [u8; HASH_SIZE] {
    fn from(hash: Hash<T>) -> Self {
        hash.0
    }
}

impl<T> AsRef<[u8; HASH_SIZE]> for Hash<T> {
    fn as_ref(&self) -> &[u8; HASH_SIZE] {
        &self.0
    }
}

impl<T> AsRef<[u8]> for Hash<T> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl<T> PartialEq for Hash<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T> Eq for Hash<T> {}

impl<T> std::hash::Hash for Hash<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl<T> std::fmt::Debug for Hash<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        display_hex(&self.0, f)
    }
}

impl<T> std::fmt::Display for Hash<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        display_hex(&self.0, f)
    }
}

fn display_hex(bytes: &[u8], f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    for byte in bytes {
        write!(f, "{:02x}", byte)?;
    }
    Ok(())
}
