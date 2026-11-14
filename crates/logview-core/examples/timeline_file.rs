//! Computes the timeline (records per time bucket and level) and looks up a
//! time, printing timings. No GUI.
//!
//! ```text
//! cargo run --release -p logview-core --example timeline_file -- <file>
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use logview_core::{LogFile, ParseJob, TimelineJob, detect, time};

fn wait(cond: impl Fn() -> bool) {
    while !cond() {
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn main() {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: timeline_file <file>"),
    );
    let file = LogFile::open(&path, Arc::new(|| {})).expect("open");
    wait(|| !file.is_indexing());
    let sample = file.lines(0..500);
    let sample: Vec<&[u8]> = sample.iter().map(|(_, b)| *b).collect();
    let format = Arc::new(detect(&sample).format.compile(&sample).expect("format"));
    let parse = ParseJob::start(file.clone(), format, true, Arc::new(|| {}));
    wait(|| !parse.is_running());
    let records = parse.records();
    println!(
        "{} records, parsed in {:.2} s",
        records.record_count(),
        parse.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let job = TimelineJob::start(records.clone(), None, 1000, Arc::new(|| {}));
    wait(|| !job.is_running());
    let t = job.result().expect("timeline");
    println!(
        "timeline: {} buckets of {} s, max {} records/bucket, in {:.3} s",
        t.counts.len(),
        t.bucket_secs,
        t.max_total(),
        started.elapsed().as_secs_f64()
    );

    let (min, max) = records.time_bounds().unwrap();
    let target = min + (max - min) * 3 / 4;
    let started = Instant::now();
    let record = records.find_time(target).unwrap();
    let line = records.record(record).unwrap().lines.start;
    println!(
        "find_time({}) → line {} in {:.1} ms",
        time::format_secs(target),
        line + 1,
        started.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "  {}",
        String::from_utf8_lossy(
            &file.line(line).unwrap()[..60.min(file.line(line).unwrap().len())]
        )
    );
}
