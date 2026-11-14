//! Background filtering.
//!
//! A [`FilterJob`] scans the file in batches of lines. Each batch is split into
//! blocks matched in parallel (rayon); matches are then merged in order and
//! expanded with context lines. Results are published after every batch, so the
//! UI shows them while the scan is still running.
//!
//! The job follows the indexer: if the file is still being indexed it waits for
//! more lines instead of stopping. Dropping the job cancels it.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::compact::CompactU64;
use crate::file::{LogFile, Notify};
use crate::filter::CompiledFilter;
use crate::records::Records;

/// Lines matched by one parallel task.
const BLOCK_LINES: usize = 16 * 1024;
/// Records matched by one parallel task.
const BLOCK_RECORDS: usize = 8 * 1024;
/// How long to wait for the indexer when all indexed lines are filtered.
const WAIT_FOR_INDEX: Duration = Duration::from_millis(5);
/// How long to wait for a followed file to grow.
const WAIT_FOR_GROWTH: Duration = Duration::from_millis(50);

/// One row of a filter result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowEntry {
    pub line: usize,
    /// `false` for context lines.
    pub is_match: bool,
}

/// Rows of a filter result: matching lines and their context, in line order.
#[derive(Debug, Default)]
struct FilterResult {
    lines: CompactU64,
    /// One bit per row: set for matches, clear for context lines.
    match_bits: Vec<u64>,
    matches: usize,
}

impl FilterResult {
    fn push(&mut self, line: usize, is_match: bool) {
        let row = self.lines.len();
        self.lines.push(line as u64);
        if row.is_multiple_of(64) {
            self.match_bits.push(0);
        }
        if is_match {
            self.match_bits[row / 64] |= 1 << (row % 64);
            self.matches += 1;
        }
    }

    fn get(&self, row: usize) -> Option<RowEntry> {
        let line = self.lines.get(row)? as usize;
        let is_match = self.match_bits[row / 64] & (1 << (row % 64)) != 0;
        Some(RowEntry { line, is_match })
    }

    fn memory_bytes(&self) -> usize {
        self.lines.memory_bytes() + self.match_bits.capacity() * size_of::<u64>()
    }
}

/// Turns an increasing stream of matching lines into rows with context lines,
/// without emitting any line twice.
#[derive(Debug)]
struct ContextExpander {
    before: usize,
    after: usize,
    /// First line not emitted yet.
    next_free: usize,
    /// End (exclusive) of the pending after-context of the last match.
    after_until: usize,
}

impl ContextExpander {
    fn new(before: usize, after: usize) -> Self {
        Self {
            before,
            after,
            next_free: 0,
            after_until: 0,
        }
    }

    fn push_match(&mut self, line: usize, out: &mut FilterResult) {
        let before_from = line.saturating_sub(self.before).max(self.next_free);
        // After-context of earlier matches that ends before this match's
        // before-context starts…
        for l in self.next_free..self.after_until.min(before_from) {
            out.push(l, false);
        }
        // …then this match's before-context and the match itself.
        for l in before_from..line {
            out.push(l, false);
        }
        out.push(line, true);
        self.next_free = line + 1;
        self.after_until = line.saturating_add(1).saturating_add(self.after);
    }

    /// Emits pending after-context up to (excluding) `end`. Every line below
    /// `end` must already have been offered to `push_match`.
    fn flush(&mut self, end: usize, out: &mut FilterResult) {
        let until = self.after_until.min(end);
        for l in self.next_free..until {
            out.push(l, false);
        }
        self.next_free = self.next_free.max(until);
    }
}

struct JobShared {
    result: RwLock<FilterResult>,
    cancel: AtomicBool,
    done: AtomicBool,
    /// All lines so far are processed and the file is only followed.
    caught_up: AtomicBool,
    processed: AtomicUsize,
    elapsed_us: AtomicU64,
    has_context: bool,
}

impl JobShared {
    fn read(&self) -> RwLockReadGuard<'_, FilterResult> {
        self.result.read().unwrap_or_else(|p| p.into_inner())
    }
}

/// Read access to the rows of a [`FilterJob`], usable from other threads
/// (e.g. by an export running while the UI keeps the job).
#[derive(Clone)]
pub struct FilterRows(Arc<JobShared>);

