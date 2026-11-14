//! A log file: memory mapping plus a line index built in the background,
//! optionally followed as it grows (see [`crate::tail`]).

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, Instant};

use crate::index::{LineIndex, scan_newlines};
use crate::merge::{MergeSource, Merged};
use crate::source::MappedFile;
use crate::tail::{FileId, PREFIX_LEN, PollResult, Poller, TailStatus, TailStore};

/// How often a followed file is checked when no change notification arrives.
pub const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// Called from worker threads whenever new data is available (e.g. to request
/// a repaint of the UI). Must be cheap.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Bytes scanned by one parallel task.
const CHUNK_SIZE: usize = 8 << 20;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("nie można otworzyć pliku {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// State shared between the [`LogFile`] handle and its worker threads.
struct Shared {
    index: RwLock<LineIndex>,
    scanned: AtomicU64,
    cancel: AtomicBool,
    done: AtomicBool,
    elapsed_us: AtomicU64,
    tail: TailStore,
    /// Following requested.
    following: AtomicBool,
    /// A follow thread exists.
    follower_running: AtomicBool,
    status: AtomicU8,
    /// Identity of the file when it was opened.
    id: Option<FileId>,
}

/// An open log file. Lines become available while the index is being built.
///
/// The handle is cheap to clone (background jobs keep their own clone).
/// Dropping the last clone cancels indexing.
#[derive(Clone)]
pub struct LogFile {
    data: Arc<MappedFile>,
    shared: Arc<Shared>,
    started: Instant,
    _cancel_on_drop: Arc<CancelOnDrop>,
    /// Rows of several files merged by time (see [`crate::merge`]); the
    /// mapping is then empty and only carries the name.
    merged: Option<Arc<Merged>>,
}

/// Lets a worker of a merged file see that the file was dropped.
pub(crate) struct CancelToken(Arc<Shared>);

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Relaxed)
    }
}

/// Cancels indexing when the last [`LogFile`] clone goes away. The indexing
/// thread holds `Shared` directly, not this guard.
struct CancelOnDrop(Arc<Shared>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Relaxed);
    }
}

