use std::{iter, mem, usize};

pub struct Fec {
    required: usize,
    total: usize,
    enc_matrix: Vec<u8>,
    vand_matrix: Vec<u8>,
}

impl Fec {
    /// # Panics
    /// if '0 < required < total <= 256' is not true
    pub fn new(required: usize, total: usize) -> Self {
        assert!(required > 0);
        assert!(total > required);
        assert!(required <= 256);
        assert!(total <= 256);

        let mut enc_matrix = vec![0; required * total];
        let mut inverted_vdm = vec![0; required * total];
        math::create_inverted_vdm(&mut inverted_vdm, required);

        for (i, e) in inverted_vdm.iter_mut().enumerate().skip(required * required) {
            *e = galois::EXP_TABLE[((i / required) * (i % required)) % 255];
        }

        for i in 0..required {
            enc_matrix[i * (required + 1)] = 1;
        }

        for row in (required * required..required * total).step_by(required) {
            for col in 0..required {
                let mut acc = 0;
                for i in 0..required {
                    acc ^= galois::mul(inverted_vdm[row + i], inverted_vdm[i * required + col]);
                }
                enc_matrix[row + col] = acc;
            }
        }

        let mut vand_matrix = vec![0; required * total];
        vand_matrix[0] = 1;
        let mut g = 1;
        for row in vand_matrix.chunks_exact_mut(total) {
            let mut a = 1;
            for col in row.iter_mut().skip(1) {
                *col = a;
                a = galois::mul(g, a);
            }
            g = galois::mul(2, g);
        }

        Self { required, total, enc_matrix, vand_matrix }
    }

