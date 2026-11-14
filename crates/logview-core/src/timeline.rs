//! Histogram of records over time, per level.
//!
//! A [`TimelineJob`] splits the file's time range into equal buckets and
//! counts records per bucket and level, either all records or only the
//! records shown by a filter result. Counting runs in parallel in the
//! background; the result is published when complete.
//!
//! With a filter result, a record is counted once, at the row of its first
//! line (filters on multi-line records return all lines of a record; context
//! rows are not counted).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::file::Notify;
use crate::filter_job::FilterRows;
use crate::level::Level;
use crate::records::Records;

/// Count slots per bucket: index 0 = no level, then [`Level::code`].
pub const LEVEL_SLOTS: usize = 7;
const BLOCK: usize = 64 * 1024;

/// Record counts over time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeline {
    /// Start of the first bucket (seconds).
    pub start: i64,
    pub bucket_secs: i64,
    pub counts: Vec<[u32; LEVEL_SLOTS]>,
    /// Records without a timestamp.
    pub untimed: u64,
}

impl Timeline {
    /// Empty histogram covering `[min, max]` with at most `buckets` buckets.
    pub fn new(min: i64, max: i64, buckets: usize) -> Self {
        let span = (max - min).max(0) + 1;
        let bucket_secs = (span + buckets.max(1) as i64 - 1) / buckets.max(1) as i64;
        let bucket_secs = bucket_secs.max(1);
        let n = ((span + bucket_secs - 1) / bucket_secs) as usize;
        Self {
            start: min,
            bucket_secs,
            counts: vec![[0; LEVEL_SLOTS]; n],
            untimed: 0,
        }
    }

    pub fn end(&self) -> i64 {
        self.start + self.bucket_secs * self.counts.len() as i64
    }

    pub fn bucket_of(&self, secs: i64) -> Option<usize> {
        if secs < self.start {
            return None;
        }
        let b = ((secs - self.start) / self.bucket_secs) as usize;
        (b < self.counts.len()).then_some(b)
    }

    /// `[start, end)` of bucket `b` in seconds.
    pub fn bucket_range(&self, b: usize) -> (i64, i64) {
        let start = self.start + b as i64 * self.bucket_secs;
        (start, start + self.bucket_secs)
    }

    pub fn total(&self, b: usize) -> u32 {
        self.counts.get(b).map_or(0, |c| c.iter().sum())
    }

    pub fn max_total(&self) -> u32 {
        (0..self.counts.len())
            .map(|b| self.total(b))
            .max()
            .unwrap_or(0)
    }

    fn add(&mut self, ts: Option<i64>, level: Option<Level>) {
        match ts.and_then(|t| self.bucket_of(t)) {
            Some(b) => self.counts[b][level.map_or(0, Level::code) as usize] += 1,
            None => self.untimed += 1,
        }
    }

    fn merge(&mut self, other: &Timeline) {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            for (x, y) in a.iter_mut().zip(b) {
                *x += y;
            }
        }
        self.untimed += other.untimed;
    }

    fn empty_like(&self) -> Self {
        Self {
            counts: vec![[0; LEVEL_SLOTS]; self.counts.len()],
            untimed: 0,
            ..*self
        }
    }
}

struct Shared {
    result: Mutex<Option<Arc<Timeline>>>,
    cancel: AtomicBool,
    done: AtomicBool,
}

/// Computes a [`Timeline`] in the background. Dropping it cancels the work.
pub struct TimelineJob {
    shared: Arc<Shared>,
}

