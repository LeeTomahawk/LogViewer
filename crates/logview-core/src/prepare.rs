//! Conversion of compressed and non-UTF-8 files into a temporary UTF-8 file.
//!
//! [`plan`] looks at the first bytes of a file: gzip / zstd are recognised by
//! their magic bytes (not the extension), UTF-16 by a BOM or the zero-byte
//! pattern. If anything needs converting, a [`PrepareJob`] streams the file
//! through the decompressor and the decoder into a file in [`temp_dir`]
//! (with progress and cancelling); the viewer then maps that file like any
//! other. The temporary file is deleted when the [`crate::LogFile`] using it
//! is closed (see [`crate::source::MappedFile`]), and leftovers of crashed
//! sessions are removed by [`clean_temp_dir`].

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::encoding::{Codec, TextEncoding};
use crate::file::Notify;

const BUFFER: usize = 1 << 20;

/// What has to be done before a file can be viewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub codec: Option<Codec>,
    /// Encoding of the (decompressed) text; `Auto` for compressed files is
    /// resolved from the decompressed data.
    pub encoding: TextEncoding,
}

impl Plan {
    pub fn needed(&self) -> bool {
        self.codec.is_some() || self.encoding.decoder_encoding().is_some()
    }

    /// E.g. "rozpakowanie (gzip), konwersja z UTF-16 LE".
    pub fn describe(&self, encoding: TextEncoding) -> String {
        let mut parts = Vec::new();
        if let Some(codec) = self.codec {
            parts.push(format!("rozpakowanie ({})", codec.name()));
        }
        if encoding.decoder_encoding().is_some() {
            parts.push(format!("konwersja z {}", encoding.short()));
        }
        parts.join(", ")
    }
}

/// Decides what `path` needs, reading its first bytes.
pub fn plan(path: &Path, choice: TextEncoding) -> io::Result<Plan> {
    let mut head = vec![0; 4096];
    let n = read_up_to(&mut File::open(path)?, &mut head)?;
    head.truncate(n);
    let codec = Codec::sniff(&head);
    let encoding = if codec.is_some() {
        choice
    } else {
        choice.resolve(&head)
    };
    Ok(Plan { codec, encoding })
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Folder for converted files.
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("logview")
}

/// Removes converted files older than `max_age` (left by sessions that did
/// not exit cleanly). Files still in use cannot be deleted on Windows and are
/// skipped.
pub fn clean_temp_dir(max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(temp_dir()) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn temp_path(source: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stem = source
        .file_name()
        .map(|n| n.to_string_lossy().replace(['.', ' '], "_"))
        .unwrap_or_else(|| "log".into());
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    temp_dir().join(format!("{stem}-{}-{nanos}-{n}.log", std::process::id()))
}

/// Counts bytes read from the source (progress of compressed input).
struct Counting<'a, R> {
    inner: R,
    count: &'a AtomicU64,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Result of a finished conversion.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The temporary UTF-8 file.
    pub path: PathBuf,
    /// The encoding the text was converted from.
    pub encoding: TextEncoding,
    /// What was done, for the user.
    pub description: String,
}

struct Shared {
    read: AtomicU64,
    written: AtomicU64,
    total: u64,
    cancel: AtomicBool,
    done: AtomicBool,
    result: Mutex<Option<Result<Prepared, String>>>,
}

/// Converts a file in the background. Dropping it cancels the conversion
/// (and removes the partial output).
pub struct PrepareJob {
    shared: Arc<Shared>,
    source: PathBuf,
    plan: Plan,
}