    pub fn required(&self) -> usize {
        self.required
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn encode(&self, data: &[u8], parity: &mut [u8]) -> Result<(), EncodeError> {
        let block_size = data.len() / self.required;

        if block_size * self.required != data.len() {
            return Err(EncodeError::InvalidDataLength);
        }

        if block_size * (self.total - self.required) != parity.len() {
            return Err(EncodeError::InvalidParityLength);
        }

        parity.fill(0);

        for (i, par) in parity.chunks_exact_mut(block_size).enumerate() {
            for (j, dat) in data.chunks_exact(block_size).enumerate() {
                galois::mul_slice_xor(
                    self.enc_matrix[(i + self.required) * self.required + j],
                    dat,
                    par,
                );
            }
        }

        Ok(())
    }

    pub fn rebuild<'a, 'b>(
        &self,
        mut shares: &'a mut [Share<'b>],
        temp_buffer: &mut Vec<u8>,
    ) -> Result<&'a mut [Share<'b>], RebuildError> {
        shares.sort_unstable_by_key(|s| s.number);
        shares = shares.take_mut(..self.required).ok_or(RebuildError::NotEnoughShares)?;

        let block_size = shares[0].data.len();
        if shares.iter().any(|s| s.data.len() != block_size) {
            return Err(RebuildError::NotEqualLengths);
        }

        temp_buffer.clear();
        temp_buffer.resize(self.required * self.required + block_size, 0);
        let (m_dec, buf) = temp_buffer.split_at_mut(self.required * self.required);

        for i in 0..self.required {
            m_dec[i * (self.required + 1)] = 1;
        }

        let data_count = shares.iter().take_while(|s| s.number < self.required).count();
        let (data, parity) = shares.split_at_mut(data_count);
        let first_id = data.first().map(|s| s.number).unwrap_or(0);
        let last_id = data.last().map(|s| s.number + 1).unwrap_or(0);

        for (i, p) in data
            .array_windows()
            .flat_map(|[a, b]| a.number + 1..b.number)
            .chain(last_id..self.required)
            .chain(0..first_id)
            .zip(parity)
        {
            let src = p.number * self.required..p.number * self.required + self.required;
            let dst = i * self.required..i * self.required + self.required;
            m_dec[dst].copy_from_slice(&self.enc_matrix[src]);
            p.number = usize::MAX - p.number;
        }

        shares.sort_unstable_by_key(|s| {
            if s.number > self.required {
                usize::MAX - s.number
            } else {
                s.number
            }
        });

        math::invert_matrix(m_dec, self.required).ok_or(RebuildError::InvertMatrix)?;

        for (i, row) in m_dec.chunks_exact(self.required).enumerate() {
            if shares[i].number < self.required {
                continue;
            }

            buf.fill(0);
            for (&cof, share) in row.iter().zip(&*shares) {
                galois::mul_slice_xor(cof, share.data, buf);
            }

            shares[i].data.copy_from_slice(buf);
        }

        Ok(shares)
    }

    pub fn decode<'a>(
        &self,
        mut shares: &'a mut [Share<'a>],
        report_invalid: bool,
        temp_buffer: &mut Vec<u8>,
    ) -> Result<&'a mut [Share<'a>], DecodeError<'a>> {
        let allocs = Resources::new(temp_buffer, self.required, self.total, shares)
            .map_err(DecodeError::InitResources)?;
        shares = self.correct(shares, report_invalid, allocs).map_err(DecodeError::Correct)?;
        self.rebuild(shares, temp_buffer).map_err(DecodeError::Rebuild)
    }

    pub fn correct<'a>(
        &self,
        shares: &'a mut [Share<'a>],
        report_invalid: bool,
        mut allocs: Resources,
    ) -> Result<&'a mut [Share<'a>], CorrectError<'a>> {
        let (r, c) = self
            .syndrome_matrix(&mut *shares, &mut allocs)
            .map_err(CorrectError::SyndromeMatrix)?;
        let synd = mem::take(&mut allocs.parity);

        let buf = mem::take(&mut allocs.buf);
        let correction = mem::take(&mut allocs.corrections);
        for i in 0..r {
            buf.fill(0);

            for j in 0..c {
                galois::mul_slice_xor(synd[i * c + j], shares[j].data, buf);
            }

            for (j, _) in buf.iter().enumerate().filter(|(_, &b)| b != 0) {
                self.berklekamp_welch(&mut *shares, j, correction, &mut allocs)
                    .map_err(CorrectError::BerklekampWelch)?;

                for share in shares.iter_mut() {
                    if share.number < self.total {
                        // considered invalid
                        share.number = usize::MAX - share.number;
                    }
                    share.data[j] = correction[usize::MAX - share.number];
                }
            }
        }

        shares.sort_unstable_by_key(|s| s.number);

        let valid_count = shares.iter().take_while(|s| s.number < self.total).count();

        for share in &mut shares[valid_count..] {
            share.number = usize::MAX - share.number;
        }

        if report_invalid && valid_count != shares.len() {
            return Err(CorrectError::InvalidShares(&mut shares[valid_count..]));
        }

        Ok(shares)
    }

    pub fn berklekamp_welch(
        &self,
        shares: &mut [Share],
        j: usize,
        correction: &mut [u8],
        allocs: &mut Resources,
    ) -> Result<(), BerklekampWelchError> {
        let e = allocs.e;
        let q = allocs.q;

        let interp_base = 2;

        let eval_point = |num| {
            if num == 0 {
                0
            } else {
                galois::exp(interp_base, num - 1)
            }
        };

        let dim = q + e;

        let s = &mut *allocs.s;
        let f = &mut *allocs.f;
        for ((f, share), row) in f.iter_mut().zip(shares).zip(s.chunks_exact_mut(dim)) {
            let x_i = eval_point(share.number);
            let r_i = share.data[j];

            *f = galois::mul(galois::exp(x_i, e), r_i);

            for (i, x) in row[..q].iter_mut().enumerate() {
                *x = galois::exp(x_i, i)
            }
            for (k, x) in row[q..].iter_mut().enumerate() {
                *x = galois::mul(galois::exp(x_i, k), r_i)
            }
        }

        let a = &mut *allocs.a;
        a.fill(0);
        a.iter_mut().step_by(dim + 1).for_each(|x| *x = 1);

        math::invert_matrix_with(s, dim, a).ok_or(BerklekampWelchError::InvertMatrix)?;

        let u = &mut *allocs.u;
        for (ri, u) in a.chunks_exact(dim).zip(&mut *u) {
            *u = ri.iter().zip(&*f).map(|(&a, &b)| galois::mul(a, b)).fold(0, galois::add);
        }

        let q_poly = math::Poly::from_iter(u[..q].iter().copied());
        let e_poly = math::Poly::from_iter(u[q..].iter().copied().chain(iter::once(1)));

        let (p_poly, rem) = q_poly.div(e_poly).ok_or(BerklekampWelchError::DivPoly)?;

        if !rem.is_zero() {
            return Err(BerklekampWelchError::CannotRecover);
        }

        for (i, out) in correction.iter_mut().enumerate() {
            let pt = if i != 0 { galois::exp(interp_base, i - 1) } else { 0 };
            *out = p_poly.eval(pt);
        }

        Ok(())
    }

    fn syndrome_matrix(
        &self,
        shares: &mut [Share],
        resources: &mut Resources,
    ) -> Result<(usize, usize), SyndromeMatrixError> {
        let keepers = &mut *resources.presence_set;
        shares.iter_mut().for_each(|s| keepers[s.number] = true);

        let out = &mut *resources.syndrome;
        for row in 0..self.required {
            let mut skipped = 0;
            for col in 0..self.total {
                if !keepers[col] {
                    skipped += 1;
                    continue;
                }

                out[row * shares.len() + col - skipped] = self.vand_matrix[row * self.total + col];
            }
        }

        math::standardize_matrix(out, self.required, shares.len())
            .ok_or(SyndromeMatrixError::FailedToStandardize)?;

        Ok(math::parity_matrix(out, self.required, shares.len(), resources.parity))
    }
}

