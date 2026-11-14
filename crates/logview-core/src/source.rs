//! Read-only memory mapping of a file.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

/// A file mapped into memory for reading.
///
/// Empty files are not mapped (mapping a zero-length file fails on some
/// platforms); [`MappedFile::bytes`] then returns an empty slice.
///
/// A converted file (see [`crate::prepare`]) is a temporary file shown under
/// the name of the original; it is deleted when the mapping is dropped (after
/// unmapping, which Windows requires for deletion).
pub struct MappedFile {
    /// Name shown to the user (the original file).
    path: PathBuf,
    map: Option<Mmap>,
    /// Temporary file to delete on drop.
    temp: Option<PathBuf>,
    /// What was done to produce the mapped data (e.g. "rozpakowanie (gzip)").
    conversion: Option<String>,
}

impl MappedFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            map: map_file(path)?,
            temp: None,
            conversion: None,
        })
    }

    /// Maps the converted `temp` file, shown as `display`. `temp` is deleted
    /// when this is dropped, also if mapping fails here.
    pub fn open_temp(temp: &Path, display: &Path, conversion: String) -> io::Result<Self> {
        let map = match map_file(temp) {
            Ok(m) => m,
            Err(e) => {
                let _ = std::fs::remove_file(temp);
                return Err(e);
            }
        };
        Ok(Self {
            path: display.to_path_buf(),
            map,
            temp: Some(temp.to_path_buf()),
            conversion: Some(conversion),
        })
    }

    /// No data, only a name (backing of a merged view).
    pub fn empty(path: PathBuf) -> Self {
        Self {
            path,
            map: None,
            temp: None,
            conversion: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn conversion(&self) -> Option<&str> {
        self.conversion.as_deref()
    }

    pub fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }

    pub fn len(&self) -> u64 {
        self.bytes().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        // Unmap first: Windows does not delete a file that is still mapped.
        self.map = None;
        if let Some(temp) = &self.temp {
            let _ = std::fs::remove_file(temp);
        }
    }
}

fn map_file(path: &Path) -> io::Result<Option<Mmap>> {
    // On Windows std opens files with FILE_SHARE_READ | WRITE | DELETE, so
    // the program writing the log is not blocked from appending.
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    // SAFETY: memmap2 requires that the underlying file is not modified
    // while mapped, which we cannot guarantee for log files. We accept
    // this the same way every mmap-based viewer does:
    // * appending past the mapped length is harmless — we never read
    //   beyond the length captured here (live tail reads appended data
    //   through `File`, see `crate::tail`);
    // * on Windows the OS refuses to truncate a file while a mapping
    //   exists, so the mapped bytes stay valid;
    // * on Unix, truncation by another process could raise SIGBUS.
    // We only create shared `&[u8]` views and never write through them.
    Ok(Some(unsafe { Mmap::map(&file)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_file_is_deleted_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("converted.log");
        std::fs::write(&temp, b"a\nb\n").unwrap();
        let mapped = MappedFile::open_temp(
            &temp,
            Path::new("orig.log.gz"),
            "rozpakowanie (gzip)".into(),
        )
        .unwrap();
        assert_eq!(mapped.path(), Path::new("orig.log.gz"));
        assert_eq!(mapped.bytes(), b"a\nb\n");
        assert_eq!(mapped.conversion(), Some("rozpakowanie (gzip)"));
        drop(mapped);
        assert!(!temp.exists());
    }
}
