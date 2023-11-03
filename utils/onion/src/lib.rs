#![feature(iter_map_windows)]
#![feature(array_windows)]
#![feature(array_chunks)]
#![feature(impl_trait_in_assoc_type)]

mod behaviour;
mod handler;
#[cfg(feature = "kad")]
mod kad_search;
mod packet;

#[cfg(test)]
mod tests;

#[cfg(feature = "kad")]
pub use libp2p_kad;
pub use {behaviour::*, handler::*, packet::KeyPair, packet::PublicKey, packet::SharedSecret};
