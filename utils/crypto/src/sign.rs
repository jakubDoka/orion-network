#[cfg(feature = "getrandom")]
use aes_gcm::aead::OsRng;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
#[cfg(feature = "std")]
use thiserror::Error;

impl_transmute! {
    Signature, SIGNATURE_SIZE, SerializedSignature;
    KeyPair, KEY_PAIR_SIZE, SerializedKeyPair;
    PublicKey, PUBLIC_KEY_SIZE, SerializedPublicKey;
}

#[derive(Clone, Copy)]
pub struct Signature {
    dili: [u8; pqc_dilithium::SIGNBYTES],
    ed: ed25519_dalek::Signature,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyPair {
    pub dili: pqc_dilithium::Keypair,
    pub ed: ed25519_dalek::SecretKey,
}

impl KeyPair {
    #[cfg(feature = "getrandom")]
    pub fn new() -> Self {
        let dili = pqc_dilithium::Keypair::generate();
        let ed = SigningKey::generate(&mut OsRng).to_bytes();
        Self { dili, ed }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            dili: self.dili.public,
            ed: SigningKey::from_bytes(&self.ed).verifying_key().to_bytes(),
        }
    }

    pub fn sign(&self, data: &[u8]) -> Signature {
        let dili = self.dili.sign(data);
        let ed = SigningKey::from(&self.ed)
            .try_sign(data)
            .expect("cannot fail from the implementation");
        Signature { dili, ed }
    }
}

#[cfg(feature = "getrandom")]
impl Default for KeyPair {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub struct PublicKey {
    pub dili: [u8; pqc_dilithium::PUBLICKEYBYTES],
    pub ed: [u8; ed25519_dalek::PUBLIC_KEY_LENGTH],
}

impl PublicKey {
    pub fn verify(&self, data: &[u8], signature: &Signature) -> Result<(), SignatureError> {
        VerifyingKey::from_bytes(&self.ed)
            .and_then(|vk| vk.verify_strict(data, &signature.ed))
            .map_err(SignatureError::Ed)?;
        pqc_dilithium::verify(&signature.dili, data, &self.dili)
            .map_err(DiliSignError::from)
            .map_err(SignatureError::Dili)?;
        Ok(())
    }
}

#[cfg(feature = "std")]
#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("dilithium signature failsed: {0}")]
    Dili(DiliSignError),
    #[error("ed25519 signature failed: {0}")]
    Ed(ed25519_dalek::SignatureError),
}

#[cfg(not(feature = "std"))]
pub enum SignatureError {
    Dili(DiliSignError),
    Ed(ed25519_dalek::SignatureError),
}

#[cfg(feature = "std")]
#[derive(Debug, Error)]
pub enum DiliSignError {
    #[error("dilithium public key is invalid")]
    Input,
    #[error("yep")]
    Verify,
}

#[cfg(not(feature = "std"))]
pub enum DiliSignError {
    Input,
    Verify,
}

impl From<pqc_dilithium::SignError> for DiliSignError {
    fn from(e: pqc_dilithium::SignError) -> Self {
        match e {
            pqc_dilithium::SignError::Input => Self::Input,
            pqc_dilithium::SignError::Verify => Self::Verify,
        }
    }
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
