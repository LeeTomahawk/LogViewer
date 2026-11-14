//! Writing filter results to a file or a string.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::file::{LogFile, Notify};
use crate::filter_job::FilterRows;

/// Rows fetched per lock of the result.
const ROWS_PER_FETCH: usize = 64 * 1024;
/// Group separator written between non-adjacent rows when context is shown
/// (same as `grep -C`).
const SEPARATOR: &[u8] = b"--\n";

/// Writes `rows` of `file` to `out`, one line per row, with `--` between
/// groups if the result has context lines. `on_progress` is called with the
/// rows and bytes written so far and may return `false` to stop.
///
/// Returns the number of rows written.
fn write_rows<W: Write>(
    file: &LogFile,
    rows: &FilterRows,
    out: &mut W,
    mut on_progress: impl FnMut(usize, u64) -> bool,
) -> io::Result<usize> {
    let separators = rows.has_context();
    let total = rows.len();
    let mut written = 0usize;
    let mut bytes = 0u64;
    let mut prev_line: Option<usize> = None;
    let mut start = 0;
    while start < total {
        for entry in rows.rows(start..start + ROWS_PER_FETCH) {
            if separators && prev_line.is_some_and(|p| p + 1 != entry.line) {
                out.write_all(SEPARATOR)?;
                bytes += SEPARATOR.len() as u64;
            }
            let line = file.line(entry.line).unwrap_or_default();
            out.write_all(line)?;
            out.write_all(b"\n")?;
            bytes += line.len() as u64 + 1;
            prev_line = Some(entry.line);
            written += 1;
        }
        start += ROWS_PER_FETCH;
        if !on_progress(written.min(total), bytes) {
            break;
        }
    }
    Ok(written)
}

/// Collects the rows as text, stopping once `limit_bytes` would be exceeded.
/// Returns the text, the number of rows included and whether all rows fit.
pub fn rows_to_string(
    file: &LogFile,
    rows: &FilterRows,
    limit_bytes: usize,
) -> (String, usize, bool) {
    let separators = rows.has_context();
    let mut out = Vec::new();
    let mut count = 0;
    let mut prev_line: Option<usize> = None;
    let total = rows.len();
    let mut start = 0;
    while start < total {
        for entry in rows.rows(start..start + ROWS_PER_FETCH) {
            let line = file.line(entry.line).unwrap_or_default();
            let sep = separators && prev_line.is_some_and(|p| p + 1 != entry.line);
            let needed = line.len() + 1 + if sep { SEPARATOR.len() } else { 0 };
            if out.len() + needed > limit_bytes {
                return (String::from_utf8_lossy(&out).into_owned(), count, false);
            }
            if sep {
                out.extend_from_slice(SEPARATOR);
            }
            out.extend_from_slice(line);
            out.push(b'\n');
            prev_line = Some(entry.line);
            count += 1;
        }
        start += ROWS_PER_FETCH;
    }
    (String::from_utf8_lossy(&out).into_owned(), count, true)
}

struct ExportShared {
    cancel: AtomicBool,
    done: AtomicBool,
    rows_written: AtomicUsize,
    bytes_written: AtomicU64,
    error: Mutex<Option<String>>,
}

/// Exports a filter result to a file on a background thread. Lines are written
/// as raw bytes (the original encoding is preserved). A cancelled or failed
/// export removes the partial file.
pub struct ExportJob {
    shared: Arc<ExportShared>,
    path: PathBuf,
    total_rows: usize,
}

