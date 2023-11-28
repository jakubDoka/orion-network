#[cfg(feature = "getrandom")]
use aes_gcm::aead::OsRng;
use pqc_kyber::KyberError;
#[cfg(feature = "std")]
use thiserror::Error;

use crate::{FixedAesPayload, SharedSecret, TransmutationCircle, SHARED_SECRET_SIZE};

impl_transmute! {
    KeyPair,
    PublicKey,
    Ciphertext,
    ChoosenPayload,
    ChoosenCiphertext,
}

pub type EncapsulationError = KyberError;

pub const ASOC_DATA: &[u8] = concat!("pqc-orion-crypto/enc/", env!("CARGO_PKG_VERSION")).as_bytes();

pub type Ciphertext = FixedAesPayload<{ pqc_kyber::KYBER_CIPHERTEXTBYTES }>;
type EncriptedKey = FixedAesPayload<{ SHARED_SECRET_SIZE }>;

pub struct ChoosenCiphertext {
    pl: FixedAesPayload<{ core::mem::size_of::<ChoosenPayload>() }>,
    x: x25519_dalek::PublicKey,
}

struct ChoosenPayload {
    key: EncriptedKey,
    kyb: [u8; pqc_kyber::KYBER_CIPHERTEXTBYTES],
}

#[derive(Clone)]
pub struct KeyPair {
    pub kyb: pqc_kyber::Keypair,
    pub x: x25519_dalek::StaticSecret,
}

impl PartialEq for KeyPair {
    fn eq(&self, other: &Self) -> bool {
        self.kyb.public == other.kyb.public && self.x.to_bytes() == other.x.to_bytes()
    }
}

impl Eq for KeyPair {}

#[cfg(feature = "getrandom")]
impl Default for KeyPair {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyPair {
    #[cfg(feature = "getrandom")]
    pub fn new() -> Self {
        let kyb = pqc_kyber::Keypair::generate(&mut OsRng).expect("might as well");
        let x = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        Self { kyb, x }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            kyb: self.kyb.public,
            x: x25519_dalek::PublicKey::from(&self.x),
        }
    }

    #[cfg(feature = "getrandom")]
    pub fn encapsulate(
        &self,
        public_key: &PublicKey,
    ) -> Result<(Ciphertext, SharedSecret), EncapsulationError> {
        let (data, secret) = pqc_kyber::encapsulate(&public_key.kyb, &mut OsRng)?;
        let x_secret = self.x.diffie_hellman(&public_key.x);
        Ok((
            Ciphertext::new(data, x_secret.to_bytes(), ASOC_DATA),
            secret,
        ))
    }

    #[cfg(feature = "getrandom")]
    pub fn encapsulate_choosen(
        &self,
        public_key: &PublicKey,
        secret: SharedSecret,
    ) -> Result<ChoosenCiphertext, EncapsulationError> {
        let (kyb, ksecret) = pqc_kyber::encapsulate(&public_key.kyb, &mut OsRng)?;
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let key = EncriptedKey::new(secret, ksecret, ASOC_DATA);
        let data = ChoosenPayload { key, kyb };
        Ok(ChoosenCiphertext {
            pl: FixedAesPayload::new(data.into_bytes(), x_secret.to_bytes(), ASOC_DATA),
            x: x25519_dalek::PublicKey::from(&self.x),
        })
    }

    pub fn decapsulate(
        &self,
        ciphertext: Ciphertext,
        public_key: &PublicKey,
    ) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let data = ciphertext
            .decrypt(x_secret.to_bytes(), ASOC_DATA)
            .map_err(DecapsulationError::Aes)?;
        pqc_kyber::decapsulate(data.as_ref(), &self.kyb.secret).map_err(DecapsulationError::Kyber)
    }

    pub fn decapsulate_choosen(
        &self,
        ciphertext: ChoosenCiphertext,
    ) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.x.diffie_hellman(&ciphertext.x);
        let data = ciphertext
            .pl
            .decrypt(x_secret.to_bytes(), ASOC_DATA)
            .map_err(DecapsulationError::Aes)?;
        let payload = ChoosenPayload::from_bytes(data);
        let secret = pqc_kyber::decapsulate(&payload.kyb, &self.kyb.secret)
            .map_err(DecapsulationError::Kyber)?;
        payload
            .key
            .decrypt(secret, ASOC_DATA)
            .map_err(DecapsulationError::Aes)
    }
}

#[cfg(feature = "std")]
#[derive(Debug, Error)]
pub enum DecapsulationError {
    #[error("kyber decapsulation failed: {0}")]
    Kyber(KyberError),
    #[error("aes decapsulation failed: {0}")]
    Aes(aes_gcm::Error),
}

#[cfg(not(feature = "std"))]
pub enum DecapsulationError {
    Kyber(KyberError),
    Aes(aes_gcm::Error),
}

#[derive(Clone, Copy, Debug)]
pub struct PublicKey {
    #[allow(dead_code)]
    kyb: pqc_kyber::PublicKey,
    x: x25519_dalek::PublicKey,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_enc_dec() {
        use super::*;
        let alice = KeyPair::new();
        let bob = KeyPair::new();
        let (ciphertext, secret) = alice.encapsulate(&bob.public_key()).unwrap();
        let dec = bob.decapsulate(ciphertext, &alice.public_key()).unwrap();
        assert_eq!(secret, dec);
    }

    #[test]
    fn test_enc_dec_choosen() {
        use super::*;
        let alice = KeyPair::new();
        let bob = KeyPair::new();
        let secret = [42u8; SHARED_SECRET_SIZE];
        let ciphertext = alice
            .encapsulate_choosen(&bob.public_key(), secret)
            .unwrap();
        let dec = bob.decapsulate_choosen(ciphertext).unwrap();
        assert_eq!(secret, dec);
    }
}
