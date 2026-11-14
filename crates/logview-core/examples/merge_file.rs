//! Merges files by time and prints timings, then filters the merged view.
//! No GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example merge_file -- <file1> <file2> [...]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::{
    FilterJob, FilterRule, FilterSpec, LogFile, MergeSource, ParseJob, detect, merge_files,
};

fn wait(cond: impl Fn() -> bool) {
    while !cond() {
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    assert!(paths.len() >= 2, "usage: merge_file <file1> <file2> [...]");

    let started = Instant::now();
    let mut jobs = Vec::new();
    let mut sources = Vec::new();
    for path in &paths {
        let file = LogFile::open(path, Arc::new(|| {})).expect("open");
        wait(|| !file.is_indexing());
        let sample = file.lines(0..500);
        let sample: Vec<&[u8]> = sample.iter().map(|(_, b)| *b).collect();
        let format = Arc::new(detect(&sample).format.compile(&sample).expect("format"));
        let job = ParseJob::start(file.clone(), Arc::clone(&format), true, Arc::new(|| {}));
        sources.push(MergeSource {
            file: file.clone(),
            format,
            records: job.records(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
        });
        jobs.push(job);
    }
    for job in &jobs {
        wait(|| !job.is_running());
    }
    let total_lines: usize = sources.iter().map(|s| s.file.line_count()).sum();
    println!(
        "{} files, {total_lines} lines indexed and parsed in {:.2} s",
        sources.len(),
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let merged = merge_files(sources, "merged", Arc::new(|| {}));
    let mut first_rows = None;
    while merged.is_indexing() {
        if first_rows.is_none() && merged.line_count() > 0 {
            first_rows = Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!(
        "merge: {} rows in {:.2} s (first rows after {:.2} s), memory {:.1} MiB",
        merged.line_count(),
        merged.index_elapsed().as_secs_f64(),
        first_rows.map_or(0.0, |t| t.as_secs_f64()),
        merged.index_memory() as f64 / (1u64 << 20) as f64
    );
    assert_eq!(merged.line_count(), total_lines, "every line merged once");

    // Check order on a sample: timestamps (first 23 chars) never decrease
    // across header lines.
    let mut last = String::new();
    let mut checked = 0;
    for (_, line) in merged.lines(0..200_000) {
        let text = String::from_utf8_lossy(line);
        if text.len() >= 23 && text.as_bytes()[4] == b'-' {
            let ts = text[..23].to_owned();
            assert!(ts >= last, "order broken: {ts} after {last}");
            last = ts;
            checked += 1;
        }
    }
    println!("order OK on {checked} header lines");

    let spec = FilterSpec {
        rules: vec![FilterRule::include("ERROR")],
        ..Default::default()
    };
    let started = Instant::now();
    let job = FilterJob::start(
        merged.clone(),
        Arc::new(spec.compile().unwrap()),
        0,
        0,
        None,
        Arc::new(|| {}),
    );
    wait(|| !job.is_running());
    println!(
        "filter ERROR on merged view: {} lines in {:.2} s",
        job.match_count(),
        started.elapsed().as_secs_f64()
    );
}