impl LogFile {
    /// Maps the file and starts indexing it on a background thread.
    pub fn open(path: &Path, notify: Notify) -> Result<Self, OpenError> {
        let data = MappedFile::open(path).map_err(|source| OpenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let id = std::fs::metadata(path).ok().map(|m| FileId::of(&m));
        Self::from_mapped(data, id, notify)
    }

    /// Opens a file converted by [`crate::prepare::PrepareJob`], shown under
    /// the name `display` (the original). The temporary file is deleted when
    /// the last clone is dropped. Converted files cannot be followed.
    pub fn open_prepared(
        prepared: &crate::prepare::Prepared,
        display: &Path,
        notify: Notify,
    ) -> Result<Self, OpenError> {
        let data = MappedFile::open_temp(&prepared.path, display, prepared.description.clone())
            .map_err(|source| OpenError::Io {
                path: display.to_path_buf(),
                source,
            })?;
        Self::from_mapped(data, None, notify)
    }

    /// What was done to the original file (decompression, conversion), if
    /// anything.
    pub fn conversion(&self) -> Option<&str> {
        self.data.conversion()
    }

    fn from_mapped(
        data: MappedFile,
        id: Option<FileId>,
        notify: Notify,
    ) -> Result<Self, OpenError> {
        let path = data.path().to_path_buf();
        let data = Arc::new(data);
        let shared = Arc::new(Shared {
            index: RwLock::new(LineIndex::new(data.len())),
            scanned: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            elapsed_us: AtomicU64::new(0),
            tail: TailStore::new(),
            following: AtomicBool::new(false),
            follower_running: AtomicBool::new(false),
            status: AtomicU8::new(TailStatus::Idle as u8),
            id,
        });
        let started = Instant::now();

        let worker_data = Arc::clone(&data);
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("logview-indexer".into())
            .spawn(move || index_worker(&worker_data, &worker_shared, started, &notify))
            .map_err(|source| OpenError::Io {
                path: path.clone(),
                source,
            })?;

        Ok(Self {
            data,
            _cancel_on_drop: Arc::new(CancelOnDrop(Arc::clone(&shared))),
            shared,
            started,
            merged: None,
        })
    }

    /// A file showing the rows of a merge; `path` is only a name.
    pub(crate) fn from_merged(merged: Arc<Merged>, path: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            index: RwLock::new(LineIndex::new(0)),
            scanned: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(true),
            elapsed_us: AtomicU64::new(0),
            tail: TailStore::new(),
            following: AtomicBool::new(false),
            follower_running: AtomicBool::new(false),
            status: AtomicU8::new(TailStatus::Idle as u8),
            id: None,
        });
        Self {
            data: Arc::new(MappedFile::empty(path)),
            _cancel_on_drop: Arc::new(CancelOnDrop(Arc::clone(&shared))),
            shared,
            started: Instant::now(),
            merged: Some(merged),
        }
    }

    pub(crate) fn cancel_flag(&self) -> CancelToken {
        CancelToken(Arc::clone(&self.shared))
    }

    /// The merged files, if this is a merged view.
    pub fn merged_sources(&self) -> Option<&[MergeSource]> {
        self.merged.as_deref().map(|m| m.sources.as_slice())
    }

    /// For a merged view: `(source index, line in the source)` of each row
    /// in `range`.
    pub fn origins(&self, range: Range<usize>) -> Option<Vec<(usize, usize)>> {
        self.merged.as_deref().map(|m| m.origins(range))
    }

    pub fn origin(&self, row: usize) -> Option<(usize, usize)> {
        self.merged.as_deref().and_then(|m| m.origin(row))
    }

    pub fn path(&self) -> &Path {
        self.data.path()
    }

    /// Size of the file as far as it was read (grows while following).
    pub fn len_bytes(&self) -> u64 {
        if let Some(m) = &self.merged {
            return m.sources.iter().map(|s| s.file.len_bytes()).sum();
        }
        self.data.len().max(self.shared.tail.consumed())
    }

    /// Number of lines available so far.
    pub fn line_count(&self) -> usize {
        if let Some(m) = &self.merged {
            return m.row_count();
        }
        self.read_index().line_count()
    }

    pub fn is_indexing(&self) -> bool {
        if let Some(m) = &self.merged {
            return !m.is_done();
        }
        !self.shared.done.load(Ordering::Acquire)
    }

    /// Whether more lines may still appear (indexing or following). Jobs wait
    /// for more lines while this is true.
    pub fn may_grow(&self) -> bool {
        self.is_indexing() || self.is_following()
    }

    // ---- live tail --------------------------------------------------------------

    /// Starts following the file: data appended later is indexed as it
    /// arrives (`notify` is called after each batch). A last line without
    /// `\n` is hidden until it is complete. Starts after indexing finishes.
    pub fn start_follow(&self, notify: Notify) {
        if self.merged.is_some() || self.data.conversion().is_some() {
            return; // a merged view is a snapshot
        }
        self.shared.following.store(true, Ordering::Release);
        self.shared
            .status
            .store(TailStatus::Following as u8, Ordering::Relaxed);
        if self
            .shared
            .follower_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // the running thread picks the request up
        }
        let data = Arc::clone(&self.data);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("logview-follow".into())
            .spawn(move || follow_worker(&data, &shared, &notify));
        if spawned.is_err() {
            self.shared.follower_running.store(false, Ordering::Release);
            self.shared.following.store(false, Ordering::Release);
        }
    }

    pub fn stop_follow(&self) {
        self.shared.following.store(false, Ordering::Release);
        if self.tail_status() == TailStatus::Following {
            self.shared
                .status
                .store(TailStatus::Idle as u8, Ordering::Relaxed);
        }
    }

    pub fn is_following(&self) -> bool {
        self.shared.following.load(Ordering::Acquire)
    }

    /// Whether following has started reading: from now on the index holds
    /// complete lines only and new lines are appended.
    pub fn is_tail_ready(&self) -> bool {
        self.shared.tail.base() != u64::MAX
    }

    pub fn tail_status(&self) -> TailStatus {
        TailStatus::from_u8(self.shared.status.load(Ordering::Relaxed))
    }

    /// Appended bytes held in memory.
    pub fn tail_memory(&self) -> u64 {
        self.shared.tail.bytes()
    }

    /// Lines appended since following started.
    pub fn appended_lines(&self) -> u64 {
        self.shared.tail.lines()
    }

    /// Bytes already scanned by the indexer.
    pub fn scanned_bytes(&self) -> u64 {
        self.shared.scanned.load(Ordering::Relaxed)
    }

    /// Indexing progress in `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        if let Some(m) = &self.merged {
            return m.progress();
        }
        let len = self.len_bytes();
        if len == 0 || !self.is_indexing() {
            1.0
        } else {
            self.scanned_bytes() as f32 / len as f32
        }
    }

    /// Time spent indexing (so far, or in total once finished).
    pub fn index_elapsed(&self) -> Duration {
        if let Some(m) = &self.merged {
            return m.elapsed();
        }
        if self.is_indexing() {
            self.started.elapsed()
        } else {
            Duration::from_micros(self.shared.elapsed_us.load(Ordering::Relaxed))
        }
    }

    /// Heap memory used by the line index.
    pub fn index_memory(&self) -> usize {
        if let Some(m) = &self.merged {
            return m.memory_bytes();
        }
        self.read_index().memory_bytes()
    }

    /// Content of `line` without its `\n` / `\r\n` terminator.
    pub fn line(&self, line: usize) -> Option<&[u8]> {
        if let Some(m) = &self.merged {
            return m.line(line);
        }
        let span = self.read_index().line_span(line)?;
        Some(self.slice(span))
    }

    /// Lines in `range` (clamped to the available lines), taking the index
    /// lock only once.
    pub fn lines(&self, range: Range<usize>) -> Vec<(usize, &[u8])> {
        if let Some(m) = &self.merged {
            return m.lines(range);
        }
        let spans: Vec<(usize, (u64, u64))> = {
            let index = self.read_index();
            let end = range.end.min(index.line_count());
            (range.start..end)
                .filter_map(|i| index.line_span(i).map(|s| (i, s)))
                .collect()
        };
        spans.into_iter().map(|(i, s)| (i, self.slice(s))).collect()
    }

    fn slice(&self, (start, end): (u64, u64)) -> &[u8] {
        // Every line lies entirely in the mapping or in one tail chunk.
        let bytes = match self.shared.tail.slice(start, end) {
            Some(b) => b,
            None => self
                .data
                .bytes()
                .get(start as usize..end as usize)
                .unwrap_or_default(),
        };
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        // A UTF-8 byte order mark is not part of the first line.
        if start == 0 {
            bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
        } else {
            bytes
        }
    }

    fn read_index(&self) -> std::sync::RwLockReadGuard<'_, LineIndex> {
        // A poisoned lock means the indexer panicked; the data written so far
        // is still consistent (only whole entries are pushed).
        self.shared
            .index
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn index_worker(data: &MappedFile, shared: &Shared, started: Instant, notify: &Notify) {
    let batch_chunks = rayon::current_num_threads() * 2;
    let finished = scan_newlines(
        data.bytes(),
        CHUNK_SIZE,
        batch_chunks,
        &shared.cancel,
        |batch, scanned| {
            {
                let mut index = shared
                    .index
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for starts in batch {
                    for s in starts {
                        index.push_start(s);
                    }
                }
            }
            shared.scanned.store(scanned, Ordering::Relaxed);
            notify();
        },
    );
    if finished {
        let mut index = shared
            .index
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        index.finish();
        // Following was requested while indexing: drop an incomplete last
        // line in the same step, so it never shows up only to vanish.
        if shared.following.load(Ordering::Acquire) {
            let bytes = data.bytes();
            let stable_end = memchr::memrchr(b'\n', bytes).map_or(0, |p| p as u64 + 1);
            index.make_growable(stable_end);
            shared.tail.init(stable_end);
        }
    }
    shared
        .elapsed_us
        .store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    shared.done.store(true, Ordering::Release);
    notify();
}

