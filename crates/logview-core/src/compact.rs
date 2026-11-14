//! Compact storage of a non-decreasing sequence of `u64` values.
//!
//! Values are stored as `u32` offsets relative to the base of a block; a new
//! block starts only when an offset no longer fits in `u32`. This gives ~4 bytes
//! per value instead of 8, while still supporting values beyond 4 G. Used for
//! line start offsets and for line numbers in filter results.

#[derive(Clone, Copy, Debug)]
struct Block {
    /// Index of the first value stored in this block.
    first: usize,
    /// Value the relative offsets of this block are added to.
    base: u64,
}

#[derive(Clone, Debug, Default)]
pub struct CompactU64 {
    blocks: Vec<Block>,
    rel: Vec<u32>,
}

impl CompactU64 {
    pub fn len(&self) -> usize {
        self.rel.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rel.is_empty()
    }

    /// Appends `value`, which must not be smaller than the last value.
    pub fn push(&mut self, value: u64) {
        debug_assert!(self.last().is_none_or(|last| last <= value));
        let needs_block = self
            .blocks
            .last()
            .is_none_or(|b| value - b.base > u32::MAX as u64);
        if needs_block {
            self.blocks.push(Block {
                first: self.rel.len(),
                base: value,
            });
        }
        let base = self.blocks.last().map_or(0, |b| b.base);
        self.rel.push((value - base) as u32);
    }

    fn block_of(&self, index: usize) -> &Block {
        &self.blocks[self.blocks.partition_point(|b| b.first <= index) - 1]
    }

    pub fn get(&self, index: usize) -> Option<u64> {
        let rel = *self.rel.get(index)?;
        Some(self.block_of(index).base + rel as u64)
    }

    pub fn last(&self) -> Option<u64> {
        self.len().checked_sub(1).and_then(|i| self.get(i))
    }

    pub fn pop(&mut self) -> Option<u64> {
        let value = self.last()?;
        self.rel.pop();
        if self
            .blocks
            .last()
            .is_some_and(|b| b.first == self.rel.len())
        {
            self.blocks.pop();
        }
        Some(value)
    }

    /// Index of the first value `>= value` (or `len()` if there is none).
    pub fn lower_bound(&self, value: u64) -> usize {
        let block = self.blocks.partition_point(|b| b.base <= value);
        if block == 0 {
            return 0;
        }
        let b = self.blocks[block - 1];
        let end = self.blocks.get(block).map_or(self.rel.len(), |n| n.first);
        b.first + self.rel[b.first..end].partition_point(|&r| b.base + (r as u64) < value)
    }

    pub fn iter_range(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = u64> + '_ {
        let end = range.end.min(self.len());
        (range.start..end).map(|i| self.get(i).unwrap_or_default())
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.rel.clear();
    }

    pub fn shrink_to_fit(&mut self) {
        self.rel.shrink_to_fit();
        self.blocks.shrink_to_fit();
    }

    /// Heap memory used, in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.rel.capacity() * size_of::<u32>() + self.blocks.capacity() * size_of::<Block>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_get_pop() {
        let mut v = CompactU64::default();
        assert_eq!(v.last(), None);
        for x in [0, 5, 5, 1000] {
            v.push(x);
        }
        assert_eq!(v.len(), 4);
        assert_eq!(v.get(2), Some(5));
        assert_eq!(v.get(4), None);
        assert_eq!(v.pop(), Some(1000));
        assert_eq!(v.last(), Some(5));
    }

    #[test]
    fn values_beyond_u32_split_blocks() {
        let far = u32::MAX as u64 + 7;
        let mut v = CompactU64::default();
        let values = [1, far, far + 1, far * 3, far * 3];
        for x in values {
            v.push(x);
        }
        let got: Vec<u64> = v.iter_range(0..10).collect();
        assert_eq!(got, values);
        assert_eq!(v.pop(), Some(far * 3));
        assert_eq!(v.pop(), Some(far * 3));
        assert_eq!(v.last(), Some(far + 1));
        v.push(far * 2);
        assert_eq!(v.last(), Some(far * 2));
    }

    #[test]
    fn lower_bound_matches_linear_search() {
        let far = u32::MAX as u64;
        let values = [3, 3, 10, 20, far + 5, far + 6, far * 4];
        let mut v = CompactU64::default();
        for x in values {
            v.push(x);
        }
        let probes = [
            0,
            3,
            4,
            10,
            11,
            20,
            21,
            far,
            far + 5,
            far + 6,
            far + 7,
            far * 4,
            far * 5,
        ];
        for p in probes {
            let expected = values.iter().position(|&x| x >= p).unwrap_or(values.len());
            assert_eq!(v.lower_bound(p), expected, "probe {p}");
        }
        assert_eq!(CompactU64::default().lower_bound(5), 0);
    }
}