impl PrepareJob {
    pub fn start(source: &Path, plan: Plan, notify: Notify) -> Self {
        let total = std::fs::metadata(source).map_or(0, |m| m.len());
        let shared = Arc::new(Shared {
            read: AtomicU64::new(0),
            written: AtomicU64::new(0),
            total,
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            result: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let path = source.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("logview-prepare".into())
            .spawn(move || {
                let output = temp_path(&path);
                let result = convert(&path, &output, plan, &worker, &notify);
                if result.is_err() || worker.cancel.load(Ordering::Relaxed) {
                    let _ = std::fs::remove_file(&output);
                }
                let result = result
                    .map(|encoding| Prepared {
                        path: output,
                        encoding,
                        description: plan.describe(encoding),
                    })
                    .map_err(|e| format!("{}: {e}", path.display()));
                *worker.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
                worker.done.store(true, Ordering::Release);
                notify();
            });
        if let Err(e) = spawned {
            *shared.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(Err(e.to_string()));
            shared.done.store(true, Ordering::Release);
        }
        Self {
            shared,
            source: source.to_path_buf(),
            plan,
        }
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn plan(&self) -> Plan {
        self.plan
    }

    pub fn is_running(&self) -> bool {
        !self.shared.done.load(Ordering::Acquire)
    }

    /// Share of the source file read so far.
    pub fn progress(&self) -> f32 {
        if self.shared.total == 0 {
            0.0
        } else {
            (self.shared.read.load(Ordering::Relaxed) as f64 / self.shared.total as f64).min(1.0)
                as f32
        }
    }

    /// UTF-8 bytes written so far.
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Relaxed)
    }

    /// The result once finished (taken once).
    pub fn take_result(&self) -> Option<Result<Prepared, String>> {
        if self.is_running() {
            return None;
        }
        self.shared
            .result
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
}

impl Drop for PrepareJob {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "anulowano")
}

/// Streams `source` through decompression and decoding into `output`.
/// Returns the resolved source encoding.
fn convert(
    source: &Path,
    output: &Path,
    plan: Plan,
    shared: &Shared,
    notify: &Notify,
) -> io::Result<TextEncoding> {
    let file = File::open(source)?;
    let counting = Counting {
        inner: BufReader::with_capacity(BUFFER, file),
        count: &shared.read,
    };
    let decompressed: Box<dyn Read + '_> = match plan.codec {
        None => Box::new(counting),
        Some(Codec::Gzip) => Box::new(flate2::read::MultiGzDecoder::new(counting)),
        Some(Codec::Zstd) => Box::new(zstd::stream::read::Decoder::new(counting)?),
    };
    let mut reader = BufReader::with_capacity(BUFFER, decompressed);
    let encoding = plan.encoding.resolve(reader.fill_buf()?);

    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = BufWriter::with_capacity(4 * BUFFER, File::create(output)?);
    let mut input = vec![0u8; BUFFER];
    let mut last_notify = 0u64;
    let mut report = |written: u64| {
        shared.written.store(written, Ordering::Relaxed);
        if written - last_notify > 32 << 20 {
            last_notify = written;
            notify();
        }
    };
    let mut written = 0u64;
    match encoding.decoder_encoding() {
        None => loop {
            if shared.cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let n = reader.read(&mut input)?;
            if n == 0 {
                break;
            }
            out.write_all(&input[..n])?;
            written += n as u64;
            report(written);
        },
        Some(enc) => {
            let mut decoder = enc.new_decoder_with_bom_removal();
            let capacity = decoder.max_utf8_buffer_length(BUFFER).unwrap_or(4 * BUFFER);
            let mut decoded = vec![0u8; capacity];
            loop {
                if shared.cancel.load(Ordering::Relaxed) {
                    return Err(cancelled());
                }
                let n = reader.read(&mut input)?;
                let last = n == 0;
                let mut pending = &input[..n];
                loop {
                    let (result, read, produced, _) =
                        decoder.decode_to_utf8(pending, &mut decoded, last);
                    out.write_all(&decoded[..produced])?;
                    written += produced as u64;
                    pending = &pending[read..];
                    if result == encoding_rs::CoderResult::InputEmpty {
                        break;
                    }
                }
                report(written);
                if last {
                    break;
                }
            }
        }
    }
    out.flush()?;
    Ok(encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn run(source: &Path, choice: TextEncoding) -> (Plan, Prepared) {
        let plan = plan(source, choice).unwrap();
        assert!(plan.needed());
        let job = PrepareJob::start(source, plan, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(20);
        let result = loop {
            if let Some(r) = job.take_result() {
                break r.unwrap();
            }
            assert!(Instant::now() < deadline, "conversion timed out");
            std::thread::sleep(Duration::from_millis(2));
        };
        (plan, result)
    }

    const TEXT: &str =
        "2026-01-01 12:00:00 INFO zażółć gęślą jaźń\n2026-01-01 12:00:01 ERROR błąd\n";

    #[test]
    fn plain_utf8_needs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log");
        std::fs::write(&path, TEXT).unwrap();
        let p = plan(&path, TextEncoding::Auto).unwrap();
        assert!(!p.needed());
        assert_eq!(
            p,
            Plan {
                codec: None,
                encoding: TextEncoding::Utf8
            }
        );
    }

    #[test]
    fn gzip_is_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.log.gz");
        let mut enc = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        enc.write_all(TEXT.as_bytes()).unwrap();
        enc.finish().unwrap();
        let (plan, out) = run(&path, TextEncoding::Auto);
        assert_eq!(plan.codec, Some(Codec::Gzip));
        assert_eq!(std::fs::read_to_string(&out.path).unwrap(), TEXT);
        assert_eq!(out.description, "rozpakowanie (gzip)");
        std::fs::remove_file(&out.path).unwrap();
    }

