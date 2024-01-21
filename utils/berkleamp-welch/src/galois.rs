include!(concat!(env!("OUT_DIR"), "/table.rs"));

/// Add two elements.
#[inline]
pub fn add(a: u8, b: u8) -> u8 {
    a ^ b
}

/// Multiply two elements.
#[inline]
pub fn mul(a: u8, b: u8) -> u8 {
    MUL_TABLE[a as usize][b as usize]
}

/// Divide one element by another. `b`, the divisor, may not be 0.
#[inline]
pub fn div(a: u8, b: u8) -> Option<u8> {
    if b == 0 {
        return None;
    }

    if a == 0 {
        return Some(0);
    }

    if a == 1 {
        return Some(INV_TABLE[b as usize]);
    }

    let log_a = LOG_TABLE[a as usize];
    let log_b = LOG_TABLE[b as usize];
    let mut log_result = log_a as isize - log_b as isize;
    if log_result < 0 {
        log_result += 255;
    }
    Some(EXP_TABLE[log_result as usize])
}

/// Compute a^n.
#[inline]
pub fn exp(a: u8, n: usize) -> u8 {
    if n == 0 {
        1
    } else if a == 0 {
        0
    } else {
        let log_a = LOG_TABLE[a as usize];
        let mut log_result = log_a as usize * n;
        while 255 <= log_result {
            log_result -= 255;
        }
        EXP_TABLE[log_result]
    }
}

#[allow(unused)]
const PURE_RUST_UNROLL: usize = 4;

#[cfg(not(all(
    feature = "simd-accel",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(target_env = "msvc"),
    not(any(target_os = "android", target_os = "ios"))
)))]
#[inline]
pub fn mul_slice_xor(c: u8, input: &[u8], out: &mut [u8]) {
    assert_eq!(input.len(), out.len());

    let mt = &MUL_TABLE[c as usize];
    let mut input_chunks = input.array_chunks::<PURE_RUST_UNROLL>();
    let mut out_chunks = out.array_chunks_mut::<PURE_RUST_UNROLL>();

    for (inp, out) in input_chunks.by_ref().zip(out_chunks.by_ref()) {
        for i in 0..PURE_RUST_UNROLL {
            out[i] ^= mt[inp[i] as usize];
        }
    }

    for (i, o) in input_chunks.remainder().iter().zip(out_chunks.into_remainder()) {
        *o ^= mt[*i as usize];
    }
}

#[cfg(not(all(
    feature = "simd-accel",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(target_env = "msvc"),
    not(any(target_os = "android", target_os = "ios"))
)))]
#[inline]
pub fn mul_slice(c: u8, input: &[u8], out: &mut [u8]) {
    assert_eq!(input.len(), out.len());

    let mt = &MUL_TABLE[c as usize];

    let mut input_chunks = input.array_chunks::<PURE_RUST_UNROLL>();
    let mut out_chunks = out.array_chunks_mut::<PURE_RUST_UNROLL>();

    for (inp, out) in input_chunks.by_ref().zip(out_chunks.by_ref()) {
        for i in 0..PURE_RUST_UNROLL {
            out[i] = mt[inp[i] as usize];
        }
    }

    for (i, o) in input_chunks.remainder().iter().zip(out_chunks.into_remainder()) {
        *o = mt[*i as usize];
    }
}

#[cfg(all(
    feature = "simd-accel",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(target_env = "msvc"),
    not(any(target_os = "android", target_os = "ios"))
))]
extern "C" {
    fn _reedsolomon_gal_mul(
        low: *const u8,
        high: *const u8,
        input: *const u8,
        out: *mut u8,
        len: usize,
    ) -> usize;

    fn reedsolomon_gal_mul_xor(
        low: *const u8,
        high: *const u8,
        input: *const u8,
        out: *mut u8,
        len: usize,
    ) -> usize;
}

#[cfg(all(
    feature = "simd-accel",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(target_env = "msvc"),
    not(any(target_os = "android", target_os = "ios"))
))]
#[inline]
pub fn _mul_slice(c: u8, input: &[u8], out: &mut [u8]) {
    assert_eq!(input.len(), out.len());

    let len = input.len();

    let low = MUL_TABLE_LOW[c as usize].as_ptr();
    let high = MUL_TABLE_HIGH[c as usize].as_ptr();

    let input_ptr: *const u8 = &input[0];
    let out_ptr: *mut u8 = &mut out[0];
    let size: usize = input.len();

    let bytes_done: usize = unsafe { _reedsolomon_gal_mul(low, high, input_ptr, out_ptr, size) };

    unsafe {
        let mt = &MUL_TABLE[c as usize];
        let range = bytes_done..len;
        for (i, o) in input.get_unchecked(range.clone()).iter().zip(out.get_unchecked_mut(range)) {
            *o = mt[*i as usize];
        }
    }
}

#[cfg(all(
    feature = "simd-accel",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(target_env = "msvc"),
    not(any(target_os = "android", target_os = "ios"))
))]
#[inline]
pub fn mul_slice_xor(c: u8, input: &[u8], out: &mut [u8]) {
    assert_eq!(input.len(), out.len());

    let low = MUL_TABLE_LOW[c as usize].as_ptr();
    let high = MUL_TABLE_HIGH[c as usize].as_ptr();

    let input_ptr = input.as_ptr();
    let out_ptr = out.as_mut_ptr();
    let size = input.len();

    let bytes_done: usize = unsafe { reedsolomon_gal_mul_xor(low, high, input_ptr, out_ptr, size) };

    if bytes_done == size {
        return;
    }

    let mt = &MUL_TABLE[c as usize];
    for i in bytes_done..size {
        out[i] ^= mt[input[i] as usize];
    }
}
