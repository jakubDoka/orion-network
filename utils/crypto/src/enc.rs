use aes_gcm::aead::OsRng;
use pqc_kyber::KyberError;
use thiserror::Error;

use crate::{FixedAesPayload, SharedSecret, SHARED_SECRET_SIZE};

impl_transmute! {
    KeyPair, KEY_PAIR_SIZE, SerializedKeyPair;
    PublicKey, PUBLIC_KEY_SIZE, SerializedPublicKey;
    Ciphertext, CIPHERTEXT_SIZE, SerializedCiphertext;
    ChoosenPayload, CHOSEN_PAYLOAD_SIZE, SerializedChoosenPayload;
}

pub type EncapsulationError = KyberError;

pub const ASOC_DATA: &[u8] = concat!("pqc-orion-crypto/enc/", env!("CARGO_PKG_VERSION")).as_bytes();

pub type Ciphertext = FixedAesPayload<{ pqc_kyber::KYBER_CIPHERTEXTBYTES }>;
type EncriptedKey = FixedAesPayload<{ SHARED_SECRET_SIZE }>;
pub type ChoosenCiphertext = FixedAesPayload<{ std::mem::size_of::<ChoosenPayload>() }>;

struct ChoosenPayload {
    key: EncriptedKey,
    kyb: [u8; pqc_kyber::KYBER_CIPHERTEXTBYTES],
}

#[derive(Clone)]
pub struct KeyPair {
    kyb: pqc_kyber::Keypair,
    x: x25519_dalek::StaticSecret,
}

impl KeyPair {
    pub fn new() -> Self {
        let kyb = pqc_kyber::Keypair::generate(&mut OsRng).expect("might as well");
        let x = x25519_dalek::StaticSecret::random_from_rng(&mut OsRng);
        Self { kyb, x }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            kyb: self.kyb.public,
            x: x25519_dalek::PublicKey::from(&self.x),
        }
    }

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

    pub fn encapsulate_choosen(
        &self,
        public_key: &PublicKey,
        secret: SharedSecret,
    ) -> Result<ChoosenCiphertext, EncapsulationError> {
        let (kyb, ksecret) = pqc_kyber::encapsulate(&public_key.kyb, &mut OsRng)?;
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let key = EncriptedKey::new(secret, ksecret, ASOC_DATA);
        let data = ChoosenPayload { key, kyb };
        Ok(ChoosenCiphertext::new(
            data.into(),
            x_secret.to_bytes(),
            ASOC_DATA,
        ))
    }

    pub fn decapsulate(
        &self,
        ciphertext: Ciphertext,
        public_key: &PublicKey,
    ) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let data = ciphertext.decrypt(x_secret.to_bytes(), ASOC_DATA)?;
        pqc_kyber::decapsulate(data.as_ref(), &self.kyb.secret).map_err(Into::into)
    }

    pub fn decapsulate_choosen(
        &self,
        ciphertext: ChoosenCiphertext,
        public_key: &PublicKey,
    ) -> Result<SharedSecret, DecapsulationError> {
        let x_secret = self.x.diffie_hellman(&public_key.x);
        let data = ciphertext.decrypt(x_secret.to_bytes(), ASOC_DATA)?;
        let payload: ChoosenPayload = data.into();
        let secret = pqc_kyber::decapsulate(&payload.kyb, &self.kyb.secret)?;
        let secret = payload.key.decrypt(secret, ASOC_DATA)?;
        Ok(secret)
    }
}

#[derive(Debug, Error)]
pub enum DecapsulationError {
    #[error("kyber decapsulation failed: {0}")]
    Kyber(#[from] KyberError),
    #[error("aes decapsulation failed: {0}")]
    Aes(#[from] aes_gcm::Error),
}

pub struct PublicKey {
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
        let dec = bob
            .decapsulate_choosen(ciphertext, &alice.public_key())
            .unwrap();
        assert_eq!(secret, dec);
    }
}