/// Follows the file while `following` is set; see [`crate::tail`].
fn follow_worker(data: &MappedFile, shared: &Shared, notify: &Notify) {
    loop {
        follow_loop(data, shared, notify);
        shared.follower_running.store(false, Ordering::Release);
        // Following may have been requested again just before we stopped.
        let again = shared.following.load(Ordering::Acquire)
            && !shared.cancel.load(Ordering::Relaxed)
            && shared
                .follower_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
        if !again {
            return;
        }
    }
}

fn follow_loop(data: &MappedFile, shared: &Shared, notify: &Notify) {
    let active =
        || shared.following.load(Ordering::Acquire) && !shared.cancel.load(Ordering::Relaxed);
    while active() && !shared.done.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !active() {
        return;
    }

    // First start: the index from now on holds complete lines only.
    if shared.tail.base() == u64::MAX {
        let bytes = data.bytes();
        let stable_end = memchr::memrchr(b'\n', bytes).map_or(0, |p| p as u64 + 1);
        shared
            .index
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .make_growable(stable_end);
        shared.tail.init(stable_end);
        notify();
    }
    let prefix = data.bytes()[..data.bytes().len().min(PREFIX_LEN)].to_vec();
    let mut poller = Poller::new(data.path(), shared.id, prefix, shared.tail.consumed());

    // Change notifications make updates immediate; polling is the fallback
    // (and the only source on file systems without notifications).
    let (tx, rx) = mpsc::channel();
    let _watcher = {
        use notify::Watcher;
        notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            let _ = tx.send(());
        })
        .ok()
        .and_then(|mut w| {
            w.watch(data.path(), notify::RecursiveMode::NonRecursive)
                .ok()
                .map(|_| w)
        })
    };

    let mut more = true;
    while active() {
        if !more {
            let _ = rx.recv_timeout(FOLLOW_POLL);
            while rx.try_recv().is_ok() {}
        }
        more = false;
        let status = match poller.poll() {
            PollResult::Unchanged => TailStatus::Following,
            PollResult::Appended { chunks, more: m } => {
                more = m;
                for (start, chunk) in chunks {
                    let lines = memchr::memchr_iter(b'\n', &chunk).count() as u64;
                    // Store first, then index: a line is only visible once
                    // its bytes are reachable.
                    let len = chunk.len() as u64;
                    shared.tail.push(start, chunk, lines);
                    if let Some(stored) = shared.tail.slice(start, start + len) {
                        shared
                            .index
                            .write()
                            .unwrap_or_else(|p| p.into_inner())
                            .append_complete(start, stored);
                    }
                }
                shared.tail.set_consumed(poller.consumed());
                notify();
                TailStatus::Following
            }
            PollResult::Missing => TailStatus::Missing,
            PollResult::Truncated => TailStatus::Truncated,
            PollResult::Replaced => TailStatus::Replaced,
        };
        let previous = shared.status.swap(status as u8, Ordering::Relaxed);
        if previous != status as u8 {
            notify();
        }
        if matches!(status, TailStatus::Truncated | TailStatus::Replaced) {
            // The data no longer matches; the owner reopens the file.
            shared.following.store(false, Ordering::Release);
            notify();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn open_wait(content: &[u8]) -> (tempfile::NamedTempFile, LogFile) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(content).unwrap();
        tmp.flush().unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while file.is_indexing() {
            assert!(Instant::now() < deadline, "indexing timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
        (tmp, file)
    }

    #[test]
    fn empty_file() {
        let (_tmp, file) = open_wait(b"");
        assert_eq!(file.line_count(), 0);
        assert_eq!(file.line(0), None);
        assert_eq!(file.progress(), 1.0);
    }

    #[test]
    fn strips_line_terminators() {
        let (_tmp, file) = open_wait(b"one\r\ntwo\n\nlast");
        let lines: Vec<&[u8]> = file.lines(0..10).into_iter().map(|(_, l)| l).collect();
        assert_eq!(lines, vec![&b"one"[..], b"two", b"", b"last"]);
    }

    #[test]
    fn lone_cr_inside_line_is_kept() {
        let (_tmp, file) = open_wait(b"a\rb\n");
        assert_eq!(file.line(0), Some(&b"a\rb"[..]));
    }

    #[test]
    fn missing_file_is_an_error() {
        let result = LogFile::open(Path::new("definitely/missing/file.log"), Arc::new(|| {}));
        assert!(result.is_err());
    }

    #[test]
    fn lines_range_is_clamped() {
        let (_tmp, file) = open_wait(b"a\nb\nc\n");
        let got: Vec<usize> = file.lines(1..100).into_iter().map(|(i, _)| i).collect();
        assert_eq!(got, vec![1, 2]);
        assert!(file.lines(5..10).is_empty());
    }
}

#[cfg(test)]
mod follow_tests {
    use super::*;
    use std::io::Write;

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn append(path: &Path, data: &[u8]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
    }

    fn texts(file: &LogFile) -> Vec<String> {
        file.lines(0..usize::MAX)
            .into_iter()
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .collect()
    }

    #[test]
    fn follows_appended_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.log");
        std::fs::write(&path, b"a\nb\npart").unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        wait_until("index", || !file.is_indexing());
        assert_eq!(file.line_count(), 3);

        file.start_follow(Arc::new(|| {}));
        assert!(file.may_grow());
        // The incomplete last line is hidden while following…
        wait_until("growable index", || file.line_count() == 2);
        append(&path, b"ial\nc\r\n");
        // …and comes back complete.
        wait_until("appended lines", || file.line_count() == 4);
        assert_eq!(texts(&file), ["a", "b", "partial", "c"]);
        assert_eq!(file.tail_status(), TailStatus::Following);
        assert_eq!(file.appended_lines(), 2);

        append(&path, b"d\n");
        wait_until("more lines", || file.line_count() == 5);
        assert_eq!(file.line(4), Some(&b"d"[..]));
        assert_eq!(file.len_bytes(), std::fs::metadata(&path).unwrap().len());

        file.stop_follow();
        assert!(!file.may_grow());
        assert_eq!(file.tail_status(), TailStatus::Idle);
    }

    #[test]
    fn follow_from_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        std::fs::write(&path, b"").unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        file.start_follow(Arc::new(|| {}));
        wait_until("follow start", || !file.is_indexing());
        append(&path, b"first\nsecond\n");
        wait_until("lines", || file.line_count() == 2);
        assert_eq!(texts(&file), ["first", "second"]);
    }

    #[test]
    fn replaced_file_stops_following() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rot.log");
        std::fs::write(&path, b"old 1\nold 2\n").unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        file.start_follow(Arc::new(|| {}));
        wait_until("index", || !file.is_indexing());
        // Rotation by rename + new file (allowed on Windows while mapped:
        // std opens files with FILE_SHARE_DELETE).
        std::fs::rename(&path, dir.path().join("rot.log.1")).unwrap();
        std::fs::write(&path, b"new content after rotation\n").unwrap();
        wait_until("rotation detected", || {
            file.tail_status() == TailStatus::Replaced
        });
        assert!(!file.is_following());
        assert_eq!(texts(&file), ["old 1", "old 2"], "old data stays readable");
    }
}