impl FilterRows {
    /// Number of rows (matches + context lines) so far.
    pub fn len(&self) -> usize {
        self.0.read().lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, row: usize) -> Option<RowEntry> {
        self.0.read().get(row)
    }

    /// Rows in `range` (clamped), taking the lock once.
    pub fn rows(&self, range: Range<usize>) -> Vec<RowEntry> {
        let result = self.0.read();
        let end = range.end.min(result.lines.len());
        (range.start.min(end)..end)
            .filter_map(|r| result.get(r))
            .collect()
    }

    /// Whether the result contains context lines (and so gaps between groups
    /// are meaningful).
    pub fn has_context(&self) -> bool {
        self.0.has_context
    }
}

/// A running or finished filter scan over a [`LogFile`].
pub struct FilterJob {
    rows: FilterRows,
    started: Instant,
}

impl FilterJob {
    /// Starts filtering `file` with `filter` on a background thread.
    pub fn start(
        file: LogFile,
        filter: Arc<CompiledFilter>,
        context_before: usize,
        context_after: usize,
        records: Option<Records>,
        notify: Notify,
    ) -> Self {
        let shared = Arc::new(JobShared {
            result: RwLock::new(FilterResult::default()),
            cancel: AtomicBool::new(false),
            caught_up: AtomicBool::new(false),
            done: AtomicBool::new(false),
            processed: AtomicUsize::new(0),
            elapsed_us: AtomicU64::new(0),
            has_context: context_before > 0 || context_after > 0,
        });
        let started = Instant::now();
        let worker_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("logview-filter".into())
            .spawn(move || {
                let expander = ContextExpander::new(context_before, context_after);
                match &records {
                    Some(records) => {
                        record_worker(&file, &filter, records, &worker_shared, expander, &notify)
                    }
                    None => filter_worker(&file, &filter, &worker_shared, expander, &notify),
                }
                worker_shared
                    .elapsed_us
                    .store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                worker_shared.done.store(true, Ordering::Release);
                notify();
            });
        if spawned.is_err() {
            // Could not start a thread: report an empty, finished result.
            shared.done.store(true, Ordering::Release);
        }
        Self {
            rows: FilterRows(shared),
            started,
        }
    }

    fn shared(&self) -> &JobShared {
        &self.rows.0
    }

    pub fn rows_handle(&self) -> FilterRows {
        self.rows.clone()
    }

    /// Whether lines are being scanned. A job on a followed file that has
    /// processed everything is not running (it wakes up for new lines).
    pub fn is_running(&self) -> bool {
        !self.shared().done.load(Ordering::Acquire)
            && !self.shared().caught_up.load(Ordering::Acquire)
    }

    /// Number of file lines already checked.
    pub fn processed_lines(&self) -> usize {
        self.shared().processed.load(Ordering::Relaxed)
    }

    pub fn elapsed(&self) -> Duration {
        if self.is_running() {
            self.started.elapsed()
        } else {
            Duration::from_micros(self.shared().elapsed_us.load(Ordering::Relaxed))
        }
    }

    /// Number of rows (matches + context lines) so far.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn match_count(&self) -> usize {
        self.shared().read().matches
    }

    pub fn has_context(&self) -> bool {
        self.rows.has_context()
    }

    pub fn get(&self, row: usize) -> Option<RowEntry> {
        self.rows.get(row)
    }

    pub fn rows(&self, range: Range<usize>) -> Vec<RowEntry> {
        self.rows.rows(range)
    }

    /// Index of the first row showing `line` or a later line.
    pub fn row_of_line(&self, line: usize) -> usize {
        self.shared().read().lines.lower_bound(line as u64)
    }

    pub fn memory_bytes(&self) -> usize {
        self.shared().read().memory_bytes()
    }
}

impl Drop for FilterJob {
    fn drop(&mut self) {
        self.shared().cancel.store(true, Ordering::Relaxed);
    }
}

