//! Background computation of the scrollbar marker bar.
//!
//! The rows of the view (all lines, or the rows of a filter result) are split
//! into a fixed number of buckets. For every bucket the job ORs the
//! [`Highlighter::marker_mask`] of its lines, plus [`SEARCH_BIT`] for lines
//! matching the search. The UI then paints one tick per pixel of the scrollbar.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use rayon::prelude::*;

use crate::file::{LogFile, Notify};
use crate::filter_job::FilterRows;
use crate::highlight::Highlighter;
use crate::pattern::Pattern;

/// Mask bit for lines matching the search.
pub const SEARCH_BIT: u64 = 1 << 63;
/// Rows processed by one parallel task.
const CHUNK_ROWS: usize = 64 * 1024;

struct Shared {
    buckets: RwLock<Vec<u64>>,
    cancel: AtomicBool,
    done: AtomicBool,
    processed: AtomicUsize,
}

/// Marker bar data for a fixed set of rows. Dropping the job cancels it.
pub struct MinimapJob {
    shared: Arc<Shared>,
    total_rows: usize,
}

impl MinimapJob {
    /// Computes markers for `rows` of `file` (all lines when `None`). The row
    /// count is fixed at start; restart the job when it changes.
    pub fn start(
        file: LogFile,
        rows: Option<FilterRows>,
        highlighter: Arc<Highlighter>,
        search: Option<Arc<Pattern>>,
        bucket_count: usize,
        notify: Notify,
    ) -> Self {
        let bucket_count = bucket_count.max(1);
        let total_rows = rows
            .as_ref()
            .map_or_else(|| file.line_count(), FilterRows::len);
        let shared = Arc::new(Shared {
            buckets: RwLock::new(vec![0; bucket_count]),
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            processed: AtomicUsize::new(0),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("logview-minimap".into())
            .spawn(move || {
                let ctx = Ctx {
                    file: &file,
                    rows: rows.as_ref(),
                    highlighter: &highlighter,
                    search: search.as_deref(),
                    total_rows,
                    bucket_count,
                };
                run(&ctx, &worker, &notify);
                worker.done.store(true, Ordering::Release);
                notify();
            });
        if spawned.is_err() {
            shared.done.store(true, Ordering::Release);
        }
        Self { shared, total_rows }
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn is_running(&self) -> bool {
        !self.shared.done.load(Ordering::Acquire)
    }

    pub fn progress(&self) -> f32 {
        if self.total_rows == 0 {
            1.0
        } else {
            self.shared.processed.load(Ordering::Relaxed) as f32 / self.total_rows as f32
        }
    }

    /// Calls `f` with the bucket masks computed so far.
    pub fn with_buckets<R>(&self, f: impl FnOnce(&[u64]) -> R) -> R {
        f(&self
            .shared
            .buckets
            .read()
            .unwrap_or_else(|p| p.into_inner()))
    }
}

impl Drop for MinimapJob {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

struct Ctx<'a> {
    file: &'a LogFile,
    rows: Option<&'a FilterRows>,
    highlighter: &'a Highlighter,
    search: Option<&'a Pattern>,
    total_rows: usize,
    bucket_count: usize,
}

impl Ctx<'_> {
    fn bucket(&self, row: usize) -> usize {
        ((row as u128 * self.bucket_count as u128) / self.total_rows.max(1) as u128) as usize
    }

    fn mask(&self, bytes: &[u8]) -> u64 {
        let mut mask = self.highlighter.marker_mask(bytes);
        if self.search.is_some_and(|s| s.is_match(bytes)) {
            mask |= SEARCH_BIT;
        }
        mask
    }

    /// (bucket, mask) pairs for rows `start..end`, one per non-empty bucket.
    fn chunk(&self, start: usize, end: usize) -> Vec<(usize, u64)> {
        let mut out: Vec<(usize, u64)> = Vec::new();
        let mut add = |row: usize, bytes: &[u8]| {
            let mask = self.mask(bytes);
            if mask == 0 {
                return;
            }
            let bucket = self.bucket(row);
            match out.last_mut() {
                Some((b, m)) if *b == bucket => *m |= mask,
                _ => out.push((bucket, mask)),
            }
        };
        match self.rows {
            None => {
                for (line, bytes) in self.file.lines(start..end) {
                    add(line, bytes);
                }
            }
            Some(rows) => {
                for (i, entry) in rows.rows(start..end).into_iter().enumerate() {
                    add(start + i, self.file.line(entry.line).unwrap_or_default());
                }
            }
        }
        out
    }
}

fn run(ctx: &Ctx<'_>, shared: &Shared, notify: &Notify) {
    let batch = CHUNK_ROWS * rayon::current_num_threads().max(1) * 4;
    let mut start = 0;
    while start < ctx.total_rows {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let end = (start + batch).min(ctx.total_rows);
        let chunks: Vec<usize> = (start..end).step_by(CHUNK_ROWS).collect();
        let results: Vec<Vec<(usize, u64)>> = chunks
            .par_iter()
            .map(|&s| {
                if shared.cancel.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                ctx.chunk(s, (s + CHUNK_ROWS).min(end))
            })
            .collect();
        {
            let mut buckets = shared.buckets.write().unwrap_or_else(|p| p.into_inner());
            for (bucket, mask) in results.into_iter().flatten() {
                if let Some(slot) = buckets.get_mut(bucket) {
                    *slot |= mask;
                }
            }
        }
        shared.processed.store(end, Ordering::Relaxed);
        start = end;
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::HighlightRule;
    use crate::pattern::PatternKind;
    use std::io::Write;
    use std::time::Duration;

    fn wait(job: &MinimapJob) {
        while job.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn markers_land_in_buckets() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // 10 lines; ERROR on line 0, search hit on line 9.
        let content: String = (0..10)
            .map(|i| match i {
                0 => "ERROR a\n".to_owned(),
                9 => "needle\n".to_owned(),
                _ => format!("line {i}\n"),
            })
            .collect();
        tmp.write_all(content.as_bytes()).unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        while file.is_indexing() {
            std::thread::sleep(Duration::from_millis(1));
        }
        let highlighter = Arc::new(Highlighter::compile(&HighlightRule::defaults()).unwrap());
        let search = Arc::new(Pattern::new("needle", PatternKind::Text, false).unwrap());
        let job = MinimapJob::start(file, None, highlighter, Some(search), 5, Arc::new(|| {}));
        wait(&job);
        let buckets = job.with_buckets(<[u64]>::to_vec);
        assert_eq!(buckets.len(), 5);
        assert_eq!(buckets[0], 1); // rule 0 (ERROR)
        assert_eq!(buckets[1..4], [0, 0, 0]);
        assert_eq!(buckets[4], SEARCH_BIT);
        assert_eq!(job.total_rows(), 10);
    }
}
