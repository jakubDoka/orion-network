#![feature(array_chunks)]
#[macro_export]
macro_rules! gen_config {
    (
        $($(#[$comment:meta])* $required_field:ident: $required_ty:ty,)*
        ;;
        $($(#[$comment2:meta])* $field:ident: $ty:ty = $default:expr,)*
    ) => {
        pub struct Config {
            $(
                $(#[$comment])*
                pub $required_field: $required_ty,
            )*
            $(
                $field: $ty,
            )*
        }

        impl Config {
            pub fn new($($required_field: $required_ty),*) -> Self {
                Self {
                    $($required_field,)*
                    $($field: $default,)*
                }
            }

            $(
                $(#[$comment2])*
                #[doc = concat!("Defaults to ", stringify!($default))]
                pub fn $field(mut self, $field: $ty) -> Self {
                    self.$field = $field;
                    self
                }
            )*
        }
    };
}

pub mod codec;
pub mod handler;
#[cfg(feature = "kad")]
pub mod kad;
pub mod stream;

pub use {arrayvec, codec::*, futures, handler::*, stream::*};

#[cfg(feature = "kad")]
pub use kad::*;

#[cfg(feature = "kad")]
pub use libp2p_kad;

#[cfg(feature = "kad")]
pub use libp2p_identity;

#[derive(Debug)]
pub struct LinearMap<K, V> {
    values: Vec<(K, V)>,
}

impl<K: Eq, V> LinearMap<K, V> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some((_, current)) = self.values.iter_mut().find(|(k, _)| k == &key) {
            return Some(std::mem::replace(current, value));
        }
        self.values.push((key, value));
        None
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(index) = self.values.iter().position(|(k, _)| k == key) {
            return Some(self.values.swap_remove(index).1);
        }
        None
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.values.iter().map(|(k, v)| (k, v))
    }
}

impl<K, V> Default for LinearMap<K, V> {
    fn default() -> Self {
        Self { values: Vec::new() }
    }
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= *byte as u64;
    }
    hash
}

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: &[u8]) -> Self {
        Self(fnv_hash(seed))
    }

    pub fn next_u64(&mut self) -> u64 {
        let Self(seed) = self;
        *seed = fnv_hash(&seed.to_le_bytes());
        *seed
    }
}

pub fn drain_filter<'a, T>(
    v: &'a mut Vec<T>,
    pred: impl FnMut(&mut T) -> bool + 'a,
) -> impl Iterator<Item = T> + 'a {
    use std::ptr;
    struct Iter<'a, F, T>
    where
        F: FnMut(&mut T) -> bool,
    {
        v: &'a mut Vec<T>,
        pred: F,
        write: *mut T,
        read: *mut T,
        end: *mut T,
    }

    impl<'a, F, T> Iterator for Iter<'a, F, T>
    where
        F: FnMut(&mut T) -> bool,
    {
        type Item = T;

        fn next(&mut self) -> Option<Self::Item> {
            unsafe {
                let check_point = self.read;
                loop {
                    if self.read == self.end {
                        let length = self.read.offset_from(check_point) as usize;
                        if check_point != self.write && length > 0 {
                            ptr::copy(check_point, self.write, length);
                        }
                        self.write = self.write.add(length);
                        return None;
                    }

                    let item = &mut *self.read;
                    self.read = self.read.add(1);

                    if !(self.pred)(item) {
                        let length = self.read.offset_from(check_point) as usize - 1;
                        if check_point != self.write && length > 0 {
                            ptr::copy(check_point, self.write, length);
                        }
                        self.write = self.write.add(length);
                        return Some(ptr::read(item));
                    }
                }
            }
        }
    }

    impl<'a, F, T> Drop for Iter<'a, F, T>
    where
        F: FnMut(&mut T) -> bool,
    {
        fn drop(&mut self) {
            self.for_each(drop);

            unsafe {
                let len = self.write.offset_from(self.v.as_mut_ptr()) as usize;
                self.v.set_len(len);
            }
        }
    }

    Iter {
        pred,
        write: v.as_mut_ptr(),
        read: v.as_mut_ptr(),
        end: unsafe { v.as_mut_ptr().add(v.len()) },
        v: {
            unsafe { v.set_len(0) }
            v
        },
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_drain_filter() {
        let mut v = vec![1, 2, 3, 4, 5, 6, 7, 8];

        let odd = drain_filter(&mut v, |x| *x % 2 == 0).collect::<Vec<_>>();

        assert_eq!(odd, vec![1, 3, 5, 7]);
        assert_eq!(v, vec![2, 4, 6, 8]);
    }
}
