//! Records and their metadata, computed in the background.
//!
//! A [`ParseJob`] parses every line with a [`CompiledFormat`]. With
//! *multiline* on, a line that is not a record header (e.g. a stack trace
//! line) belongs to the previous record; otherwise every line is a record.
//!
//! Per record it stores the first line (in a [`CompactU64`], only for
//! multiline) and a packed `u32` with the level (3 bits) and the timestamp
//! in seconds (29 bits, relative to a base ≈ 8 years before the first
//! timestamp, so ±8 years around it are representable). That is 4–8 bytes per
//! record.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::compact::CompactU64;
use crate::file::{LogFile, Notify};
use crate::format::{CompiledFormat, LineMeta};
use crate::level::Level;

const BLOCK_LINES: usize = 16 * 1024;
const WAIT_FOR_INDEX: Duration = Duration::from_millis(5);
const WAIT_FOR_GROWTH: Duration = Duration::from_millis(50);

const TS_BITS: u32 = 29;
const TS_MASK: u32 = (1 << TS_BITS) - 1;
/// Offset of the first timestamp from the base.
const TS_HALF: i64 = 1 << (TS_BITS - 1);

#[derive(Debug, Default)]
struct RecordIndex {
    /// First line of each record (multiline only).
    starts: CompactU64,
    /// Packed level + timestamp per record.
    meta: Vec<u32>,
    /// Seconds value of packed timestamp 0.
    base: Option<i64>,
    min_secs: Option<i64>,
    max_secs: Option<i64>,
    /// Records per level code (0 = no level).
    level_counts: [usize; 7],
}

impl RecordIndex {
    fn push(&mut self, line: usize, meta: LineMeta, multiline: bool) {
        if multiline {
            self.starts.push(line as u64);
        }
        let level = meta.level.map_or(0, Level::code);
        self.level_counts[level as usize] += 1;
        let ts = match meta.ts {
            Some(ms) => {
                let secs = ms.div_euclid(1000);
                self.min_secs = Some(self.min_secs.map_or(secs, |m| m.min(secs)));
                self.max_secs = Some(self.max_secs.map_or(secs, |m| m.max(secs)));
                let base = *self.base.get_or_insert(secs - TS_HALF);
                let offset = secs - base;
                if (1..=TS_MASK as i64).contains(&offset) {
                    offset as u32
                } else {
                    0
                }
            }
            None => 0,
        };
        self.meta.push(u32::from(level) << TS_BITS | ts);
    }

    fn unpack(&self, packed: u32) -> (Option<i64>, Option<Level>) {
        let ts = packed & TS_MASK;
        let ts = (ts != 0).then(|| self.base.unwrap_or(0) + ts as i64);
        (ts, Level::from_code((packed >> TS_BITS) as u8))
    }
}

struct Shared {
    index: RwLock<RecordIndex>,
    cancel: AtomicBool,
    done: AtomicBool,
    /// All lines so far are parsed and the file is only followed.
    caught_up: AtomicBool,
    processed: AtomicUsize,
    elapsed_us: AtomicU64,
    multiline: bool,
}

/// One record: its lines and metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordInfo {
    pub lines: Range<usize>,
    /// Timestamp in seconds.
    pub ts: Option<i64>,
    pub level: Option<Level>,
}

/// Read access to the records of a [`ParseJob`]; cheap to clone.
#[derive(Clone)]
pub struct Records(Arc<Shared>);

impl Records {
    fn read(&self) -> RwLockReadGuard<'_, RecordIndex> {
        self.0.index.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn multiline(&self) -> bool {
        self.0.multiline
    }

    pub fn is_done(&self) -> bool {
        self.0.done.load(Ordering::Acquire)
    }

    /// Parsed everything so far; waiting for a followed file to grow.
    pub fn is_caught_up(&self) -> bool {
        self.0.caught_up.load(Ordering::Acquire)
    }

    /// Lines parsed so far.
    pub fn processed_lines(&self) -> usize {
        self.0.processed.load(Ordering::Acquire)
    }

    pub fn record_count(&self) -> usize {
        self.read().meta.len()
    }

    /// Records whose line range is final (the last record may still grow
    /// while parsing runs).
    pub fn complete_records(&self) -> usize {
        // Read `done` first: if parsing was finished, the count is final.
        let done = self.is_done();
        let count = self.record_count();
        if done || !self.multiline() {
            count
        } else {
            count.saturating_sub(1)
        }
    }

    /// Record containing `line` (lines before the first record map to 0).
    pub fn record_of_line(&self, line: usize) -> usize {
        if self.multiline() {
            self.read()
                .starts
                .lower_bound(line as u64 + 1)
                .saturating_sub(1)
        } else {
            line
        }
    }

