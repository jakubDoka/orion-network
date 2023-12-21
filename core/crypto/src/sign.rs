#[cfg(feature = "getrandom")]
use aes_gcm::aead::OsRng;
use ed25519_dalek::{SigningKey, VerifyingKey};
#[cfg(feature = "std")]
use {ed25519_dalek::Signer, thiserror::Error};

impl_transmute! {
    Signature,
    KeyPair,
    PublicKey,
}

pub type Ed = [u8; 32];

#[derive(Clone, Copy)]
pub struct Signature {
    post: [u8; falcon::BYTES],
    pre: ed25519_dalek::Signature,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyPair {
    post: falcon::Keypair,
    pre: ed25519_dalek::SecretKey,
}

impl KeyPair {
    #[cfg(feature = "getrandom")]
    pub fn new() -> Self {
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0u8; falcon::SEED_BYTES];
        OsRng.fill_bytes(&mut seed);
        let post = falcon::Keypair::new(&seed).expect("yea, whatever");
        let pre = SigningKey::generate(&mut OsRng).to_bytes();
        Self { post, pre }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            post: *self.post.public_key(),
            pre: SigningKey::from_bytes(&self.pre).verifying_key().to_bytes(),
        }
    }

    #[cfg(feature = "getrandom")]
    pub fn sign(&self, data: &[u8]) -> Signature {
        let post = self.post.sign(data, OsRng).expect("really now?");
        let pre = SigningKey::from(&self.pre)
            .try_sign(data)
            .expect("cannot fail from the implementation");
        Signature { post, pre }
    }

    pub fn pre_quantum(&self) -> Ed {
        self.pre
    }
}

#[cfg(feature = "getrandom")]
impl Default for KeyPair {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PublicKey {
    pub post: falcon::PublicKey,
    pub pre: [u8; ed25519_dalek::PUBLIC_KEY_LENGTH],
}

impl PublicKey {
    pub fn verify(&self, data: &[u8], signature: &Signature) -> Result<(), SignatureError> {
        VerifyingKey::from_bytes(&self.pre)
            .and_then(|vk| vk.verify_strict(data, &signature.pre))
            .map_err(SignatureError::PreQuantum)?;
        self.post
            .verify(data, &signature.post)
            .then_some(())
            .ok_or(SignatureError::PostQuantum)?;
        Ok(())
    }
}

#[cfg(feature = "std")]
#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("dilithium signature failsed")]
    PostQuantum,
    #[error("ed25519 signature failed: {0}")]
    PreQuantum(ed25519_dalek::SignatureError),
}

#[cfg(not(feature = "std"))]
pub enum SignatureError {
    PostQuantum,
    PreQuantum(ed25519_dalek::SignatureError),
}

#[cfg(test)]
mod test {
    #[test]
    fn test_sign_verify() {
        use super::*;
        let keypair = KeyPair::new();
        let data = b"hello world";
        let signature = keypair.sign(data);
        let public_key = keypair.public_key();
        public_key.verify(data, &signature).unwrap();
        public_key
            .verify(b"deez nust", &signature)
            .expect_err("invalid signature");
    }
}
