#![cfg_attr(not(feature = "std"), no_std)]
#![feature(iter_next_chunk)]
#![feature(iter_advance_by)]
#![feature(ip_in_core)]
#![feature(macro_metavar_expr)]

use core::str::FromStr;

use component_utils::arrayvec::ArrayString;

pub mod contracts;

pub const USER_NAME_CAP: usize = 32;
pub type RawUserName = [u8; USER_NAME_CAP];
pub type UserName = ArrayString<USER_NAME_CAP>;

pub fn username_to_raw(u: UserName) -> RawUserName {
    let mut arr = [0; USER_NAME_CAP];
    arr[..u.len()].copy_from_slice(u.as_bytes());
    arr
}

pub fn username_from_raw(name: RawUserName) -> Option<UserName> {
    let len = name.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let name = &name[..len];
    UserName::from_str(core::str::from_utf8(name).ok()?).ok()
}
