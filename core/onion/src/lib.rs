#![feature(iter_map_windows)]
#![feature(array_windows)]
#![feature(array_chunks)]
#![feature(impl_trait_in_assoc_type)]
#![feature(unwrap_infallible)]
#![feature(extract_if)]

mod behaviour;
mod handler;
#[cfg(feature = "libp2p")]
mod kad_search;
pub mod key_share;
mod packet;

#[cfg(test)]
mod tests;

pub use {
    behaviour::*,
    handler::*,
    packet::{KeyPair, PublicKey, SharedSecret},
};