pub struct Resources<'a> {
    presence_set: &'a mut [bool],
    syndrome: &'a mut [u8],
    parity: &'a mut [u8],

    buf: &'a mut [u8],
    corrections: &'a mut [u8],

    e: usize,
    q: usize,
    s: &'a mut [u8],
    a: &'a mut [u8],
    f: &'a mut [u8],
    u: &'a mut [u8],
}

impl<'a> Resources<'a> {
    pub fn new(
        buffer: &'a mut Vec<u8>,
        required: usize,
        totoal: usize,
        shares: &mut [Share],
    ) -> Result<Self, ResourcesError> {
        shares.sort_unstable_by_key(|s| s.number);
        if shares.array_windows().any(|[a, b]| a.number == b.number) {
            return Err(ResourcesError::DuplicateShares);
        }

        if shares.len() < required {
            return Err(ResourcesError::NotEnoughShares);
        }

        let share_len = shares[0].data.len();
        if shares.iter().any(|s| s.data.len() != share_len) {
            return Err(ResourcesError::NotEqualLengths);
        }

        let presence_set_len = totoal;
        let syndrome_len = required * share_len;
        let parity_len = (share_len - required) * share_len;

        let buf_len = share_len;
        let corrections_len = totoal;

        let e = (shares.len() - required) / 2;
        let q = e + required;
        let dim = q + e;

        let s_len = dim * dim;
        let a_len = dim * dim;
        let f_len = dim;
        let u_len = dim;

        buffer.clear();
        buffer.resize(
            presence_set_len
                + syndrome_len
                + parity_len
                + buf_len
                + corrections_len
                + s_len
                + a_len
                + f_len
                + u_len,
            0,
        );

        let mut rest = &mut buffer[..];
        let mut take = |len| rest.take_mut(..len).expect("buffer too small, why?");

        Ok(Self {
            // SAFETY: the buffer is zeroed out
            presence_set: unsafe { std::mem::transmute(take(presence_set_len)) },
            syndrome: take(syndrome_len),
            parity: take(parity_len),
            buf: take(buf_len),
            corrections: take(corrections_len),
            e,
            q,
            s: take(s_len),
            a: take(a_len),
            f: take(f_len),
            u: take(u_len),
        })
    }
}

#[derive(Debug)]
pub enum ResourcesError {
    DuplicateShares,
    NotEnoughShares,
    NotEqualLengths,
}

#[derive(Debug)]
pub enum EncodeError {
    InvalidParityLength,
    InvalidDataLength,
}

#[derive(Debug)]
pub enum RebuildError {
    InvalidLength,
    NotEnoughShares,
    NotEqualLengths,
    InvertMatrix,
}