fn filter_worker(
    file: &LogFile,
    filter: &CompiledFilter,
    shared: &JobShared,
    mut expander: ContextExpander,
    notify: &Notify,
) {
    let batch_lines = BLOCK_LINES * rayon::current_num_threads().max(1) * 4;
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let mut next = 0usize;
    loop {
        if cancelled() {
            return;
        }
        // Read `may_grow` before `line_count`: if no more lines can come,
        // the count read afterwards is final.
        let growing = file.may_grow();
        let available = file.line_count();
        if next >= available {
            if !growing {
                break;
            }
            let following = !file.is_indexing();
            if following && !shared.caught_up.swap(true, Ordering::AcqRel) {
                notify();
            }
            std::thread::sleep(if following {
                WAIT_FOR_GROWTH
            } else {
                WAIT_FOR_INDEX
            });
            continue;
        }
        shared.caught_up.store(false, Ordering::Release);

        let end = available.min(next + batch_lines);
        let block_starts: Vec<usize> = (next..end).step_by(BLOCK_LINES).collect();
        let matched: Vec<Vec<usize>> = block_starts
            .par_iter()
            .map(|&start| {
                if cancelled() {
                    return Vec::new();
                }
                file.lines(start..(start + BLOCK_LINES).min(end))
                    .into_iter()
                    .filter(|(_, bytes)| filter.matches(bytes))
                    .map(|(line, _)| line)
                    .collect()
            })
            .collect();
        if cancelled() {
            return;
        }

        {
            let mut result = shared.result.write().unwrap_or_else(|p| p.into_inner());
            for line in matched.into_iter().flatten() {
                expander.push_match(line, &mut result);
            }
            expander.flush(end, &mut result);
        }
        shared.processed.store(end, Ordering::Relaxed);
        next = end;
        notify();
    }
}

