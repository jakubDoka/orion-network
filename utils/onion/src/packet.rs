use std::usize;

use aes_gcm::{
    aead::{generic_array::GenericArray, OsRng},
    aes::cipher::Unsigned,
    AeadCore, AeadInPlace, Aes256Gcm, KeyInit, KeySizeUser,
};
use libp2p_identity::PeerId;
use x25519_dalek::{PublicKey, StaticSecret};

pub const OK: u8 = 0;
pub const MISSING_PEER: u8 = 1;
pub const OCCUPIED_PEER: u8 = 2;
pub const ASOC_DATA: &[u8] = b"onion deez nuts";
pub const PATH_LEN: usize = 2;
pub const MAX_INIT_PACKET_SIZE: usize = PATH_LEN
    * (std::mem::size_of::<PeerId>()
        + 1
        + <Aes256Gcm as AeadCore>::TagSize::USIZE
        + <Aes256Gcm as AeadCore>::NonceSize::USIZE)
    + <Aes256Gcm as KeySizeUser>::KeySize::USIZE;
pub const CONFIRM_PACKET_SIZE: usize =
    <Aes256Gcm as AeadCore>::TagSize::USIZE + <Aes256Gcm as AeadCore>::NonceSize::USIZE;

pub fn wrap_with_key(key: &aes_gcm::Key<aes_gcm::Aes256Gcm>, skip: usize, buffer: &mut Vec<u8>) {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let cipher = Aes256Gcm::new(key);

    let tag = cipher
        .encrypt_in_place_detached(&nonce, ASOC_DATA, &mut buffer[skip..])
        .expect("we are certainly not that big");

    buffer.extend_from_slice(&tag);
    buffer.extend_from_slice(&nonce);
}

pub fn wrap(persistent: &StaticSecret, sender: &PublicKey, buffer: &mut Vec<u8>) {
    let key = persistent.diffie_hellman(&sender);
    let key = GenericArray::from(*key.as_bytes());
    wrap_with_key(&key, 0, buffer);
}

pub fn new_initial(
    path: [(PublicKey, PeerId); PATH_LEN],
    ephemeral: &StaticSecret,
    buffer: &mut Vec<u8>,
) {
    for (pk, id) in path {
        let id_bytes = id.to_bytes();
        buffer.extend_from_slice(&id_bytes);
        buffer.extend_from_slice(&[id_bytes.len() as u8]);

        wrap(ephemeral, &pk, buffer);
    }

    buffer.extend_from_slice(PublicKey::from(ephemeral).as_bytes());
}

pub fn peel_wih_key(
    key: &aes_gcm::Key<aes_gcm::Aes256Gcm>,
    mut buffer: &mut [u8],
) -> Option<usize> {
    let tag_size = <Aes256Gcm as AeadCore>::TagSize::to_usize();
    let nonce_size = <Aes256Gcm as AeadCore>::NonceSize::to_usize();

    if buffer.len() < tag_size + nonce_size {
        return None;
    }

    let mut tail;

    (buffer, tail) = buffer.split_at_mut(buffer.len() - nonce_size);
    let nonce = *GenericArray::from_slice(tail);
    (buffer, tail) = buffer.split_at_mut(buffer.len() - tag_size);
    let tag = *GenericArray::from_slice(tail);

    let cipher = Aes256Gcm::new(&key);

    cipher
        .decrypt_in_place_detached(&nonce, ASOC_DATA, buffer, &tag)
        .ok()?;

    Some(buffer.len())
}

pub fn peel(persistent: &StaticSecret, sender: &PublicKey, buffer: &mut [u8]) -> Option<usize> {
    let key = persistent.diffie_hellman(&sender);
    let key = GenericArray::from(*key.as_bytes());

    peel_wih_key(&key, buffer)
}

pub fn peel_initial(
    persistent: &StaticSecret,
    original_buffer: &mut [u8],
) -> Option<(Option<PeerId>, PublicKey, usize)> {
    let key_size = <Aes256Gcm as KeySizeUser>::KeySize::USIZE;

    if original_buffer.len() < key_size {
        return None;
    }

    let (buffer, tail) = original_buffer.split_at_mut(original_buffer.len() - key_size);
    let sender: [_; 32] = tail.try_into().expect("just checked that");
    let sender = PublicKey::from(sender);

    if buffer.len() == 0 {
        return Some((None, sender, 0));
    }

    let packet_len = peel(persistent, &sender, buffer)?;

    let buffer = &mut buffer[..packet_len];
    let (len, buffer) = buffer.split_last_mut()?;
    let (buffer, tail) = buffer.split_at_mut(buffer.len() - *len as usize);
    let id = PeerId::from_bytes(tail).ok()?;

    let len = buffer.len();
    original_buffer[len..len + key_size].copy_from_slice(sender.as_bytes());
    Some((Some(id), sender, len + key_size))
}