#[derive(Debug)]
pub enum DecodeError<'a> {
    InitResources(ResourcesError),
    Correct(CorrectError<'a>),
    Rebuild(RebuildError),
}

#[derive(Debug)]
pub enum CorrectError<'a> {
    NotEnoughShares,
    InvalidShares(&'a mut [Share<'a>]),
    SyndromeMatrix(SyndromeMatrixError),
    BerklekampWelch(BerklekampWelchError),
}

#[derive(Debug)]
pub enum SyndromeMatrixError {
    FailedToStandardize,
}

#[derive(Debug)]
pub enum BerklekampWelchError {
    NotEnoughShares,
    InvertMatrix,
    DivPoly,
    CannotRecover,
}

#[derive(Debug)]
pub struct Share<'a> {
    number: usize,
    data: &'a mut [u8],
}

mod math {
    use {
        arrayvec::ArrayVec,
        std::{iter, mem},
    };

    pub fn standardize_matrix(mx: &mut [u8], r: usize, c: usize) -> Option<()> {
        assert_eq!(mx.len(), r * c);

        for i in 0..r {
            let Some((p_row, p_val)) = (i..r).map(|j| (j, mx[j * c + i])).find(|&(_, x)| x != 0)
            else {
                continue;
            };

            if p_row != i {
                matrix_swap_rows(mx, c, i, p_row);
            }

            let inv = galois::div(1, p_val).expect("we checked");
            mx[i * c..i * c + c].iter_mut().for_each(|x| *x = galois::mul(inv, *x));

            for j in i + 1..r {
                matrix_add_mul_rows(mx, c, i, j, mx[j * c + i]);
            }
        }

        for i in (0..r).rev() {
            for j in (0..i).rev() {
                matrix_add_mul_rows(mx, c, i, j, mx[j * c + i]);
            }
        }

        Some(())
    }

    pub fn invert_matrix_with(mx: &mut [u8], k: usize, out: &mut [u8]) -> Option<()> {
        assert_eq!(mx.len(), k * k);
        assert_eq!(out.len(), k * k);

        for i in 0..k {
            let Some((p_row, p_val)) = (i..k).map(|j| (j, mx[j * k + i])).find(|&(_, x)| x != 0)
            else {
                continue;
            };

            if p_row != i {
                matrix_swap_rows(mx, k, i, p_row);
                matrix_swap_rows(out, k, i, p_row);
            }

            let inv = galois::div(1, p_val).expect("we checked");
            mx[i * k..i * k + k].iter_mut().for_each(|x| *x = galois::mul(inv, *x));
            out[i * k..i * k + k].iter_mut().for_each(|x| *x = galois::mul(inv, *x));

            debug_assert!(mx[i * k + i] == 1, "{:?}", mx);

            for j in i + 1..k {
                let fac = mx[j * k + i];
                matrix_add_mul_rows(mx, k, i, j, fac);
                matrix_add_mul_rows(out, k, i, j, fac);
            }
        }

        for i in (0..k).rev() {
            for j in (0..i).rev() {
                let fac = mx[j * k + i];
                matrix_add_mul_rows(mx, k, i, j, fac);
                matrix_add_mul_rows(out, k, i, j, fac);
            }
        }

        debug_assert!((0..k).all(|i| mx[i * k + i] == 1), "{:?}", mx);

        Some(())
    }

    pub fn parity_matrix(mx: &mut [u8], r: usize, c: usize, out: &mut [u8]) -> (usize, usize) {
        assert_eq!(mx.len(), r * c);
        assert_eq!(out.len(), (c - r) * c);

        for i in 0..c - r {
            out[i * c + i + r] = 1;
        }

        for i in 0..c - r {
            for j in 0..r {
                out[i * c + j] = mx[j * c + i + r];
            }
        }

        ((c - r), c)
    }

