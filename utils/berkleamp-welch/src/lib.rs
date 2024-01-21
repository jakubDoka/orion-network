#![feature(array_chunks)]
#![feature(slice_take)]
#![feature(array_windows)]
#![allow(clippy::many_single_char_names)]

mod fec;
mod galois;
mod math;

pub use fec::*;
