use {
    component_utils::Codec,
    std::{ops::Range, u32},
};

type T = u32;

#[derive(Clone, Codec)]
pub struct SortedCompactVec {
    data: Vec<Range<T>>,
}

impl SortedCompactVec {
    pub fn new() -> Self {
        #[allow(clippy::single_range_in_vec_init)]
        Self { data: vec![0..T::MAX] }
    }

    pub fn lowest_active(&self) -> Option<T> {
        // this may panic but at that point we are fucked anyway
        self.data.first().map(|r| r.end)
    }

    #[must_use]
    pub fn push(&mut self, value: T) -> bool {
        self.push_range(value..value + 1)
    }

    pub fn push_range(&mut self, range: Range<T>) -> bool {
        let Err(i) = self.data.binary_search_by(|other| {
            if range.end < other.start {
                std::cmp::Ordering::Greater
            } else if range.start > other.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) else {
            return false;
        };

        let inc_prev =
            self.data.get(i.wrapping_sub(1)).is_some_and(|other| range.start == other.end + 1);
        let dec_next = self.data.get(i).is_some_and(|other| range.end == other.start + 1);
        match (inc_prev, dec_next) {
            (true, true) => {
                let other = self.data.remove(i);
                self.data[i - 1].end = other.end;
            }
            (true, false) => self.data[i - 1].end = range.end,
            (false, true) => self.data[i].start = range.start,
            (false, false) => {
                self.data.insert(i, range);
            }
        }

        true
    }

    pub fn pop(&mut self) -> Option<T> {
        let last = self.data.last_mut()?;
        if last.start == last.end {
            self.data.pop().map(|r| r.start)
        } else {
            last.end -= 1;
            Some(last.end)
        }
    }

    pub fn pop_n(&mut self, n: usize) -> Option<Range<T>> {
        let last = self.data.last_mut()?;

        if last.end - last.start <= n as u32 {
            self.data.pop()
        } else {
            last.end -= n as u32;
            Some(last.end..last.end + n as u32)
        }
    }
}

impl Default for SortedCompactVec {
    fn default() -> Self {
        Self::new()
    }
}
