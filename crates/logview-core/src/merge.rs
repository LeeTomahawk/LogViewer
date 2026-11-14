//! Several files merged into one view ordered by time.
//!
//! The merge works on records (a header line plus its continuation lines,
//! from each file's [`Records`]), so stack traces stay with their entry. For
//! every source the timestamps of all records are read in parallel (with
//! millisecond precision from the header line; a record without timestamp
//! takes the previous one, records before the first timestamp sort first).
//! A k-way merge then emits records ordered by `(time, source)`; within one
//! source the original order is always kept.
//!
//! Rows are stored compactly: one byte per row with the source index, plus a
//! checkpoint of every source's next line number each [`CHECKPOINT_ROWS`]
//! rows. The line of a row is the checkpoint plus the number of rows of the
//! same source since the checkpoint (lines of one source appear in order).
//! That is ~1 byte per row. Rows appear while the merge runs.
//!
//! Sources that are being followed are merged as they were when the merge
//! started.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::file::{CancelToken, LogFile, Notify};
use crate::format::CompiledFormat;
use crate::records::{RecordInfo, Records};

/// Rows between two checkpoints of line positions.
pub const CHECKPOINT_ROWS: usize = 1024;
/// Maximum number of merged files (source index is one byte).
pub const MAX_SOURCES: usize = 255;
/// Records read per lock of a source's record list.
const RECORD_BLOCK: usize = 64 * 1024;
/// Rows published to readers at once.
const PUBLISH_ROWS: usize = 1 << 20;

/// One file taking part in a merge.
#[derive(Clone)]
pub struct MergeSource {
    pub file: LogFile,
    pub format: Arc<CompiledFormat>,
    pub records: Records,
    /// Short name shown in the view (usually the file name).
    pub name: String,
}

#[derive(Debug, Default)]
struct Rows {
    /// Source index per row.
    src: Vec<u8>,
    /// Next line number of every source at each checkpoint (stride = sources).
    checkpoints: Vec<u64>,
}

impl Rows {
    fn locate(&self, sources: usize, range: Range<usize>) -> Vec<(usize, usize)> {
        let end = range.end.min(self.src.len());
        if range.start >= end {
            return Vec::new();
        }
        let block = range.start / CHECKPOINT_ROWS;
        let mut counters: Vec<u64> =
            self.checkpoints[block * sources..(block + 1) * sources].to_vec();
        for &s in &self.src[block * CHECKPOINT_ROWS..range.start] {
            counters[s as usize] += 1;
        }
        let mut out = Vec::with_capacity(end - range.start);
        for &s in &self.src[range.start..end] {
            out.push((s as usize, counters[s as usize] as usize));
            counters[s as usize] += 1;
        }
        out
    }
}

/// The merged rows of a merged [`LogFile`].
pub struct Merged {
    pub sources: Vec<MergeSource>,
    rows: RwLock<Rows>,
    done: AtomicBool,
    /// Lines that will be merged in total (known once the merge has started).
    total: AtomicUsize,
    started: Instant,
    elapsed_us: AtomicU64,
}

