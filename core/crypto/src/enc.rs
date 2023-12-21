use crate::{FixedAesPayload, SharedSecret, TransmutationCircle, ASOC_DATA, SHARED_SECRET_SIZE};
#[cfg(feature = "getrandom")]
use aes_gcm::aead::OsRng;
#[cfg(feature = "std")]
use thiserror::Error;

impl_transmute! {
    Keypair,
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
pub struct Keypair {
    post: kyber::Keypair,
    #[allow(dead_code)]
    pre: x25519_dalek::StaticSecret,
}

impl PartialEq for Keypair {
    fn eq(&self, other: &Self) -> bool {
        self.post.publickey() == other.post.publickey()
            && self.pre.to_bytes() == other.pre.to_bytes()
    }
}

impl Eq for Keypair {}

#[cfg(feature = "getrandom")]
impl Default for Keypair {
    fn default() -> Self {
        Self::new()
    }
}

impl Keypair {
    #[cfg(feature = "getrandom")]
    pub fn new() -> Self {
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0; kyber::KEY_SEEDBYTES];
        OsRng.fill_bytes(&mut seed);
        let post = kyber::Keypair::new(&seed);
        let pre = x25519_dalek::StaticSecret::random_from_rng(OsRng);
        Self { post, pre }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            post: self.post.publickey(),
            pre: x25519_dalek::PublicKey::from(&self.pre),
        }
    }

    #[cfg(feature = "getrandom")]
    pub fn encapsulate(&self, public_key: &PublicKey) -> (Ciphertext, SharedSecret) {
        use aes_gcm::aead::rand_core::RngCore;

        let mut seed = [0; kyber::ENC_SEEDBYTES];
        OsRng.fill_bytes(&mut seed);
        let (data, secret) = public_key.post.enc(&seed);
        let x_secret = self.pre.diffie_hellman(&public_key.pre);
        (
            Ciphertext {
                pl: FixedAesPayload::new(data, x_secret.to_bytes(), ASOC_DATA),
                x: x25519_dalek::PublicKey::from(&self.pre),
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
        let (kyb, ksecret) = public_key.post.enc(&seed);
        let x_secret = self.pre.diffie_hellman(&public_key.pre);
        let key = EncriptedKey::new(secret, ksecret, ASOC_DATA);
        let data = ChoosenPayload { key, kyb };
        ChoosenCiphertext {
            pl: FixedAesPayload::new(data.into_bytes(), x_secret.to_bytes(), ASOC_DATA),
            x: x25519_dalek::PublicKey::from(&self.pre),
        }
    }

    pub fn decapsulate(&self, ciphertext: Ciphertext) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.pre.diffie_hellman(&ciphertext.x);
        let data = ciphertext
            .pl
            .decrypt(x_secret.to_bytes(), ASOC_DATA)
            .map_err(DecapsulationError::Aes)?;
        self.post.dec(&data).ok_or(DecapsulationError::Kyber)
    }

    pub fn decapsulate_choosen(
        &self,
        ciphertext: ChoosenCiphertext,
    ) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.pre.diffie_hellman(&ciphertext.x);
        let data = ciphertext
            .pl
            .decrypt(x_secret.to_bytes(), ASOC_DATA)
            .map_err(DecapsulationError::Aes)?;
        let payload = ChoosenPayload::from_bytes(data);
        let secret = self
            .post
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
    Kyber,
    Aes(aes_gcm::Error),
}

#[derive(Clone, Copy, Debug)]
pub struct PublicKey {
    #[allow(dead_code)]
    post: kyber::PublicKey,
    #[allow(dead_code)]
    pre: x25519_dalek::PublicKey,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_enc_dec() {
        use super::*;
        let alice = Keypair::new();
        let bob = Keypair::new();
        let (ciphertext, secret) = alice.encapsulate(&bob.public_key());
        let dec = bob.decapsulate(ciphertext).unwrap();
        assert_eq!(secret, dec);
    }

    #[test]
    fn test_enc_dec_choosen() {
        use super::*;
        let alice = Keypair::new();
        let bob = Keypair::new();
        let secret = [42u8; SHARED_SECRET_SIZE];
        let ciphertext = alice.encapsulate_choosen(&bob.public_key(), secret);
        let dec = bob.decapsulate_choosen(ciphertext).unwrap();
        assert_eq!(secret, dec);
    }
}