#[cfg(test)]
mod prepared_tests {
    use super::*;
    use crate::encoding::TextEncoding;
    use crate::prepare::{PrepareJob, plan};

    #[test]
    fn bom_is_not_part_of_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom.log");
        std::fs::write(&path, b"\xEF\xBB\xBFfirst\nsecond\n").unwrap();
        let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
        while file.is_indexing() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(file.line(0), Some(&b"first"[..]));
        assert_eq!(file.line(1), Some(&b"second"[..]));
    }

    #[test]
    fn converted_file_is_shown_under_original_name_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u16.log");
        let bytes: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("ąę\nb\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(&path, bytes).unwrap();
        let p = plan(&path, TextEncoding::Auto).unwrap();
        let job = PrepareJob::start(&path, p, Arc::new(|| {}));
        let prepared = loop {
            if let Some(r) = job.take_result() {
                break r.unwrap();
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let file = LogFile::open_prepared(&prepared, &path, Arc::new(|| {})).unwrap();
        while file.is_indexing() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(file.path(), path.as_path());
        assert_eq!(file.line(0), Some("ąę".as_bytes()));
        assert_eq!(file.conversion(), Some("konwersja z UTF-16 LE"));
        file.start_follow(Arc::new(|| {}));
        assert!(!file.is_following(), "converted files cannot be followed");
        drop(file);
        // The indexer thread may still hold the mapping for a moment.
        let deadline = Instant::now() + Duration::from_secs(5);
        while prepared.path.exists() {
            assert!(Instant::now() < deadline, "temporary file not removed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