impl Merged {
    fn read(&self) -> RwLockReadGuard<'_, Rows> {
        self.rows.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn row_count(&self) -> usize {
        self.read().src.len()
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn progress(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if self.is_done() || total == 0 {
            if self.is_done() { 1.0 } else { 0.0 }
        } else {
            self.row_count() as f32 / total as f32
        }
    }

    pub fn elapsed(&self) -> Duration {
        if self.is_done() {
            Duration::from_micros(self.elapsed_us.load(Ordering::Relaxed))
        } else {
            self.started.elapsed()
        }
    }

    pub fn memory_bytes(&self) -> usize {
        let rows = self.read();
        rows.src.capacity() + rows.checkpoints.capacity() * size_of::<u64>()
    }

    /// `(source index, line in that source)` for rows in `range` (clamped).
    pub fn origins(&self, range: Range<usize>) -> Vec<(usize, usize)> {
        self.read().locate(self.sources.len(), range)
    }

    pub fn origin(&self, row: usize) -> Option<(usize, usize)> {
        self.origins(row..row + 1).pop()
    }

    /// Bytes of the line behind `row`.
    pub fn line(&self, row: usize) -> Option<&[u8]> {
        let (src, line) = self.origin(row)?;
        self.sources[src].file.line(line)
    }

    pub fn lines(&self, range: Range<usize>) -> Vec<(usize, &[u8])> {
        let start = range.start;
        self.origins(range)
            .into_iter()
            .enumerate()
            .map(|(i, (src, line))| {
                (
                    start + i,
                    self.sources[src].file.line(line).unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// Starts merging `sources` (at most [`MAX_SOURCES`]) on a background
/// thread and returns the merged file at once; rows appear as the merge
/// proceeds. `name` is shown as the file's path.
pub fn merge_files(sources: Vec<MergeSource>, name: &str, notify: Notify) -> LogFile {
    let mut sources = sources;
    sources.truncate(MAX_SOURCES);
    let merged = Arc::new(Merged {
        sources,
        rows: RwLock::new(Rows::default()),
        done: AtomicBool::new(false),
        total: AtomicUsize::new(0),
        started: Instant::now(),
        elapsed_us: AtomicU64::new(0),
    });
    let file = LogFile::from_merged(Arc::clone(&merged), PathBuf::from(name));
    let cancel = file.cancel_flag();
    let spawned = std::thread::Builder::new()
        .name("logview-merge".into())
        .spawn({
            let merged = Arc::clone(&merged);
            move || {
                merge_worker(&merged, &cancel, &notify);
                merged.elapsed_us.store(
                    merged.started.elapsed().as_micros() as u64,
                    Ordering::Relaxed,
                );
                merged.done.store(true, Ordering::Release);
                notify();
            }
        });
    if spawned.is_err() {
        merged.done.store(true, Ordering::Release);
    }
    file
}

/// Records of one source in file order, read in blocks.
struct Cursor<'a> {
    records: &'a Records,
    /// Number of records taking part.
    count: usize,
    next: usize,
    buffer: Vec<RecordInfo>,
    buffer_start: usize,
}

impl Cursor<'_> {
    fn get(&mut self, r: usize) -> Option<RecordInfo> {
        if r >= self.count {
            return None;
        }
        if r < self.buffer_start || r >= self.buffer_start + self.buffer.len() {
            self.buffer_start = r;
            self.buffer = self.records.records(r..(r + RECORD_BLOCK).min(self.count));
        }
        self.buffer.get(r - self.buffer_start).cloned()
    }
}

fn merge_worker(merged: &Merged, cancel: &CancelToken, notify: &Notify) {
    let cancelled = || cancel.is_cancelled();
    // Wait until every source is indexed and parsed (or caught up when it is
    // followed).
    loop {
        if cancelled() {
            return;
        }
        let ready = merged
            .sources
            .iter()
            .all(|s| !s.file.is_indexing() && (s.records.is_done() || s.records.is_caught_up()));
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let counts: Vec<usize> = merged
        .sources
        .iter()
        .map(|s| s.records.complete_records())
        .collect();
    let total: usize = merged
        .sources
        .iter()
        .zip(&counts)
        .map(|(s, &n)| {
            n.checked_sub(1)
                .and_then(|last| s.records.record(last))
                .map_or(0, |r| r.lines.end)
        })
        .sum();
    merged.total.store(total, Ordering::Relaxed);

    // Millisecond timestamps of all records, in parallel.
    let times: Vec<Vec<i64>> = merged
        .sources
        .iter()
        .zip(&counts)
        .map(|(source, &count)| record_times(source, count, cancel))
        .collect();
    if cancelled() {
        return;
    }

    let n = merged.sources.len();
    let mut cursors: Vec<Cursor<'_>> = merged
        .sources
        .iter()
        .zip(&counts)
        .map(|(s, &count)| Cursor {
            records: &s.records,
            count,
            next: 0,
            buffer: Vec::new(),
            buffer_start: 0,
        })
        .collect();
    let mut heap: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::with_capacity(n);
    for (i, t) in times.iter().enumerate() {
        if let Some(&first) = t.first() {
            heap.push(Reverse((first, i)));
        }
    }

    let mut counters = vec![0u64; n];
    let mut pending: Vec<u8> = Vec::with_capacity(PUBLISH_ROWS);
    let publish = |pending: &mut Vec<u8>, counters: &mut [u64]| {
        let mut rows = merged.rows.write().unwrap_or_else(|p| p.into_inner());
        for &s in pending.iter() {
            if rows.src.len().is_multiple_of(CHECKPOINT_ROWS) {
                rows.checkpoints.extend_from_slice(counters);
            }
            rows.src.push(s);
            counters[s as usize] += 1;
        }
        pending.clear();
    };
    while let Some(Reverse((_, src))) = heap.pop() {
        if cancelled() {
            return;
        }
        let cursor = &mut cursors[src];
        let Some(record) = cursor.get(cursor.next) else {
            continue;
        };
        pending.extend(std::iter::repeat_n(src as u8, record.lines.len()));
        cursor.next += 1;
        if let Some(&t) = times[src].get(cursor.next) {
            heap.push(Reverse((t, src)));
        }
        if pending.len() >= PUBLISH_ROWS {
            publish(&mut pending, &mut counters);
            notify();
        }
    }
    publish(&mut pending, &mut counters);
    let mut rows = merged.rows.write().unwrap_or_else(|p| p.into_inner());
    rows.src.shrink_to_fit();
    rows.checkpoints.shrink_to_fit();
}

/// Timestamp (ms) of each of the first `count` records; a record without one
/// takes the previous record's, records before the first get `i64::MIN`.
fn record_times(source: &MergeSource, count: usize, cancel: &CancelToken) -> Vec<i64> {
    let blocks: Vec<usize> = (0..count).step_by(RECORD_BLOCK).collect();
    let parts: Vec<Vec<Option<i64>>> = blocks
        .par_iter()
        .map(|&start| {
            if cancel.is_cancelled() {
                return Vec::new();
            }
            source
                .records
                .records(start..(start + RECORD_BLOCK).min(count))
                .into_iter()
                .map(|r| {
                    source
                        .file
                        .line(r.lines.start)
                        .and_then(|line| source.format.meta(line))
                        .and_then(|m| m.ts)
                        // Fall back to the record's second-resolution time.
                        .or(r.ts.map(|s| s * 1000))
                })
                .collect()
        })
        .collect();
    let mut out = Vec::with_capacity(count);
    let mut last = i64::MIN;
    for t in parts.into_iter().flatten() {
        if let Some(t) = t {
            last = t;
        }
        out.push(last);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::LogFormat;
    use crate::records::ParseJob;

    fn source(dir: &std::path::Path, name: &str, content: &str) -> (MergeSource, ParseJob) {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        let sample: Vec<&[u8]> = content.lines().map(str::as_bytes).collect();
        let format = Arc::new(LogFormat::builtins()[0].compile(&sample).unwrap());
        let job = ParseJob::start(file.clone(), Arc::clone(&format), true, Arc::new(|| {}));
        (
            MergeSource {
                file,
                format,
                records: job.records(),
                name: name.into(),
            },
            job,
        )
    }

    fn wait(file: &LogFile) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while file.is_indexing() {
            assert!(Instant::now() < deadline, "merge timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn merges_by_time_keeping_records_together() {
        let dir = tempfile::tempdir().unwrap();
        let (a, _ja) = source(
            dir.path(),
            "a.log",
            "2026-01-01 00:00:01.000 INFO  [t] a - a1\n\
             2026-01-01 00:00:03.000 ERROR [t] a - a2\n\
             \tat A.trace(A.java:1)\n\
             2026-01-01 00:00:05.500 INFO  [t] a - a3\n",
        );
        let (b, _jb) = source(
            dir.path(),
            "b.log",
            "2026-01-01 00:00:02.000 INFO  [t] b - b1\n\
             2026-01-01 00:00:03.000 INFO  [t] b - b2 same second as a2\n\
             2026-01-01 00:00:05.250 WARN  [t] b - b3\n",
        );
        let merged = merge_files(vec![a, b], "scalone", Arc::new(|| {}));
        wait(&merged);
        let texts: Vec<String> = merged
            .lines(0..100)
            .into_iter()
            .map(|(_, l)| {
                String::from_utf8_lossy(l)
                    .rsplit(" - ")
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            texts,
            [
                "a1",
                "b1",
                "a2",
                "\tat A.trace(A.java:1)",
                "b2 same second as a2",
                "b3",
                "a3"
            ]
        );
        assert_eq!(merged.origin(3), Some((0, 2)));
        assert_eq!(merged.origin(4), Some((1, 1)));
        assert_eq!(merged.merged_sources().unwrap()[1].name, "b.log");
        assert_eq!(merged.path(), std::path::Path::new("scalone"));
    }

    #[test]
    fn many_rows_use_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        // Interleaved timestamps: even seconds in a, odd in b.
        let make = |offset: usize| -> String {
            (0..3000)
                .map(|i| {
                    let s = i * 2 + offset;
                    format!(
                        "2026-01-01 {:02}:{:02}:{:02}.000 INFO  [t] x - n{s}\n",
                        s / 3600,
                        s / 60 % 60,
                        s % 60
                    )
                })
                .collect()
        };
        let (a, _ja) = source(dir.path(), "a.log", &make(0));
        let (b, _jb) = source(dir.path(), "b.log", &make(1));
        let merged = merge_files(vec![a, b], "m", Arc::new(|| {}));
        wait(&merged);
        assert_eq!(merged.line_count(), 6000);
        for row in [0, 1, 1023, 1024, 1025, 4097, 5999] {
            let text = String::from_utf8_lossy(merged.line(row).unwrap()).into_owned();
            assert!(text.ends_with(&format!(" - n{row}")), "row {row}: {text}");
        }
        let block: Vec<usize> = merged
            .origins(2047..2050)
            .unwrap()
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(block, [1, 0, 1]);
    }
}