impl TimelineJob {
    /// Counts the complete records of `records` (or only those shown in
    /// `rows`) into at most `buckets` buckets over the file's time range.
    pub fn start(
        records: Records,
        rows: Option<FilterRows>,
        buckets: usize,
        notify: Notify,
    ) -> Self {
        let shared = Arc::new(Shared {
            result: Mutex::new(None),
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("logview-timeline".into())
            .spawn(move || {
                let result = compute(&records, rows.as_ref(), buckets, &worker.cancel);
                *worker.result.lock().unwrap_or_else(|p| p.into_inner()) = result.map(Arc::new);
                worker.done.store(true, Ordering::Release);
                notify();
            });
        if spawned.is_err() {
            shared.done.store(true, Ordering::Release);
        }
        Self { shared }
    }

    pub fn is_running(&self) -> bool {
        !self.shared.done.load(Ordering::Acquire)
    }

    /// The histogram once computed (`None` while running or when the file
    /// has no timestamps).
    pub fn result(&self) -> Option<Arc<Timeline>> {
        self.shared
            .result
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Drop for TimelineJob {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

fn compute(
    records: &Records,
    rows: Option<&FilterRows>,
    buckets: usize,
    cancel: &AtomicBool,
) -> Option<Timeline> {
    let (min, max) = records.time_bounds()?;
    let empty = Timeline::new(min, max, buckets);
    let cancelled = || cancel.load(Ordering::Relaxed);
    let parts: Vec<Timeline> = match rows {
        None => {
            let count = records.complete_records();
            let blocks: Vec<usize> = (0..count).step_by(BLOCK).collect();
            blocks
                .par_iter()
                .map(|&start| {
                    let mut t = empty.empty_like();
                    if !cancelled() {
                        for r in records.records(start..(start + BLOCK).min(count)) {
                            t.add(r.ts, r.level);
                        }
                    }
                    t
                })
                .collect()
        }
        Some(rows) => {
            let count = rows.len();
            let blocks: Vec<usize> = (0..count).step_by(BLOCK).collect();
            blocks
                .par_iter()
                .map(|&start| {
                    let mut t = empty.empty_like();
                    if cancelled() {
                        return t;
                    }
                    let entries = rows.rows(start..(start + BLOCK).min(count));
                    let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
                        return t;
                    };
                    let r0 = records.record_of_line(first.line);
                    let r1 = records.record_of_line(last.line) + 1;
                    let recs = records.records(r0..r1);
                    // Both lists are ordered by line: walk them together.
                    let mut i = 0;
                    for entry in entries.iter().filter(|e| e.is_match) {
                        while i + 1 < recs.len() && recs[i].lines.end <= entry.line {
                            i += 1;
                        }
                        if let Some(rec) = recs.get(i)
                            && rec.lines.start == entry.line
                        {
                            t.add(rec.ts, rec.level);
                        }
                    }
                    t
                })
                .collect()
        }
    };
    if cancelled() {
        return None;
    }
    let mut total = empty;
    for part in &parts {
        total.merge(part);
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::LogFile;
    use crate::filter::{FilterRule, FilterSpec};
    use crate::filter_job::FilterJob;
    use crate::format::LogFormat;
    use crate::records::ParseJob;
    use std::time::Duration;

    const LOG: &str = "\
2026-01-01 00:00:00.000 INFO  [w] app - a
2026-01-01 00:00:30.000 ERROR [w] app - b
\tat X.y(X.java:1)
2026-01-01 00:01:10.000 WARN  [w] app - c
2026-01-01 00:01:59.000 ERROR [w] app - d
";

    fn wait(cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn buckets_cover_range() {
        let t = Timeline::new(100, 219, 4);
        assert_eq!((t.bucket_secs, t.counts.len()), (30, 4));
        assert_eq!(t.bucket_of(100), Some(0));
        assert_eq!(t.bucket_of(219), Some(3));
        assert_eq!(t.bucket_of(99), None);
        assert_eq!(t.bucket_range(1), (130, 160));
        let one = Timeline::new(5, 5, 100);
        assert_eq!((one.bucket_secs, one.counts.len()), (1, 1));
    }

    #[test]
    fn counts_records_and_filter_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(&path, LOG).unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        let sample: Vec<&[u8]> = LOG.lines().map(str::as_bytes).collect();
        let format = Arc::new(LogFormat::builtins()[0].compile(&sample).unwrap());
        let parse = ParseJob::start(file.clone(), format, true, Arc::new(|| {}));
        wait(|| !parse.is_running());
        let records = parse.records();

        // 120 s range in 2 buckets of 60 s.
        let all = TimelineJob::start(records.clone(), None, 2, Arc::new(|| {}));
        wait(|| !all.is_running());
        let t = all.result().unwrap();
        assert_eq!(t.bucket_secs, 60);
        let error = Level::Error.code() as usize;
        assert_eq!(t.counts[0][Level::Info.code() as usize], 1);
        assert_eq!(t.counts[0][error], 1, "the stack trace is not a record");
        assert_eq!(t.counts[1][Level::Warn.code() as usize], 1);
        assert_eq!(t.counts[1][error], 1);

        // Filter "error" on records: returns all lines of the two records.
        let spec = FilterSpec {
            rules: vec![FilterRule::include("error")],
            ..Default::default()
        };
        let job = FilterJob::start(
            file,
            Arc::new(spec.compile().unwrap()),
            1,
            1,
            Some(records.clone()),
            Arc::new(|| {}),
        );
        wait(|| !job.is_running());
        let filtered = TimelineJob::start(records, Some(job.rows_handle()), 2, Arc::new(|| {}));
        wait(|| !filtered.is_running());
        let t = filtered.result().unwrap();
        let totals: Vec<u32> = (0..t.counts.len()).map(|b| t.total(b)).collect();
        assert_eq!(
            totals,
            [1, 1],
            "only ERROR records, once each, no context rows"
        );
        assert_eq!(t.max_total(), 1);
    }
}
