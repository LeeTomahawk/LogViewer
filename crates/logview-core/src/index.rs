//! Index of line start offsets, stored in a [`CompactU64`] (~4 B per line):
//! 100 M lines take ~400 MB instead of 800 MB with plain `u64` offsets.

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::compact::CompactU64;

/// Index of line start offsets in a byte buffer.
///
/// While indexing is in progress the last stored start belongs to a line whose
/// end is not known yet, so it is not counted in [`LineIndex::line_count`].
/// [`LineIndex::finish`] makes that last line visible.
#[derive(Debug, Default)]
pub struct LineIndex {
    starts: CompactU64,
    data_len: u64,
    complete: bool,
}

impl LineIndex {
    /// Creates an index for a buffer of `data_len` bytes. A non-empty buffer
    /// always has a line starting at offset 0.
    pub fn new(data_len: u64) -> Self {
        let mut index = Self {
            data_len,
            ..Self::default()
        };
        if data_len > 0 {
            index.push_start(0);
        }
        index
    }

    /// Records a line start. Starts must be pushed in increasing order.
    pub fn push_start(&mut self, start: u64) {
        self.starts.push(start);
    }

    /// Marks the index as complete. A start equal to the data length (left by a
    /// trailing newline) does not begin a real line and is dropped.
    pub fn finish(&mut self) {
        if self.starts.last() == Some(self.data_len) {
            self.starts.pop();
        }
        // Release the spare capacity left by `Vec` growth (up to 2x).
        self.starts.shrink_to_fit();
        self.complete = true;
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Prepares a complete index for appending (live tail): an incomplete
    /// last line (after `stable_end`, the offset just past the last `\n`) is
    /// removed; it will come back complete through [`append_complete`].
    ///
    /// [`append_complete`]: LineIndex::append_complete
    pub fn make_growable(&mut self, stable_end: u64) {
        debug_assert!(self.complete);
        if self.data_len > stable_end && self.starts.last() == Some(stable_end) {
            self.starts.pop();
        }
        self.data_len = stable_end;
    }

    /// Appends complete lines (`chunk` ends with `\n`) starting at the
    /// current end of the data.
    pub fn append_complete(&mut self, chunk_start: u64, chunk: &[u8]) {
        debug_assert_eq!(chunk_start, self.data_len);
        debug_assert_eq!(chunk.last(), Some(&b'\n'));
        if chunk.is_empty() {
            return;
        }
        self.starts.push(chunk_start);
        for p in memchr::memchr_iter(b'\n', &chunk[..chunk.len() - 1]) {
            self.starts.push(chunk_start + p as u64 + 1);
        }
        self.data_len = chunk_start + chunk.len() as u64;
    }

    /// End of the indexed data (bytes).
    pub fn data_len(&self) -> u64 {
        self.data_len
    }

    /// Number of lines whose extent is known.
    pub fn line_count(&self) -> usize {
        if self.complete {
            self.starts.len()
        } else {
            self.starts.len().saturating_sub(1)
        }
    }

    /// Byte range `[start, end)` of `line`, including its line terminator.
    pub fn line_span(&self, line: usize) -> Option<(u64, u64)> {
        if line >= self.line_count() {
            return None;
        }
        let start = self.starts.get(line)?;
        let end = self.starts.get(line + 1).unwrap_or(self.data_len);
        Some((start, end))
    }

    /// Approximate heap memory used by the index, in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.starts.memory_bytes()
    }

    /// Builds a complete index synchronously. Used by tests and small inputs.
    pub fn build(data: &[u8], chunk_size: usize) -> Self {
        let mut index = Self::new(data.len() as u64);
        let cancel = AtomicBool::new(false);
        scan_newlines(data, chunk_size, 4, &cancel, |batch, _| {
            for starts in batch {
                for s in starts {
                    index.push_start(s);
                }
            }
        });
        index.finish();
        index
    }
}