    pub fn invert_matrix(mx: &mut [u8], k: usize) -> Option<()> {
        assert_eq!(mx.len(), k * k);

        let mut unused_rows = vec![true; k];
        let mut swaps = vec![];
        for i in 0..k {
            let pivot = unused_rows
                .iter_mut()
                .enumerate()
                .position(|(j, unused)| mx[j * k + i] != 0 && mem::take(unused))?;

            if pivot != i {
                let [a, b] = [pivot.min(i), pivot.max(i)];
                matrix_swap_rows(mx, k, a, b);
                swaps.push((a, b));
            }

            let (above_pivot, rest) = mx.split_at_mut(i * k);
            let (pivot_row, below_pivot) = rest.split_at_mut(k);

            let c = pivot_row[i];

            if c != 1 {
                let c = galois::div(1, c)?;
                pivot_row[i] = 1;
                pivot_row.iter_mut().for_each(|x| *x = galois::mul(c, *x));
            }

            if pivot_row[..i].iter().chain(pivot_row[i + 1..].iter()).all(|&x| x == 0) {
                continue;
            }

            // we avoid chain since that for some reason makes things slower

            for row in above_pivot.chunks_exact_mut(k) {
                let c = std::mem::take(&mut row[i]);
                galois::mul_slice_xor(c, pivot_row, row);
            }

            for row in below_pivot.chunks_exact_mut(k) {
                let c = std::mem::take(&mut row[i]);
                galois::mul_slice_xor(c, pivot_row, row);
            }
        }

        for (a, b) in swaps.into_iter().rev() {
            matrix_swap_rows(mx, k, a, b);
        }

        Some(())
    }

    #[inline(always)]
    pub fn matrix_swap_rows(mx: &mut [u8], c: usize, a: usize, b: usize) {
        assert!(a < b);
        let (left, right) = mx.split_at_mut(b * c);
        left[a * c..a * c + c].swap_with_slice(&mut right[..c]);
    }

    #[inline(always)]
    pub fn matrix_add_mul_rows(mx: &mut [u8], c: usize, a: usize, b: usize, fac: u8) {
        let (source, dest) = if a < b {
            let (left, right) = mx.split_at_mut(b * c);
            (&left[a * c..a * c + c], &mut right[..c])
        } else {
            let (left, right) = mx.split_at_mut(a * c);
            (&right[..c], &mut left[b * c..b * c + c])
        };
        galois::mul_slice_xor(fac, source, dest);
    }

    pub fn create_inverted_vdm(vdm: &mut [u8], k: usize) {
        assert!(vdm.len() >= k * k);

        if k == 1 {
            vdm[0] = 1;
            return;
        }

        let mut b = vec![0; k];
        let mut c = vec![0; k];

        c[k - 1] = 0;
        for i in 1..k {
            let mul_p_i = &galois::MUL_TABLE[galois::EXP_TABLE[i] as usize];
            for j in (k - 1 - (i - 1))..(k - 1) {
                c[j] ^= mul_p_i[c[j + 1] as usize];
            }
            c[k - 1] ^= galois::EXP_TABLE[i];
        }

        for row in 0..k {
            let index = if row != 0 { galois::EXP_TABLE[row] as usize } else { 0 };
            let mul_p_row = &galois::MUL_TABLE[index];

            let mut t = 1;
            b[k - 1] = 1;
            for i in (0..(k - 1)).rev() {
                b[i] = c[i + 1] ^ mul_p_row[b[i + 1] as usize];
                t = b[i] ^ mul_p_row[t as usize];
            }

            let mul_t_inv = &galois::MUL_TABLE[galois::INV_TABLE[t as usize] as usize];
            for col in 0..k {
                vdm[col * k + row] = mul_t_inv[b[col] as usize];
            }
        }
    }

    #[derive(Debug, Clone, Default)]
    pub struct Poly {
        data: ArrayVec<u8, 256>,
    }

    impl Poly {
        pub fn from_iter(iter: impl IntoIterator<Item = u8>) -> Self {
            Self { data: iter.into_iter().collect() }
        }

        pub fn zero(size: usize) -> Self {
            Self { data: iter::repeat(0).take(size).collect() }
        }

        pub fn is_zero(&self) -> bool {
            self.data.iter().all(|&x| x == 0)
        }

        pub fn deg(&self) -> usize {
            self.data.len() - 1
        }

        pub fn scale(mut self, factor: u8) -> Self {
            self.data.iter_mut().for_each(|x| *x = galois::mul(factor, *x));
            self
        }