    /// Records in `range` (clamped to complete records), under one lock.
    pub fn records(&self, range: Range<usize>) -> Vec<RecordInfo> {
        let done = self.is_done();
        let processed = self.processed_lines();
        let index = self.read();
        let count = index.meta.len();
        let complete = if done || !self.multiline() {
            count
        } else {
            count.saturating_sub(1)
        };
        let end = range.end.min(complete);
        (range.start.min(end)..end)
            .map(|r| {
                let (ts, level) = index.unpack(index.meta[r]);
                let lines = if self.multiline() {
                    let start = index.starts.get(r).unwrap_or_default() as usize;
                    let end = index.starts.get(r + 1).map_or(processed, |s| s as usize);
                    start..end
                } else {
                    r..r + 1
                };
                RecordInfo { lines, ts, level }
            })
            .collect()
    }

    pub fn record(&self, r: usize) -> Option<RecordInfo> {
        self.records(r..r + 1).pop()
    }

    /// Earliest and latest timestamp (seconds) seen so far.
    pub fn time_bounds(&self) -> Option<(i64, i64)> {
        let index = self.read();
        index.min_secs.zip(index.max_secs)
    }

    /// The first record at or after `secs`, or the last record with a
    /// timestamp if all are earlier. A linear scan (≈20 ms for 20 M records),
    /// so it also works when timestamps are not in order.
    pub fn find_time(&self, secs: i64) -> Option<usize> {
        let index = self.read();
        let mut last_timed = None;
        for (r, &packed) in index.meta.iter().enumerate() {
            if let (Some(ts), _) = index.unpack(packed) {
                if ts >= secs {
                    return Some(r);
                }
                last_timed = Some(r);
            }
        }
        last_timed
    }

    /// Timestamp (seconds) of the record containing `line`.
    pub fn time_of_line(&self, line: usize) -> Option<i64> {
        let r = self.record_of_line(line);
        let index = self.read();
        index.meta.get(r).and_then(|&p| index.unpack(p).0)
    }

    /// Records per level: index 0 = no level, then [`Level::code`].
    pub fn level_counts(&self) -> [usize; 7] {
        self.read().level_counts
    }

    pub fn memory_bytes(&self) -> usize {
        let index = self.read();
        index.starts.memory_bytes() + index.meta.capacity() * size_of::<u32>()
    }
}

/// Background parsing of all lines of a file. Dropping it cancels parsing.
pub struct ParseJob {
    records: Records,
    started: Instant,
}

impl ParseJob {
    pub fn start(
        file: LogFile,
        format: Arc<CompiledFormat>,
        multiline: bool,
        notify: Notify,
    ) -> Self {
        // Plain text has no continuation lines.
        let multiline = multiline && !format.is_plain();
        let shared = Arc::new(Shared {
            index: RwLock::new(RecordIndex::default()),
            cancel: AtomicBool::new(false),
            caught_up: AtomicBool::new(false),
            done: AtomicBool::new(false),
            processed: AtomicUsize::new(0),
            elapsed_us: AtomicU64::new(0),
            multiline,
        });
        let started = Instant::now();
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("logview-parse".into())
            .spawn(move || {
                parse_worker(&file, &format, &worker, &notify);
                shrink(&worker);
                worker
                    .elapsed_us
                    .store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                worker.done.store(true, Ordering::Release);
                notify();
            });
        if spawned.is_err() {
            shared.done.store(true, Ordering::Release);
        }
        Self {
            records: Records(shared),
            started,
        }
    }

    pub fn records(&self) -> Records {
        self.records.clone()
    }

    /// Whether lines are being parsed (false once a followed file is
    /// caught up).
    pub fn is_running(&self) -> bool {
        !self.records.is_done() && !self.records.is_caught_up()
    }

    pub fn elapsed(&self) -> Duration {
        if self.is_running() {
            self.started.elapsed()
        } else {
            Duration::from_micros(self.records.0.elapsed_us.load(Ordering::Relaxed))
        }
    }
}

impl Drop for ParseJob {
    fn drop(&mut self) {
        self.records.0.cancel.store(true, Ordering::Relaxed);
    }
}

