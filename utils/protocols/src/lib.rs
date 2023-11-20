#![cfg_attr(not(feature = "std"), no_std)]
#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]
#![feature(ip_in_core)]

use component_utils::arrayvec::ArrayString;

#[cfg(feature = "std")]
pub mod chat;
pub mod contracts;

pub const USER_NAME_CAP: usize = 32;
pub type UserName = ArrayString<USER_NAME_CAP>;
