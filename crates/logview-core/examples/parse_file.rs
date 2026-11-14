//! Detects the format, parses records and runs a level filter, printing
//! timings. No GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example parse_file -- <file> [--single-line]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::{FilterJob, FilterSpec, Level, LevelSet, LogFile, ParseJob, detect, time};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = PathBuf::from(
        args.first()
            .expect("usage: parse_file <file> [--single-line]"),
    );
    let multiline = !args.iter().any(|a| a == "--single-line");

    let file = LogFile::open(&path, Arc::new(|| {})).expect("open failed");
    while file.is_indexing() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let sample_lines = file.lines(0..500);
    let sample: Vec<&[u8]> = sample_lines.iter().map(|(_, b)| *b).collect();
    let detection = detect(&sample);
    let format = Arc::new(detection.format.compile(&sample).expect("compile"));
    println!(
        "format: {} ({:.0}% of sample), columns: {:?}",
        detection.format.name,
        detection.ratio * 100.0,
        format.columns()
    );

    let job = ParseJob::start(
        file.clone(),
        Arc::clone(&format),
        multiline,
        Arc::new(|| {}),
    );
    while job.is_running() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let records = job.records();
    println!(
        "parse: {} lines → {} records in {:.2} s, memory {:.1} MiB",
        file.line_count(),
        records.record_count(),
        job.elapsed().as_secs_f64(),
        records.memory_bytes() as f64 / (1u64 << 20) as f64
    );
    if let Some((from, to)) = records.time_bounds() {
        println!(
            "time: {} – {}",
            time::format_secs(from),
            time::format_secs(to)
        );
    }
    let counts = records.level_counts();
    let names = ["(brak)", "TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"];
    let summary: Vec<String> = names
        .iter()
        .zip(counts)
        .map(|(n, c)| format!("{n}={c}"))
        .collect();
    println!("levels: {}", summary.join(" "));

    let mut levels = LevelSet(0);
    levels.set(Some(Level::Error), true);
    let spec = FilterSpec {
        levels,
        ..Default::default()
    };
    let started = Instant::now();
    let filter = FilterJob::start(
        file.clone(),
        Arc::new(spec.compile().unwrap()),
        0,
        0,
        Some(records),
        Arc::new(|| {}),
    );
    while filter.is_running() {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "level filter ERROR: {} lines in {:.2} s",
        filter.match_count(),
        started.elapsed().as_secs_f64()
    );
}