    #[test]
    fn zstd_with_utf16_inside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zst");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(TEXT.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(&path, zstd::encode_all(&utf16[..], 3).unwrap()).unwrap();
        let (plan, out) = run(&path, TextEncoding::Auto);
        assert_eq!(plan.codec, Some(Codec::Zstd));
        assert_eq!(
            out.encoding,
            TextEncoding::Utf16Le,
            "resolved after decompression"
        );
        assert_eq!(
            std::fs::read_to_string(&out.path).unwrap(),
            TEXT,
            "BOM removed"
        );
        assert_eq!(
            out.description,
            "rozpakowanie (zstd), konwersja z UTF-16 LE"
        );
        std::fs::remove_file(&out.path).unwrap();
    }

    #[test]
    fn windows_1250_is_converted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cp.log");
        let (bytes, _, _) = encoding_rs::WINDOWS_1250.encode(TEXT);
        std::fs::write(&path, &bytes).unwrap();
        // Auto keeps it as UTF-8 (no BOM, not UTF-16)…
        assert!(!plan(&path, TextEncoding::Auto).unwrap().needed());
        // …the user picks the encoding.
        let (_, out) = run(&path, TextEncoding::Windows1250);
        assert_eq!(std::fs::read_to_string(&out.path).unwrap(), TEXT);
        std::fs::remove_file(&out.path).unwrap();
    }

    #[test]
    fn big_utf16_be_without_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("be.log");
        let text: String = (0..60_000).map(|i| format!("line {i} ąę\n")).collect();
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        std::fs::write(&path, &bytes).unwrap();
        let (plan, out) = run(&path, TextEncoding::Auto);
        assert_eq!(plan.encoding, TextEncoding::Utf16Be);
        assert_eq!(std::fs::read_to_string(&out.path).unwrap(), text);
        std::fs::remove_file(&out.path).unwrap();
    }

    #[test]
    fn broken_gzip_reports_error_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.gz");
        std::fs::write(&path, [0x1F, 0x8B, 0x08, 0, 1, 2, 3]).unwrap();
        let plan = plan(&path, TextEncoding::Auto).unwrap();
        let job = PrepareJob::start(&path, plan, Arc::new(|| {}));
        let result = loop {
            if let Some(r) = job.take_result() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(result.is_err());
    }
}