impl ExportJob {
    /// The filter should be finished: rows added after the start are not
    /// exported.
    pub fn start(file: LogFile, rows: FilterRows, path: &Path, notify: Notify) -> Self {
        let shared = Arc::new(ExportShared {
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            rows_written: AtomicUsize::new(0),
            bytes_written: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let total_rows = rows.len();
        let worker = Arc::clone(&shared);
        let worker_path = path.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("logview-export".into())
            .spawn(move || {
                let result = export(&file, &rows, &worker_path, &worker, &notify);
                let cancelled = worker.cancel.load(Ordering::Relaxed);
                if result.is_err() || cancelled {
                    let _ = std::fs::remove_file(&worker_path);
                }
                if let Err(e) = result {
                    *worker.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                }
                worker.done.store(true, Ordering::Release);
                notify();
            });
        if let Err(e) = spawned {
            *shared.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
            shared.done.store(true, Ordering::Release);
        }
        Self {
            shared,
            path: path.to_path_buf(),
            total_rows,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_running(&self) -> bool {
        !self.shared.done.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    pub fn was_cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    pub fn rows_written(&self) -> usize {
        self.shared.rows_written.load(Ordering::Relaxed)
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Relaxed)
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    pub fn progress(&self) -> f32 {
        if self.total_rows == 0 {
            1.0
        } else {
            self.rows_written() as f32 / self.total_rows as f32
        }
    }

    pub fn error(&self) -> Option<String> {
        self.shared
            .error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

fn export(
    file: &LogFile,
    rows: &FilterRows,
    path: &Path,
    shared: &ExportShared,
    notify: &Notify,
) -> io::Result<()> {
    let mut out = BufWriter::with_capacity(4 << 20, File::create(path)?);
    write_rows(file, rows, &mut out, |written, bytes| {
        shared.rows_written.store(written, Ordering::Relaxed);
        shared.bytes_written.store(bytes, Ordering::Relaxed);
        notify();
        !shared.cancel.load(Ordering::Relaxed)
    })?;
    out.flush()?;
    // Make sure the data reached the OS before reporting success.
    out.get_ref().sync_data()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{FilterRule, FilterSpec};
    use crate::filter_job::FilterJob;
    use std::time::{Duration, Instant};

    fn job(
        content: &[u8],
        pattern: &str,
        before: usize,
        after: usize,
    ) -> (tempfile::NamedTempFile, LogFile, FilterJob) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(content).unwrap();
        tmp.flush().unwrap();
        let file = LogFile::open(tmp.path(), Arc::new(|| {})).unwrap();
        let spec = FilterSpec {
            rules: vec![FilterRule::include(pattern)],
            ..Default::default()
        };
        let job = FilterJob::start(
            file.clone(),
            Arc::new(spec.compile().unwrap()),
            before,
            after,
            None,
            Arc::new(|| {}),
        );
        while job.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }
        (tmp, file, job)
    }

    #[test]
    fn string_without_context() {
        let (_t, file, job) = job(b"a x\nb\nc x\r\n", "x", 0, 0);
        let (text, count, complete) = rows_to_string(&file, &job.rows_handle(), 1 << 20);
        assert_eq!(text, "a x\nc x\n");
        assert_eq!((count, complete), (2, true));
    }

    #[test]
    fn string_with_separators() {
        let (_t, file, job) = job(b"x0\n1\n2\n3\nx4\n5\n", "x", 0, 1);
        let (text, _, _) = rows_to_string(&file, &job.rows_handle(), 1 << 20);
        assert_eq!(text, "x0\n1\n--\nx4\n5\n");
    }

    #[test]
    fn string_respects_limit() {
        let (_t, file, job) = job(b"x1\nx2\nx3\n", "x", 0, 0);
        let (text, count, complete) = rows_to_string(&file, &job.rows_handle(), 7);
        assert_eq!(text, "x1\nx2\n");
        assert_eq!((count, complete), (2, false));
    }

    #[test]
    fn export_to_file_keeps_raw_bytes() {
        let (_t, file, job) = job(b"x \xFF\nno\nx2\n", "x", 0, 0);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.log");
        let export = ExportJob::start(file, job.rows_handle(), &out, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while export.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(export.error(), None);
        assert_eq!(export.rows_written(), 2);
        assert_eq!(std::fs::read(&out).unwrap(), b"x \xFF\nx2\n");
    }

    #[test]
    fn export_error_is_reported() {
        let (_t, file, job) = job(b"x\n", "x", 0, 0);
        let bad = Path::new("definitely/missing/dir/out.log");
        let export = ExportJob::start(file, job.rows_handle(), bad, Arc::new(|| {}));
        while export.is_running() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(export.error().is_some());
    }
}
