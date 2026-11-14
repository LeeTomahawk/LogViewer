//! Computes the scrollbar markers (default highlight rules + a search) and
//! highlights a screenful of lines, printing timings. No GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example minimap_file -- <file> [search]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::{
    HighlightRule, Highlighter, LogFile, MinimapJob, Pattern, PatternKind, SEARCH_BIT, display_cut,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let path: PathBuf = args
        .next()
        .expect("usage: minimap_file <file> [search]")
        .into();
    let search = args
        .next()
        .map(|s| Arc::new(Pattern::new(&s, PatternKind::Text, false).expect("bad search")));

    let file = LogFile::open(&path, Arc::new(|| {})).expect("open failed");
    while file.is_indexing() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let rules = HighlightRule::defaults();
    let highlighter = Arc::new(Highlighter::compile(&rules).unwrap());

    let started = Instant::now();
    let job = MinimapJob::start(
        file.clone(),
        None,
        Arc::clone(&highlighter),
        search.clone(),
        4096,
        Arc::new(|| {}),
    );
    while job.is_running() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let (rule_buckets, search_buckets) = job.with_buckets(|b| {
        (
            b.iter().filter(|m| **m & !SEARCH_BIT != 0).count(),
            b.iter().filter(|m| **m & SEARCH_BIT != 0).count(),
        )
    });
    println!(
        "markers: {} lines in {:.2} s; buckets with rules: {rule_buckets}/4096, with search: {search_buckets}/4096",
        file.line_count(),
        started.elapsed().as_secs_f64()
    );

    // Cost of highlighting one screen (60 lines) as the view does per frame
    // on a cache miss.
    let extra: Vec<(&Pattern, u16)> = search
        .as_deref()
        .map(|s| (s, u16::MAX))
        .into_iter()
        .collect();
    let rounds = 1000;
    let started = Instant::now();
    for round in 0..rounds {
        let first = round * 60 % file.line_count().max(1);
        for (_, bytes) in file.lines(first..first + 60) {
            let hay = &bytes[..display_cut(bytes, 4096)];
            std::hint::black_box(highlighter.highlight(hay, &extra));
        }
    }
    println!(
        "highlight: {:.1} µs per screen of 60 lines",
        started.elapsed().as_secs_f64() * 1e6 / rounds as f64
    );
}
