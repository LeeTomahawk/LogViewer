//! Indexes a file and prints timing, without the GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example index_file -- <file>
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::LogFile;

fn main() {
    let path: PathBuf = std::env::args_os()
        .nth(1)
        .expect("usage: index_file <file>")
        .into();
    let started = Instant::now();
    let file = LogFile::open(&path, Arc::new(|| {})).expect("open failed");
    let mut first_lines = None;
    while file.is_indexing() {
        if first_lines.is_none() && file.line_count() > 0 {
            first_lines = Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let secs = file.index_elapsed().as_secs_f64();
    let gib = file.len_bytes() as f64 / (1u64 << 30) as f64;
    println!("file:          {}", path.display());
    println!("size:          {gib:.2} GiB");
    println!("lines:         {}", file.line_count());
    if let Some(t) = first_lines {
        println!("first lines:   {:.1} ms", t.as_secs_f64() * 1000.0);
    }
    println!("index time:    {secs:.2} s ({:.2} GiB/s)", gib / secs);
    println!(
        "index memory:  {:.1} MiB ({:.2} B/line)",
        file.index_memory() as f64 / (1u64 << 20) as f64,
        file.index_memory() as f64 / file.line_count().max(1) as f64
    );
}
