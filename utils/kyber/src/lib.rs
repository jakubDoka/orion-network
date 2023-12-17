//#![no_std]
#![feature(array_chunks)]
#![feature(iter_array_chunks)]

mod barrett;
mod cbd;
mod indcpa;
mod kem;
mod mondgomery;
mod ntt;
mod params;
mod poly;
mod polyvec;
mod symetric;
mod verify;

use self::params::INDCPA_SECRETKEYBYTES;
pub use params::{CIPHERTEXTBYTES, PUBLICKEYBYTES, SECRETKEYBYTES, SYMBYTES};

pub const SEEDBYTES: usize = SYMBYTES * 2;

pub struct Keypair([u8; SECRETKEYBYTES]);

impl Keypair {
    pub fn new(seed: &[u8; SEEDBYTES]) -> Self {
        Self(kem::keypair_derand(seed))
    }

    pub fn publickey(&self) -> PublicKey {
        PublicKey(
            self.0[INDCPA_SECRETKEYBYTES..INDCPA_SECRETKEYBYTES + PUBLICKEYBYTES]
                .try_into()
                .unwrap(),
        )
    }

    pub fn dec(&self, ct: &[u8; CIPHERTEXTBYTES]) -> Option<[u8; SYMBYTES]> {
        kem::dec(*ct, self.0)
    }

    pub fn to_bytes(&self) -> [u8; SECRETKEYBYTES] {
        self.0
    }

    pub fn from_bytes(bytes: &[u8; SECRETKEYBYTES]) -> Self {
        Self(*bytes)
    }
}

pub struct PublicKey([u8; PUBLICKEYBYTES]);

impl PublicKey {
    pub fn enc(&self, seed: &[u8; SYMBYTES]) -> ([u8; CIPHERTEXTBYTES], [u8; SYMBYTES]) {
        kem::enc_derand(self.0, seed)
    }

    pub fn to_bytes(&self) -> [u8; PUBLICKEYBYTES] {
        self.0
    }

    pub fn from_bytes(bytes: &[u8; PUBLICKEYBYTES]) -> Self {
        Self(*bytes)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_kem() {
        let keypair = super::Keypair::new(&[6u8; super::SEEDBYTES]);
        let (ct, ss) = keypair.publickey().enc(&[7u8; super::SYMBYTES]);
        let ss2 = keypair.dec(&ct).unwrap();
        assert_eq!(ss, ss2);
    }
}