/// Scans `data` for `\n` in parallel and reports the start offset of every
/// following line, in order.
///
/// The data is split into chunks of `chunk_size` bytes; `batch_chunks` chunks
/// are scanned in parallel at a time, then `sink` receives the per-chunk start
/// lists (in file order) and the number of bytes scanned so far.
///
/// Returns `false` if scanning was cancelled.
pub fn scan_newlines(
    data: &[u8],
    chunk_size: usize,
    batch_chunks: usize,
    cancel: &AtomicBool,
    mut sink: impl FnMut(Vec<Vec<u64>>, u64),
) -> bool {
    let chunk_size = chunk_size.max(1);
    let batch_bytes = chunk_size * batch_chunks.max(1);
    let mut pos = 0usize;
    while pos < data.len() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let batch_end = (pos + batch_bytes).min(data.len());
        let starts: Vec<Vec<u64>> = data[pos..batch_end]
            .par_chunks(chunk_size)
            .enumerate()
            .map(|(i, chunk)| {
                let chunk_base = (pos + i * chunk_size) as u64;
                memchr::memchr_iter(b'\n', chunk)
                    .map(|p| chunk_base + p as u64 + 1)
                    .collect()
            })
            .collect();
        sink(starts, batch_end as u64);
        pos = batch_end;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference implementation: start/end of each line, terminator included.
    fn naive_spans(data: &[u8]) -> Vec<(u64, u64)> {
        let mut spans = Vec::new();
        let mut start = 0usize;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                spans.push((start as u64, i as u64 + 1));
                start = i + 1;
            }
        }
        if start < data.len() {
            spans.push((start as u64, data.len() as u64));
        }
        spans
    }

    fn spans(index: &LineIndex) -> Vec<(u64, u64)> {
        (0..index.line_count())
            .map(|i| index.line_span(i).unwrap())
            .collect()
    }

    #[test]
    fn empty_input_has_no_lines() {
        let index = LineIndex::build(b"", 4);
        assert_eq!(index.line_count(), 0);
        assert_eq!(index.line_span(0), None);
    }

    #[test]
    fn single_newline_is_one_empty_line() {
        let index = LineIndex::build(b"\n", 4);
        assert_eq!(spans(&index), vec![(0, 1)]);
    }

    #[test]
    fn missing_trailing_newline_keeps_last_line() {
        let index = LineIndex::build(b"a\nbb\nccc", 2);
        assert_eq!(spans(&index), vec![(0, 2), (2, 5), (5, 8)]);
    }

    #[test]
    fn trailing_newline_adds_no_empty_line() {
        let index = LineIndex::build(b"a\nbb\n", 2);
        assert_eq!(spans(&index), vec![(0, 2), (2, 5)]);
    }

    #[test]
    fn crlf_spans_include_terminator() {
        let index = LineIndex::build(b"a\r\nb\r\n", 3);
        assert_eq!(spans(&index), vec![(0, 3), (3, 6)]);
    }

    #[test]
    fn matches_naive_for_all_chunk_sizes() {
        let data = b"first\n\nthird line\r\n\n\nx\nlast without newline";
        let expected = naive_spans(data);
        for chunk in 1..=data.len() + 2 {
            assert_eq!(
                spans(&LineIndex::build(data, chunk)),
                expected,
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn only_newlines() {
        let data = b"\n\n\n\n";
        for chunk in 1..6 {
            assert_eq!(spans(&LineIndex::build(data, chunk)), naive_spans(data));
        }
    }

    #[test]
    fn many_lines() {
        let line_count = 200_017;
        let data: Vec<u8> = (0..line_count)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let index = LineIndex::build(&data, 1 << 12);
        assert_eq!(index.line_count(), line_count);
        assert_eq!(spans(&index), naive_spans(&data));
    }

    #[test]
    fn offsets_beyond_u32() {
        // Simulate starts far apart without allocating gigabytes.
        let far = u32::MAX as u64 + 10;
        let mut index = LineIndex::new(far * 3);
        index.push_start(far);
        index.push_start(far * 2);
        index.finish();
        assert_eq!(index.line_count(), 3);
        assert_eq!(index.line_span(0), Some((0, far)));
        assert_eq!(index.line_span(1), Some((far, far * 2)));
        assert_eq!(index.line_span(2), Some((far * 2, far * 3)));
    }

    #[test]
    fn incomplete_index_hides_last_line() {
        let mut index = LineIndex::new(10);
        index.push_start(4);
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.line_span(0), Some((0, 4)));
        assert_eq!(index.line_span(1), None);
        index.finish();
        assert_eq!(index.line_count(), 2);
    }

    #[test]
    fn growable_index_drops_partial_line_and_appends() {
        let data = b"a\nbb\npart";
        let mut index = LineIndex::build(data, 4);
        assert_eq!(index.line_count(), 3);
        index.make_growable(5);
        assert_eq!(spans(&index), vec![(0, 2), (2, 5)]);
        index.append_complete(5, b"partial\nc\n");
        assert_eq!(spans(&index), vec![(0, 2), (2, 5), (5, 13), (13, 15)]);
        index.append_complete(15, b"\n");
        assert_eq!(index.line_span(4), Some((15, 16)));
    }

    #[test]
    fn growable_from_empty_and_from_complete_data() {
        let mut empty = LineIndex::build(b"", 4);
        empty.make_growable(0);
        empty.append_complete(0, b"x\n");
        assert_eq!(spans(&empty), vec![(0, 2)]);

        let mut full = LineIndex::build(b"a\n", 4);
        full.make_growable(2);
        assert_eq!(full.line_count(), 1);
        full.append_complete(2, b"b\n");
        assert_eq!(spans(&full), vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn cancel_stops_scan() {
        let cancel = AtomicBool::new(true);
        let finished = scan_newlines(b"a\nb\n", 1, 1, &cancel, |_, _| panic!("no batches"));
        assert!(!finished);
    }
}
