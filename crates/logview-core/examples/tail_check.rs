//! End-to-end check of live tail: a writer thread appends lines while a
//! followed `LogFile` reads them; prints the delay from write to visible line
//! and checks that no line is lost or duplicated.
//!
//! ```text
//! cargo run --release -p logview-core --example tail_check -- <dir> [lines] [initial_MB]
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::LogFile;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(
        args.first()
            .expect("usage: tail_check <dir> [lines] [initial_MB]"),
    );
    let lines: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let initial_mb: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let path = dir.join("tail_check.log");

    // Initial content, ending in an incomplete line.
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let line = "0".repeat(99);
        for _ in 0..initial_mb * 1024 * 1024 / 100 {
            writeln!(f, "{line}").unwrap();
        }
        write!(f, "partial-").unwrap();
    }
    let file = LogFile::open(&path, Arc::new(|| {})).unwrap();
    file.start_follow(Arc::new(|| {}));
    while !file.is_tail_ready() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let base = file.line_count();
    println!(
        "initial: {} lines, {:.1} MiB",
        base,
        file.len_bytes() as f64 / (1 << 20) as f64
    );

    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let mut times = Vec::with_capacity(lines);
        for n in 0..lines {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&writer_path)
                .unwrap();
            // The first line completes the initial "partial-".
            writeln!(f, "line {n}").unwrap();
            times.push(Instant::now());
            drop(f);
            std::thread::sleep(Duration::from_millis(20));
        }
        times
    });

    let mut seen = Vec::with_capacity(lines);
    let deadline = Instant::now() + Duration::from_secs(60);
    while seen.len() < lines && Instant::now() < deadline {
        let count = file.line_count();
        while base + seen.len() < count {
            seen.push(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let written = writer.join().unwrap();

    let texts: Vec<String> = file
        .lines(base..base + lines)
        .into_iter()
        .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
        .collect();
    let mut ok = texts.len() == lines;
    for (n, text) in texts.iter().enumerate() {
        let expected = if n == 0 {
            "partial-line 0".to_owned()
        } else {
            format!("line {n}")
        };
        if *text != expected {
            println!("MISMATCH at {n}: {text:?} != {expected:?}");
            ok = false;
            break;
        }
    }
    let mut delays: Vec<f64> = seen
        .iter()
        .zip(&written)
        .map(|(s, w)| s.saturating_duration_since(*w).as_secs_f64() * 1000.0)
        .collect();
    delays.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        delays
            .get(((delays.len() as f64 - 1.0) * q) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    println!(
        "{} of {lines} lines seen, content {}; delay ms: median {:.0}, p95 {:.0}, max {:.0}",
        seen.len(),
        if ok { "OK" } else { "WRONG" },
        pick(0.5),
        pick(0.95),
        pick(1.0)
    );
    file.stop_follow();
    drop(file);
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_file(&path);
}
