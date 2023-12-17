use crate::params::*;

pub fn reduce(a: i16) -> i16 {
    const V: i16 = (((1 << 26) + Q / 2) / Q) as i16;
    let t = (V as i32 * a as i32 + (1 << 25)) >> 26;
    a - (t * Q as i32) as i16
}
