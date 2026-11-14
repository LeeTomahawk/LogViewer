//! Benchmarks of the hot paths, on synthetic log lines.
//!
//! ```text
//! cargo bench -p logview-core --bench core
//! cargo bench -p logview-core --bench core -- --warm-up-time 1 --measurement-time 3   # faster
//! ```

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use logview_core::{
    FilterRule, FilterSpec, HighlightRule, Highlighter, LineIndex, LogFormat, display_line,
};

/// `lines` lines in the format of the `gen_log` example.
fn sample(lines: usize) -> Vec<u8> {
    let levels = [
        "INFO ", "DEBUG", "INFO ", "WARN ", "INFO ", "ERROR", "TRACE",
    ];
    let mut out = Vec::with_capacity(lines * 110);
    for i in 0..lines {
        let level = levels[i % levels.len()];
        let line = format!(
            "2026-01-01 {:02}:{:02}:{:02}.{:03} {level} [worker-{}] app.db.pool - Request {} took {} ms user=zażółć\n",
            i / 3_600_000 % 24,
            i / 60_000 % 60,
            i / 1000 % 60,
            i % 1000,
            i % 16,
            i * 7919 % 100_000,
            i % 5000
        );
        out.extend_from_slice(line.as_bytes());
    }
    out
}

fn lines(data: &[u8]) -> Vec<&[u8]> {
    data.split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect()
}

fn index(c: &mut Criterion) {
    let data = sample(600_000); // ~64 MB
    let mut group = c.benchmark_group("index");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.sample_size(20);
    group.bench_function("build 64 MB", |b| {
        b.iter(|| black_box(LineIndex::build(&data, 8 << 20).line_count()))
    });
    group.finish();
}

fn filter(c: &mut Criterion) {
    let data = sample(100_000);
    let lines = lines(&data);
    let mut group = c.benchmark_group("filter");
    group.throughput(Throughput::Elements(lines.len() as u64));
    let cases = [
        (
            "text case-sensitive",
            FilterRule::include("ERROR").case_sensitive(),
        ),
        ("text ignore case", FilterRule::include("error")),
        ("text ignore case, Polish", FilterRule::include("ZAŻÓŁĆ")),
        ("regex", FilterRule::include(r"took \d{4,} ms").regex()),
        ("exclude", FilterRule::exclude("DEBUG")),
    ];
    for (name, rule) in cases {
        let filter = FilterSpec {
            rules: vec![rule],
            ..Default::default()
        }
        .compile()
        .unwrap();
        group.bench_function(name, |b| {
            b.iter(|| lines.iter().filter(|l| filter.matches(l)).count())
        });
    }
    group.finish();
}

fn format(c: &mut Criterion) {
    let data = sample(100_000);
    let lines = lines(&data);
    let builtin = LogFormat::builtins()[0].clone();
    let fast = builtin.compile(&lines[..100]).unwrap();
    // Same regex with the fast path disabled (`(?:)` after `^`).
    let logview_core::FormatKind::Regex(pattern) = &builtin.kind else {
        unreachable!()
    };
    let slow = LogFormat::regex("slow", &pattern.replacen('^', "^(?:)", 1))
        .compile(&lines[..100])
        .unwrap();
    let mut group = c.benchmark_group("format meta");
    group.throughput(Throughput::Elements(lines.len() as u64));
    group.bench_function("fast path", |b| {
        b.iter(|| lines.iter().filter_map(|l| fast.meta(l)).count())
    });
    group.bench_function("capture groups", |b| {
        b.iter(|| lines.iter().filter_map(|l| slow.meta(l)).count())
    });
    group.finish();
}

fn render(c: &mut Criterion) {
    let data = sample(60);
    let lines = lines(&data);
    let highlighter = Highlighter::compile(&HighlightRule::defaults()).unwrap();
    let mut group = c.benchmark_group("screen of 60 lines");
    group.bench_function("highlight", |b| {
        b.iter(|| {
            for l in &lines {
                black_box(highlighter.highlight(l, &[]));
            }
        })
    });
    group.bench_function("display text", |b| {
        b.iter(|| {
            for l in &lines {
                black_box(display_line(l, 4096, 4));
            }
        })
    });
    group.finish();
}

criterion_group!(benches, index, filter, format, render);
criterion_main!(benches);
