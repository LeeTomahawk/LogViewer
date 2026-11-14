//! Line selection stored as sorted, disjoint, inclusive ranges, so selecting
//! all of a 100 M line file costs one entry.

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<(usize, usize)>,
}

impl Selection {
    pub fn clear(&mut self) {
        self.ranges.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn select_only(&mut self, line: usize) {
        self.ranges = vec![(line, line)];
    }

    /// Replaces the selection with the lines between `a` and `b` inclusive.
    pub fn select_range(&mut self, a: usize, b: usize) {
        self.ranges = vec![(a.min(b), a.max(b))];
    }

    pub fn contains(&self, line: usize) -> bool {
        let i = self.ranges.partition_point(|&(_, end)| end < line);
        self.ranges.get(i).is_some_and(|&(start, _)| start <= line)
    }

    pub fn toggle(&mut self, line: usize) {
        let i = self.ranges.partition_point(|&(_, end)| end < line);
        match self.ranges.get(i).copied() {
            Some((start, end)) if start <= line => {
                // Remove `line` from the range, splitting it if needed.
                let mut replacement = Vec::with_capacity(2);
                if start < line {
                    replacement.push((start, line - 1));
                }
                if line < end {
                    replacement.push((line + 1, end));
                }
                self.ranges.splice(i..=i, replacement);
            }
            _ => {
                self.ranges.insert(i, (line, line));
                self.merge_around(i);
            }
        }
    }

    /// Merges the range at `i` with adjacent neighbours.
    fn merge_around(&mut self, mut i: usize) {
        if i > 0 && self.ranges[i - 1].1 + 1 >= self.ranges[i].0 {
            self.ranges[i - 1].1 = self.ranges[i - 1].1.max(self.ranges[i].1);
            self.ranges.remove(i);
            i -= 1;
        }
        if i + 1 < self.ranges.len() && self.ranges[i].1 + 1 >= self.ranges[i + 1].0 {
            self.ranges[i].1 = self.ranges[i].1.max(self.ranges[i + 1].1);
            self.ranges.remove(i + 1);
        }
    }

    pub fn count(&self) -> usize {
        self.ranges.iter().map(|&(s, e)| e - s + 1).sum()
    }

    /// Selected lines in increasing order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges.iter().flat_map(|&(s, e)| s..=e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &Selection) -> Vec<usize> {
        s.iter().collect()
    }

    #[test]
    fn range_is_normalised() {
        let mut s = Selection::default();
        s.select_range(5, 2);
        assert_eq!(lines(&s), vec![2, 3, 4, 5]);
        assert_eq!(s.count(), 4);
        assert!(s.contains(2) && s.contains(5) && !s.contains(6) && !s.contains(1));
    }

    #[test]
    fn toggle_splits_range() {
        let mut s = Selection::default();
        s.select_range(1, 5);
        s.toggle(3);
        assert_eq!(lines(&s), vec![1, 2, 4, 5]);
        s.toggle(1);
        s.toggle(5);
        assert_eq!(lines(&s), vec![2, 4]);
    }

    #[test]
    fn toggle_merges_neighbours() {
        let mut s = Selection::default();
        s.toggle(10);
        s.toggle(12);
        s.toggle(11);
        assert_eq!(s.ranges, vec![(10, 12)]);
        s.toggle(9);
        s.toggle(13);
        assert_eq!(s.ranges, vec![(9, 13)]);
    }

    #[test]
    fn toggle_twice_is_identity() {
        let mut s = Selection::default();
        s.select_only(7);
        s.toggle(7);
        assert!(s.is_empty());
        s.toggle(0);
        assert_eq!(lines(&s), vec![0]);
    }
}
