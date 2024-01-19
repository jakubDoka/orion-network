mod math {
    use std::{iter, mem};

    pub fn invert_matrix(mx: &mut [u8], k: usize) -> Option<()> {
        assert_eq!(mx.len(), k * k);

        let mut unused_rows = vec![true; k];
        let mut swaps = vec![];
        let mut id_row = vec![0; k];
        for i in 0..k {
            let pivot = unused_rows
                .iter_mut()
                .zip(mx.chunks_exact(k))
                .position(|(unused, row)| row[i] != 0 && mem::take(unused))?;

            if pivot != i {
                let [a, b] = [pivot.min(i), pivot.max(i)];
                let (left, right) = mx.split_at_mut(b * k);
                left[a * i..a * i + k].swap_with_slice(&mut right[..k]);
                swaps.push((a, b));
            }

            let (above_pivot, rest) = mx.split_at_mut(i * k);
            let (pivot_row, below_pivot) = rest.split_at_mut(k);

            let c = pivot_row[i];

            if c == 0 {
                return None;
            }

            if c != 1 {
                let c = galois::div(1, c);
                pivot_row[i] = 1;
                pivot_row.iter_mut().for_each(|x| *x = galois::mul(c, *x));
            }

            id_row[i] = 1;
            if id_row != pivot_row {
                let iter = iter::empty()
                    .chain(above_pivot.chunks_exact_mut(k))
                    .chain(below_pivot.chunks_exact_mut(k));

                for row in iter {
                    let c = std::mem::take(&mut row[i]);
                    galois::mul_slice_xor(c, pivot_row, row);
                }
            }
            id_row[i] = 0;
        }

        for (a, b) in swaps.into_iter().rev() {
            let (left, right) = mx.split_at_mut(b * k);
            left[a * k..a * k + k].swap_with_slice(&mut right[..k]);
        }

        Some(())
    }
}

#[cfg(test)]
mod test {
    use {super::*, std::usize};

    #[test]
    fn invert_matrix() {
        let cases = [
            ([1, 0, 0, 0, 1, 0, 0, 0, 1], 3, [1, 0, 0, 0, 1, 0, 0, 0, 1]),
            ([1, 2, 0, 3, 1, 1, 0, 9, 1], 3, [1, 0, 0, 0, 1, 0, 0, 0, 1]),
        ];

        for (i, (m, k, result)) in cases.into_iter().enumerate() {
            let mut mcpy = m;
            math::invert_matrix(&mut mcpy, k).unwrap();
            println!("case {}", i);
            for (a, b) in mcpy.iter().zip(result.iter()) {
                println!("{:?} {:?}", a, b);
            }
            assert_eq!(mcpy, result, "case {}", i);
        }
    }

    #[test]
    #[ignore]
    fn invert_matric_bench() {
        const DIM: usize = 32;
        let mut m = [0; DIM * DIM];
        loop {
            getrandom::getrandom(&mut m[..]).unwrap();
            if math::invert_matrix(&mut { m }, DIM).is_some() {
                break;
            }
        }
        let n_runs = 1024 * 1024;
        let now = std::time::Instant::now();
        for _ in 0..n_runs {
            math::invert_matrix(&mut { m }, DIM).unwrap();
        }
        println!("{:?}", now.elapsed().checked_div(n_runs).unwrap());
    }
}
