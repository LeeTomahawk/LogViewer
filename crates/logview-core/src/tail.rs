//! Live tail: data appended to a file after it was opened.
//!
//! The part of the file that existed when it was opened stays memory-mapped.
//! Appended data is read with ordinary file reads (a fresh handle on every
//! poll) into an append-only list of chunks that each hold whole lines, so
//! every line lies entirely in the mapping or in one chunk and can still be
//! handed out as `&[u8]` without copying. A trailing line without `\n` waits
//! in [`Poller`] until it is complete.
//!
//! Why not remap the file? On Windows a mapped file cannot be truncated by
//! another process, and mapping the growing region again and again would keep
//! that lock and cost address space. Reading the tail with `File` keeps the
//! mapping fixed; the price is that appended data lives in RAM
//! ([`TailStore::bytes`]), so the viewer reopens the file once that grows
//! large. Truncation of the *mapped* part is still impossible on Windows while
//! the file is open (programs rotating with "copytruncate" get an error);
//! rotation by renaming the file and creating a new one works and is detected.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Bytes at the start of the file compared on every poll to detect that the
/// path now refers to a different file.
pub const PREFIX_LEN: usize = 4096;
/// Maximum bytes read in one poll (the rest follows in the next one).
pub const MAX_READ_PER_POLL: u64 = 64 << 20;

/// State of following a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TailStatus {
    /// Not following.
    Idle = 0,
    Following = 1,
    /// The file became shorter than what was read.
    Truncated = 2,
    /// The path now refers to another file (rotation by rename).
    Replaced = 3,
    /// The file no longer exists (still polled; it may come back).
    Missing = 4,
}

impl TailStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Following,
            2 => Self::Truncated,
            3 => Self::Replaced,
            4 => Self::Missing,
            _ => Self::Idle,
        }
    }
}

/// Appended data: chunks of whole lines in file order.
pub(crate) struct TailStore {
    chunks: elsa::sync::FrozenVec<Box<[u8]>>,
    /// File offset of each chunk.
    starts: RwLock<Vec<u64>>,
    /// Lines starting at or after this offset are in chunks
    /// (`u64::MAX` until following starts).
    base: AtomicU64,
    bytes: AtomicU64,
    /// File offset up to which data was read (including an incomplete line).
    consumed: AtomicU64,
    lines: AtomicU64,
}

impl TailStore {
    pub fn new() -> Self {
        Self {
            chunks: elsa::sync::FrozenVec::new(),
            starts: RwLock::new(Vec::new()),
            base: AtomicU64::new(u64::MAX),
            bytes: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            lines: AtomicU64::new(0),
        }
    }

    pub fn base(&self) -> u64 {
        self.base.load(Ordering::Acquire)
    }

    pub fn init(&self, base: u64) {
        self.consumed.store(base, Ordering::Relaxed);
        self.base.store(base, Ordering::Release);
    }

    /// Bytes `[start, end)` if they lie in the appended data.
    pub fn slice(&self, start: u64, end: u64) -> Option<&[u8]> {
        if start < self.base() {
            return None;
        }
        let (i, chunk_start) = {
            let starts = self.starts.read().unwrap_or_else(|p| p.into_inner());
            let i = starts.partition_point(|&s| s <= start).checked_sub(1)?;
            (i, starts[i])
        };
        let chunk = self.chunks.get(i)?;
        chunk.get((start - chunk_start) as usize..(end - chunk_start) as usize)
    }

    /// Adds a chunk; must be called before its lines enter the index.
    pub fn push(&self, start: u64, chunk: Box<[u8]>, lines: u64) {
        let len = chunk.len() as u64;
        self.chunks.push(chunk);
        self.starts
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .push(start);
        self.bytes.fetch_add(len, Ordering::Relaxed);
        self.lines.fetch_add(lines, Ordering::Relaxed);
    }

    pub fn set_consumed(&self, offset: u64) {
        self.consumed.store(offset, Ordering::Relaxed);
    }

    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Relaxed)
    }

    /// Appended bytes held in memory.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Lines appended since following started.
    pub fn lines(&self) -> u64 {
        self.lines.load(Ordering::Relaxed)
    }
}

