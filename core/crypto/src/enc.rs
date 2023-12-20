use crate::{FixedAesPayload, SharedSecret, TransmutationCircle, ASOC_DATA, SHARED_SECRET_SIZE};
#[cfg(feature = "getrandom")]
use aes_gcm::aead::OsRng;
#[cfg(feature = "std")]
use thiserror::Error;

impl_transmute! {
    KeyPair,
    PublicKey,
    Ciphertext,
    ChoosenPayload,
    ChoosenCiphertext,
}

type EncriptedKey = FixedAesPayload<{ SHARED_SECRET_SIZE }>;

pub struct Ciphertext {
    pl: FixedAesPayload<{ kyber::CIPHERTEXTBYTES }>,
    x: x25519_dalek::PublicKey,
}

pub struct ChoosenCiphertext {
    pl: FixedAesPayload<{ core::mem::size_of::<ChoosenPayload>() }>,
    x: x25519_dalek::PublicKey,
}

struct ChoosenPayload {
    key: EncriptedKey,
    kyb: [u8; kyber::CIPHERTEXTBYTES],
}

#[derive(Clone)]
pub struct KeyPair {
    pub kyb: kyber::Keypair,
    pub x: x25519_dalek::StaticSecret,
}

impl PartialEq for KeyPair {
    fn eq(&self, other: &Self) -> bool {
        self.kyb.publickey() == other.kyb.publickey() && self.x.to_bytes() == other.x.to_bytes()
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
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0; kyber::KEY_SEEDBYTES];
        OsRng.fill_bytes(&mut seed);
        let kyb = kyber::Keypair::new(&seed);
        let x = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        Self { kyb, x }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            kyb: self.kyb.publickey(),
            x: x25519_dalek::PublicKey::from(&self.x),
        }
    }

    #[cfg(feature = "getrandom")]
    pub fn encapsulate(&self, public_key: &PublicKey) -> (Ciphertext, SharedSecret) {
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0; kyber::ENC_SEEDBYTES];
        OsRng.fill_bytes(&mut seed);
        let (data, secret) = public_key.kyb.enc(&seed);
        let x_secret = self.x.diffie_hellman(&public_key.x);
        (
            Ciphertext {
                pl: FixedAesPayload::new(data, x_secret.to_bytes(), ASOC_DATA),
                x: x25519_dalek::PublicKey::from(&self.x),
            },
            secret,
        )
    }

    #[cfg(feature = "getrandom")]
    pub fn encapsulate_choosen(
        &self,
        public_key: &PublicKey,
        secret: SharedSecret,
    ) -> ChoosenCiphertext {
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0; kyber::ENC_SEEDBYTES];
        OsRng.fill_bytes(&mut seed);
        let (kyb, ksecret) = public_key.kyb.enc(&seed);
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let key = EncriptedKey::new(secret, ksecret, ASOC_DATA);
        let data = ChoosenPayload { key, kyb };
        ChoosenCiphertext {
            pl: FixedAesPayload::new(data.into_bytes(), x_secret.to_bytes(), ASOC_DATA),
            x: x25519_dalek::PublicKey::from(&self.x),
        }
    }

    pub fn decapsulate(&self, ciphertext: Ciphertext) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.x.diffie_hellman(&ciphertext.x);
        let data = ciphertext
            .pl
            .decrypt(x_secret.to_bytes(), ASOC_DATA)
            .map_err(DecapsulationError::Aes)?;
        self.kyb.dec(&data).ok_or(DecapsulationError::Kyber)
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
        let secret = self
            .kyb
            .dec(&payload.kyb)
            .ok_or(DecapsulationError::Kyber)?;
        payload
            .key
            .decrypt(secret, ASOC_DATA)
            .map_err(DecapsulationError::Aes)
    }
}

#[cfg(feature = "std")]
#[derive(Debug, Error)]
pub enum DecapsulationError {
    #[error("kyber decapsulation failed")]
    Kyber,
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
    kyb: kyber::PublicKey,
    x: x25519_dalek::PublicKey,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_enc_dec() {
        use super::*;
        let alice = KeyPair::new();
        let bob = KeyPair::new();
        let (ciphertext, secret) = alice.encapsulate(&bob.public_key());
        let dec = bob.decapsulate(ciphertext).unwrap();
        assert_eq!(secret, dec);
    }

    #[test]
    fn test_enc_dec_choosen() {
        use super::*;
        let alice = KeyPair::new();
        let bob = KeyPair::new();
        let secret = [42u8; SHARED_SECRET_SIZE];
        let ciphertext = alice.encapsulate_choosen(&bob.public_key(), secret);
        let dec = bob.decapsulate_choosen(ciphertext).unwrap();
        assert_eq!(secret, dec);
    }
}
