//! Filters a file and prints timing, without the GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example filter_file -- <file> <pattern> [--regex] [--case] [--exclude] [-C N]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::{FilterJob, FilterRule, FilterSpec, LogFile, PatternKind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: filter_file <file> <pattern> [--regex] [--case] [--exclude] [-C N]");
        std::process::exit(2);
    }
    let path = PathBuf::from(&args[0]);
    let rule = FilterRule {
        pattern: args[1].clone(),
        kind: if args.iter().any(|a| a == "--regex") {
            PatternKind::Regex
        } else {
            PatternKind::Text
        },
        case_sensitive: args.iter().any(|a| a == "--case"),
        exclude: args.iter().any(|a| a == "--exclude"),
        enabled: true,
    };
    let context = args
        .iter()
        .position(|a| a == "-C")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let spec = FilterSpec {
        rules: vec![rule],
        context_before: context,
        context_after: context,
        ..Default::default()
    };
    let filter = Arc::new(
        spec.compile()
            .unwrap_or_else(|e| panic!("bad filter: {e:?}")),
    );

    let file = LogFile::open(&path, Arc::new(|| {})).expect("open failed");
    while file.is_indexing() {
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "index: {} lines in {:.2} s",
        file.line_count(),
        file.index_elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let job = FilterJob::start(
        file.clone(),
        filter,
        context,
        context,
        None,
        Arc::new(|| {}),
    );
    let mut first_result = None;
    while job.is_running() {
        if first_result.is_none() && !job.is_empty() {
            first_result = Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let secs = job.elapsed().as_secs_f64();
    let gib = file.len_bytes() as f64 / (1u64 << 30) as f64;
    println!("matches: {} (rows {})", job.match_count(), job.len());
    if let Some(t) = first_result {
        println!("first result after {:.1} ms", t.as_secs_f64() * 1000.0);
    }
    println!(
        "filter: {secs:.2} s ({:.2} GiB/s, {:.1} M lines/s), result memory {:.1} MiB",
        gib / secs,
        file.line_count() as f64 / secs / 1e6,
        job.memory_bytes() as f64 / (1u64 << 20) as f64
    );
}