/// Identity of a file beyond its content, where the OS offers one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileId {
    created: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl FileId {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            created: meta.created().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        }
    }

    /// Whether `other` is certainly another file. Creation times are compared
    /// only when both are known (on Windows a file re-created under the same
    /// name within seconds may inherit the old creation time; the prefix
    /// check catches most of those cases).
    fn differs(&self, other: &Self) -> bool {
        #[cfg(unix)]
        if self.inode != other.inode {
            return true;
        }
        matches!((self.created, other.created), (Some(a), Some(b)) if a != b)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollResult {
    Unchanged,
    /// Chunks of whole lines `(file offset, bytes)`; `more` if data remains.
    Appended {
        chunks: Vec<(u64, Box<[u8]>)>,
        more: bool,
    },
    Truncated,
    Replaced,
    Missing,
}

/// Reads data appended to a file, a whole line at a time.
pub(crate) struct Poller {
    path: PathBuf,
    id: Option<FileId>,
    /// Known start of the file (up to [`PREFIX_LEN`] bytes).
    prefix: Vec<u8>,
    /// Offset up to which the file was read.
    consumed: u64,
    /// Bytes after the last `\n` (an incomplete line).
    pending: Vec<u8>,
}

impl Poller {
    /// Starts reading at `stable_end` (just past the last `\n` of the data
    /// already indexed). `prefix` is the start of that data.
    pub fn new(path: &Path, id: Option<FileId>, prefix: Vec<u8>, stable_end: u64) -> Self {
        Self {
            path: path.to_path_buf(),
            id,
            prefix,
            consumed: stable_end,
            pending: Vec::new(),
        }
    }

    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    pub fn poll(&mut self) -> PollResult {
        match self.try_poll() {
            Ok(result) => result,
            Err(e) if e.kind() == io::ErrorKind::NotFound => PollResult::Missing,
            // Transient errors (e.g. the writer holds an exclusive lock for a
            // moment): try again on the next poll.
            Err(_) => PollResult::Unchanged,
        }
    }

    fn try_poll(&mut self) -> io::Result<PollResult> {
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        if let Some(id) = &self.id
            && id.differs(&FileId::of(&meta))
        {
            return Ok(PollResult::Replaced);
        }
        if len < self.consumed {
            return Ok(PollResult::Truncated);
        }

        // Same content at the start? Also extends the known prefix.
        let head_len = (len as usize).min(PREFIX_LEN);
        let mut head = vec![0; head_len];
        file.read_exact(&mut head)?;
        if !head.starts_with(&self.prefix) {
            return Ok(PollResult::Replaced);
        }
        if head.len() > self.prefix.len() {
            self.prefix = head;
        }

        if len == self.consumed {
            return Ok(PollResult::Unchanged);
        }
        let to_read = (len - self.consumed).min(MAX_READ_PER_POLL);
        file.seek(SeekFrom::Start(self.consumed))?;
        let mut buf = vec![0; to_read as usize];
        file.read_exact(&mut buf)?;
        let pending_start = self.consumed - self.pending.len() as u64;
        self.consumed += to_read;
        self.pending.extend_from_slice(&buf);

        let mut chunks = Vec::new();
        if let Some(last_nl) = memchr::memrchr(b'\n', &self.pending) {
            let complete: Box<[u8]> = self.pending[..=last_nl].into();
            self.pending.drain(..=last_nl);
            chunks.push((pending_start, complete));
        }
        Ok(PollResult::Appended {
            chunks,
            more: self.consumed < len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &Path, data: &[u8]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(data).unwrap();
    }

    fn chunks(result: PollResult) -> Vec<(u64, Vec<u8>)> {
        match result {
            PollResult::Appended { chunks, .. } => {
                chunks.into_iter().map(|(o, c)| (o, c.into_vec())).collect()
            }
            other => panic!("expected data, got {other:?}"),
        }
    }

    #[test]
    fn reads_whole_lines_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log");
        std::fs::write(&path, b"one\ntw").unwrap();
        let mut p = Poller::new(&path, None, b"one\ntw".to_vec(), 4);
        // The incomplete "tw" from the start is read again from offset 4.
        assert_eq!(
            p.poll(),
            PollResult::Appended {
                chunks: vec![],
                more: false
            }
        );
        append(&path, b"o\nthr");
        assert_eq!(chunks(p.poll()), vec![(4, b"two\n".to_vec())]);
        assert_eq!(p.poll(), PollResult::Unchanged);
        append(&path, b"ee\n");
        assert_eq!(chunks(p.poll()), vec![(8, b"three\n".to_vec())]);
        assert_eq!(p.consumed(), 14);
    }

    #[test]
    fn detects_truncation_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.log");
        std::fs::write(&path, b"header 1\nline\n").unwrap();
        let mut p = Poller::new(&path, None, b"header 1\nline\n".to_vec(), 14);
        std::fs::write(&path, b"short\n").unwrap();
        assert_eq!(p.poll(), PollResult::Truncated);

        let mut p = Poller::new(&path, None, b"short\n".to_vec(), 6);
        std::fs::write(&path, b"other content, longer than before\n").unwrap();
        assert_eq!(p.poll(), PollResult::Replaced);

        std::fs::remove_file(&path).unwrap();
        assert_eq!(p.poll(), PollResult::Missing);
    }

    #[test]
    fn tail_store_slices() {
        let store = TailStore::new();
        assert_eq!(store.slice(0, 1), None);
        store.init(10);
        store.push(10, b"ab\n".to_vec().into_boxed_slice(), 1);
        store.push(13, b"cde\n".to_vec().into_boxed_slice(), 1);
        assert_eq!(store.slice(10, 13), Some(&b"ab\n"[..]));
        assert_eq!(store.slice(13, 17), Some(&b"cde\n"[..]));
        assert_eq!(store.slice(14, 16), Some(&b"de"[..]));
        assert_eq!(store.slice(5, 7), None, "before the tail");
        assert_eq!(store.bytes(), 7);
        assert_eq!(store.lines(), 2);
    }
}
