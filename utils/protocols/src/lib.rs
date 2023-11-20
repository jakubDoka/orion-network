#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]

use component_utils::arrayvec::ArrayString;

#[cfg(feature = "libp2p")]
pub mod chat;
pub mod contracts;

pub const USER_NAME_CAP: usize = 32;
pub type UserName = ArrayString<USER_NAME_CAP>;
