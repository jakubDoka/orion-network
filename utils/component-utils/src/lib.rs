#![cfg_attr(not(feature = "std"), no_std)]
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

#[cfg(feature = "std")]
pub mod codec;
#[cfg(feature = "std")]
pub mod kad;
#[cfg(feature = "std")]
pub mod stream;

pub use arrayvec;

#[cfg(feature = "std")]
pub use {codec::*, futures, kad::*, libp2p, stream::*};

#[cfg(feature = "std")]
pub fn drain_filter<'a, T>(
    v: &'a mut Vec<T>,
    pred: impl FnMut(&mut T) -> bool + 'a,
) -> impl Iterator<Item = T> + 'a {
    use core::ptr;
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

pub struct DropFn<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> DropFn<F> {
    pub fn new(f: F) -> Self {
        Self(Some(f))
    }
}

impl<F: FnOnce()> Drop for DropFn<F> {
    fn drop(&mut self) {
        self.0.take().unwrap()()
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