        pub fn add(mut self, b: Self) -> Self {
            self.data.iter_mut().zip(b.data).for_each(|(a, b)| *a = galois::add(*a, b));
            self
        }

        pub fn sanitize(&mut self) {
            let trailing_zeros = self.data.iter().rev().take_while(|&&x| x == 0).count();
            self.data.truncate((self.data.len() - trailing_zeros).max(1));
        }

        pub fn div(mut self, mut b: Self) -> Option<(Self, Self)> {
            self.sanitize();
            b.sanitize();

            if b.data.is_empty() {
                return None;
            }

            if self.data.is_empty() {
                return Some((Self::zero(1), Self::zero(1)));
            }

            let mut q = Self::zero(self.deg() - b.deg() + 1);
            let mut p = self;

            while b.deg() <= p.deg() {
                let leading_p = p.data.last().copied().unwrap_or(0);
                let leading_b = b.data.last().copied().unwrap_or(0);

                let coef = galois::div(leading_p, leading_b)?;
                q.data.push(coef);

                let scaled = b.clone().scale(coef);
                let padded = Self::from_iter(
                    iter::repeat(0).take(p.deg() - scaled.deg()).chain(scaled.data),
                );

                p = p.add(padded);
                let pop = p.data.pop();
                debug_assert!(pop == Some(0));
            }

            q.data.reverse();

            p.sanitize();
            q.sanitize();

            Some((q, p))
        }

        pub fn eval(&self, x: u8) -> u8 {
            let mut out = 0;
            for (i, coef) in self.data.iter().enumerate() {
                out ^= galois::mul(*coef, galois::exp(x, i));
            }
            out
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn encode_rebuild() {
        let mut data = [1, 2, 3, 4, 5, 6, 7, 8];
        let expected = data;
        let mut parity = [0; 8];

        let fec = Fec::new(2, 4);

        fec.encode(&data, &mut parity).unwrap();

        let (left, _) = parity.split_at_mut(4);
        let mut shares =
            [Share { number: 1, data: &mut data[4..] }, Share { number: 2, data: left }];

        let shares = fec.rebuild(&mut shares, &mut vec![]).unwrap();

        assert_eq!(shares[0].data, &expected[..4]);
        assert_eq!(shares[1].data, &expected[4..]);
    }

    #[test]
    fn encode_damage_correct() {
        let data = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut parity = [0; 8];

        let fec = Fec::new(2, 4);

        fec.encode(&data, &mut parity).unwrap();

        let (left_parity, right_parity) = parity.split_at_mut(4);
        let mut corrupted = data;
        corrupted[0] = 0;
        let (left, right) = corrupted.split_at_mut(4);
        let mut shares = [
            Share { number: 0, data: left },
            Share { number: 1, data: right },
            Share { number: 2, data: left_parity },
            Share { number: 3, data: right_parity },
        ];

        let shares = fec.decode(&mut shares, false, &mut vec![]).unwrap();

        assert_eq!(shares[0].data, &data[..4]);
        assert_eq!(shares[1].data, &data[4..]);
    }

    #[test]
    #[ignore]
    fn bench_reed_solomon() {
        let f = Fec::new(32, 64);

        let mut data = [0; 32 * 1024];
        data.iter_mut().enumerate().for_each(|(i, x)| *x = i as u8);
        let mut parity = [0; 32 * 1024];

        f.encode(&data, &mut parity).unwrap();

        let mut parity_cp = parity;
        let mut shards = parity_cp
            .chunks_exact_mut(1024)
            .enumerate()
            .map(|(i, x)| Share { number: i + 32, data: x })
            .collect::<Vec<_>>();

        let now = std::time::Instant::now();
        let iters = 100000;
        let mut temp = vec![];
        for _ in 0..iters {
            f.rebuild(&mut shards, &mut temp).unwrap();
            for (shard, chunk) in shards.iter_mut().zip(parity.chunks_exact_mut(1024)) {
                shard.data.copy_from_slice(chunk);
                shard.number = usize::MAX - shard.number;
            }
        }
        println!("{:?}", now.elapsed().checked_div(iters).unwrap());
    }
}