/// Filters by records (multi-line records and/or level/time). Every line of a
/// matching record is a match. Follows the parse job like `filter_worker`
/// follows the indexer.
fn record_worker(
    file: &LogFile,
    filter: &CompiledFilter,
    records: &Records,
    shared: &JobShared,
    mut expander: ContextExpander,
    notify: &Notify,
) {
    let batch = BLOCK_RECORDS * rayon::current_num_threads().max(1) * 4;
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let text = filter.has_text_rules();
    let mut next = 0usize;
    loop {
        if cancelled() {
            return;
        }
        let done = records.is_done();
        let complete = records.complete_records();
        if next >= complete {
            if done {
                break;
            }
            let following = records.is_caught_up();
            if following && !shared.caught_up.swap(true, Ordering::AcqRel) {
                notify();
            }
            std::thread::sleep(if following {
                WAIT_FOR_GROWTH
            } else {
                WAIT_FOR_INDEX
            });
            continue;
        }
        shared.caught_up.store(false, Ordering::Release);
        let end = complete.min(next + batch);
        let blocks: Vec<usize> = (next..end).step_by(BLOCK_RECORDS).collect();
        let matched: Vec<Vec<Range<usize>>> = blocks
            .par_iter()
            .map(|&start| {
                if cancelled() {
                    return Vec::new();
                }
                let recs = records.records(start..(start + BLOCK_RECORDS).min(end));
                let (Some(first), Some(last)) = (recs.first(), recs.last()) else {
                    return Vec::new();
                };
                // Fetch the lines of the whole block once.
                let base = first.lines.start;
                let lines = if text {
                    file.lines(base..last.lines.end)
                } else {
                    Vec::new()
                };
                recs.into_iter()
                    .filter(|r| filter.matches_meta(r.ts, r.level))
                    .filter(|r| {
                        !text || {
                            let from = (r.lines.start - base).min(lines.len());
                            let to = (r.lines.end - base).min(lines.len());
                            filter.matches_record(lines[from..to].iter().map(|(_, b)| *b))
                        }
                    })
                    .map(|r| r.lines)
                    .collect()
            })
            .collect();
        if cancelled() {
            return;
        }
        let lines_end = records.record(end - 1).map_or(0, |r| r.lines.end);
        {
            let mut result = shared.result.write().unwrap_or_else(|p| p.into_inner());
            for range in matched.into_iter().flatten() {
                for line in range {
                    expander.push_match(line, &mut result);
                }
            }
            expander.flush(lines_end, &mut result);
        }
        shared.processed.store(lines_end, Ordering::Relaxed);
        next = end;
        notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{FilterRule, FilterSpec, IncludeMode};
    use std::io::Write;

    fn expand(matches: &[usize], before: usize, after: usize, total: usize) -> Vec<(usize, bool)> {
        let mut out = FilterResult::default();
        let mut e = ContextExpander::new(before, after);
        for &m in matches {
            e.push_match(m, &mut out);
        }
        e.flush(total, &mut out);
        (0..out.lines.len())
            .map(|r| {
                let entry = out.get(r).unwrap();
                (entry.line, entry.is_match)
            })
            .collect()
    }

    /// Reference: mark lines within the context window of any match.
    fn naive(matches: &[usize], before: usize, after: usize, total: usize) -> Vec<(usize, bool)> {
        (0..total)
            .filter_map(|l| {
                if matches.contains(&l) {
                    Some((l, true))
                } else if matches
                    .iter()
                    .any(|&m| l + before >= m && l < m || l > m && l <= m + after)
                {
                    Some((l, false))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn no_context_emits_only_matches() {
        assert_eq!(expand(&[2, 5], 0, 0, 10), vec![(2, true), (5, true)]);
    }

    #[test]
    fn context_is_clamped_to_file() {
        assert_eq!(
            expand(&[0, 9], 2, 2, 10),
            vec![
                (0, true),
                (1, false),
                (2, false),
                (7, false),
                (8, false),
                (9, true)
            ]
        );
    }

    #[test]
    fn overlapping_context_is_not_duplicated() {
        for (matches, before, after) in [
            (vec![3, 5], 2, 2),
            (vec![3, 4, 5], 1, 1),
            (vec![1, 10, 12, 30], 3, 5),
            (vec![0, 2, 4, 6, 8], 0, 1),
            (vec![5, 20], 0, 3),
            (vec![5, 20], 4, 0),
        ] {
            assert_eq!(
                expand(&matches, before, after, 40),
                naive(&matches, before, after, 40),
                "{matches:?} -{before} +{after}"
            );
        }
    }

    #[test]
    fn partial_flush_then_more_matches() {
        let mut out = FilterResult::default();
        let mut e = ContextExpander::new(1, 3);
        e.push_match(2, &mut out);
        e.flush(4, &mut out); // batch ended at line 4: after-context 3 only
        e.push_match(5, &mut out); // 4 is after-context of 2 and before-context of 5
        e.flush(100, &mut out);
        let lines: Vec<usize> = (0..out.lines.len())
            .map(|r| out.get(r).unwrap().line)
            .collect();
        assert_eq!(lines, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out.matches, 2);
    }

    fn run(content: &str, spec: FilterSpec) -> Vec<(usize, bool)> {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let filter = Arc::new(spec.compile().unwrap());
        let job = FilterJob::start(
            file,
            filter,
            spec.context_before,
            spec.context_after,
            None,
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.is_running() {
            assert!(Instant::now() < deadline, "filter timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        job.rows(0..usize::MAX)
            .into_iter()
            .map(|r| (r.line, r.is_match))
            .collect()
    }

    #[test]
    fn job_filters_file() {
        let content = "INFO a\nERROR b\nINFO c\nWARN d\nERROR e";
        let spec = FilterSpec {
            rules: vec![FilterRule::include("error")],
            ..Default::default()
        };
        assert_eq!(run(content, spec), vec![(1, true), (4, true)]);
    }

    #[test]
    fn job_with_context_and_exclude() {
        let content = "0\n1 x\n2\n3\n4 x skip\n5\n6 x\n7\n";
        let spec = FilterSpec {
            rules: vec![FilterRule::include("x"), FilterRule::exclude("skip")],
            mode: IncludeMode::All,
            context_before: 1,
            context_after: 1,
            ..Default::default()
        };
        assert_eq!(
            run(content, spec),
            vec![
                (0, false),
                (1, true),
                (2, false),
                (5, false),
                (6, true),
                (7, false)
            ]
        );
    }

    #[test]
    fn job_handles_many_blocks() {
        let content: String = (0..BLOCK_LINES * 5 + 3)
            .map(|i| {
                if i % 1000 == 0 {
                    format!("hit {i}\n")
                } else {
                    format!("miss {i}\n")
                }
            })
            .collect();
        let spec = FilterSpec {
            rules: vec![FilterRule::include("hit")],
            ..Default::default()
        };
        let rows = run(&content, spec);
        let expected: Vec<(usize, bool)> = (0..BLOCK_LINES * 5 + 3)
            .filter(|i| i % 1000 == 0)
            .map(|i| (i, true))
            .collect();
        assert_eq!(rows, expected);
    }

    #[test]
    fn row_of_line_finds_next_row() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"a\nx\nb\nx\nc\n").unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let spec = FilterSpec {
            rules: vec![FilterRule::include("x")],
            ..Default::default()
        };
        let job = FilterJob::start(
            file,
            Arc::new(spec.compile().unwrap()),
            0,
            0,
            None,
            Arc::new(|| {}),
        );
        while job.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(job.row_of_line(0), 0);
        assert_eq!(job.row_of_line(1), 0);
        assert_eq!(job.row_of_line(2), 1);
        assert_eq!(job.row_of_line(4), 2);
        assert_eq!(job.match_count(), 2);
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use crate::filter::{FilterRule, FilterSpec};
    use crate::format::LogFormat;
    use crate::level::{Level, LevelSet};
    use crate::records::ParseJob;
    use std::io::Write;

    const LOG: &str = "\
2026-01-01 00:00:00.000 INFO  [w] app - start
2026-01-01 00:00:10.000 ERROR [w] app - boom
\tat com.example.Db.query(Db.java:10)
\tat com.example.Main.run(Main.java:5)
2026-01-01 00:01:00.000 WARN  [w] app - slow Db
2026-01-01 00:02:00.000 DEBUG [w] app - tick
";

    fn run(spec: FilterSpec, multiline: bool) -> Vec<(usize, bool)> {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(LOG.as_bytes()).unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let sample: Vec<&[u8]> = LOG.lines().map(str::as_bytes).collect();
        let format = Arc::new(LogFormat::builtins()[0].compile(&sample).unwrap());
        let parse = ParseJob::start(file.clone(), format, multiline, Arc::new(|| {}));
        let job = FilterJob::start(
            file,
            Arc::new(spec.compile().unwrap()),
            spec.context_before,
            spec.context_after,
            Some(parse.records()),
            Arc::new(|| {}),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.is_running() {
            assert!(Instant::now() < deadline, "filter timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        job.rows(0..usize::MAX)
            .into_iter()
            .map(|r| (r.line, r.is_match))
            .collect()
    }

    #[test]
    fn match_in_stack_trace_returns_whole_record() {
        let spec = FilterSpec {
            rules: vec![FilterRule::include("Main.run")],
            ..Default::default()
        };
        assert_eq!(
            run(spec.clone(), true),
            vec![(1, true), (2, true), (3, true)]
        );
        // Without multiline only the matching line itself.
        assert_eq!(run(spec, false), vec![(3, true)]);
    }

    #[test]
    fn level_filter_keeps_records() {
        let mut levels = LevelSet(0);
        levels.set(Some(Level::Error), true);
        levels.set(Some(Level::Warn), true);
        let spec = FilterSpec {
            levels,
            ..Default::default()
        };
        assert_eq!(
            run(spec, true),
            vec![(1, true), (2, true), (3, true), (4, true)]
        );
    }

    #[test]
    fn time_range_and_text() {
        let spec = FilterSpec {
            rules: vec![FilterRule::include("db")],
            time_from: Some(1_767_225_600 + 30),
            ..Default::default()
        };
        assert_eq!(run(spec, true), vec![(4, true)]);
    }
}

#[cfg(test)]
mod follow_tests {
    use super::*;
    use crate::filter::{FilterRule, FilterSpec};
    use std::io::Write;

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn filter_follows_appended_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.log");
        std::fs::write(&path, b"ERROR one\nINFO two\n").unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        file.start_follow(Arc::new(|| {}));
        let spec = FilterSpec {
            rules: vec![FilterRule::include("error")],
            ..Default::default()
        };
        let job = FilterJob::start(
            file.clone(),
            Arc::new(spec.compile().unwrap()),
            0,
            0,
            None,
            Arc::new(|| {}),
        );
        wait_until("caught up", || !job.is_running());
        assert_eq!(job.match_count(), 1);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"INFO three\nERROR four\n").unwrap();
        drop(f);
        wait_until("new match", || job.match_count() == 2);
        assert_eq!(job.get(1).map(|e| e.line), Some(3));
        wait_until("caught up again", || !job.is_running());

        file.stop_follow();
        wait_until("finished", || job.shared().done.load(Ordering::Acquire));
    }
}