fn parse_worker(file: &LogFile, format: &CompiledFormat, shared: &Shared, notify: &Notify) {
    let batch = BLOCK_LINES * rayon::current_num_threads().max(1) * 4;
    let cancelled = || shared.cancel.load(Ordering::Relaxed);
    let mut next = 0usize;
    loop {
        if cancelled() {
            return;
        }
        let growing = file.may_grow();
        let available = file.line_count();
        if next >= available {
            if !growing {
                return;
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
        let end = available.min(next + batch);
        let blocks: Vec<usize> = (next..end).step_by(BLOCK_LINES).collect();
        let parsed: Vec<(usize, Vec<Option<LineMeta>>)> = blocks
            .par_iter()
            .map(|&start| {
                if cancelled() {
                    return (start, Vec::new());
                }
                let metas = file
                    .lines(start..(start + BLOCK_LINES).min(end))
                    .into_iter()
                    .map(|(_, bytes)| format.meta(bytes))
                    .collect();
                (start, metas)
            })
            .collect();
        if cancelled() {
            return;
        }
        {
            let mut index = shared.index.write().unwrap_or_else(|p| p.into_inner());
            for (start, metas) in parsed {
                for (i, meta) in metas.into_iter().enumerate() {
                    let line = start + i;
                    match meta {
                        Some(meta) => index.push(line, meta, shared.multiline),
                        // A continuation line before any header still needs a
                        // record; without multiline every line is a record.
                        None if line == 0 || !shared.multiline => {
                            index.push(line, LineMeta::default(), shared.multiline)
                        }
                        None => {}
                    }
                }
            }
        }
        shared.processed.store(end, Ordering::Release);
        next = end;
        notify();
    }
}

/// Releases spare `Vec` capacity once parsing is complete.
fn shrink(shared: &Shared) {
    let mut index = shared.index.write().unwrap_or_else(|p| p.into_inner());
    index.starts.shrink_to_fit();
    index.meta.shrink_to_fit();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::LogFormat;
    use std::io::Write;

    const LOG: &str = "\
preamble without header
2026-01-01 00:00:00.000 INFO  [w] app - start
2026-01-01 00:00:10.000 ERROR [w] app - boom
\tat a.b(C.java:1)
\tat d.e(F.java:2)
2026-01-01 00:01:00.000 WARN  [w] app - end
";
    const T0: i64 = 1_767_225_600;

    fn run(multiline: bool) -> (tempfile::NamedTempFile, ParseJob) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(LOG.as_bytes()).unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let sample: Vec<&[u8]> = LOG.lines().map(str::as_bytes).collect();
        let format = LogFormat::builtins()[0].compile(&sample).unwrap();
        let job = ParseJob::start(file, Arc::new(format), multiline, Arc::new(|| {}));
        while job.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }
        (tmp, job)
    }

    #[test]
    fn multiline_groups_stack_traces() {
        let (_t, job) = run(true);
        let r = job.records();
        assert_eq!(r.record_count(), 4);
        let all = r.records(0..10);
        assert_eq!(
            all[0],
            RecordInfo {
                lines: 0..1,
                ts: None,
                level: None
            }
        );
        assert_eq!(
            all[1],
            RecordInfo {
                lines: 1..2,
                ts: Some(T0),
                level: Some(Level::Info)
            }
        );
        assert_eq!(
            all[2],
            RecordInfo {
                lines: 2..5,
                ts: Some(T0 + 10),
                level: Some(Level::Error)
            }
        );
        assert_eq!(all[3].lines, 5..6);
        assert_eq!(r.record_of_line(3), 2);
        assert_eq!(r.record_of_line(5), 3);
        assert_eq!(r.time_bounds(), Some((T0, T0 + 60)));
        assert_eq!(r.level_counts()[0], 1);
        assert_eq!(r.level_counts()[Level::Error.code() as usize], 1);
    }

    #[test]
    fn time_lookup() {
        let (_t, job) = run(true);
        let r = job.records();
        assert_eq!(
            r.find_time(T0 - 100),
            Some(1),
            "before the first: first timed record"
        );
        assert_eq!(r.find_time(T0 + 5), Some(2));
        assert_eq!(r.find_time(T0 + 10), Some(2));
        assert_eq!(r.find_time(T0 + 11), Some(3));
        assert_eq!(
            r.find_time(T0 + 999),
            Some(3),
            "after the last: last timed record"
        );
        assert_eq!(
            r.time_of_line(3),
            Some(T0 + 10),
            "stack trace line belongs to its record"
        );
        assert_eq!(r.time_of_line(0), None);
    }

    #[test]
    fn single_line_records() {
        let (_t, job) = run(false);
        let r = job.records();
        assert_eq!(r.record_count(), 6);
        assert_eq!(r.record_of_line(3), 3);
        let rec = r.record(3).unwrap();
        assert_eq!(
            rec,
            RecordInfo {
                lines: 3..4,
                ts: None,
                level: None
            }
        );
    }

    #[test]
    fn packing_out_of_range_timestamps() {
        let mut index = RecordIndex::default();
        let far = TS_HALF * 3;
        index.push(
            0,
            LineMeta {
                ts: Some(T0 * 1000),
                level: Some(Level::Debug),
            },
            false,
        );
        index.push(
            1,
            LineMeta {
                ts: Some((T0 + far) * 1000),
                level: None,
            },
            false,
        );
        assert_eq!(index.unpack(index.meta[0]), (Some(T0), Some(Level::Debug)));
        assert_eq!(
            index.unpack(index.meta[1]),
            (None, None),
            "outside packable range"
        );
        assert_eq!(index.max_secs, Some(T0 + far), "bounds still exact");
    }
}
