//! Appends log lines to a file at a steady rate, to try live tail.
//!
//! ```text
//! cargo run --release -p logview-core --example write_live -- <file> [lines_per_second] [--rotate-every N]
//! ```
//!
//! With `--rotate-every N`, after every N lines the file is renamed to
//! `<file>.1` and a new one is started (rotation by rename).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn timestamp() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let secs = ms.div_euclid(1000);
    let (y, m, d) = logview_core::time::civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = PathBuf::from(
        args.first()
            .expect("usage: write_live <file> [lines_per_second] [--rotate-every N]"),
    );
    let rate: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let rotate_every: Option<u64> = args
        .iter()
        .position(|a| a == "--rotate-every")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());
    let interval = Duration::from_secs_f64(1.0 / rate.max(0.1));
    eprintln!(
        "writing {rate} lines/s to {} (Ctrl+C stops)",
        path.display()
    );

    let started = Instant::now();
    for n in 0u64.. {
        if let Some(every) = rotate_every
            && n > 0
            && n % every == 0
        {
            let rotated = path.with_extension("log.1");
            let _ = std::fs::remove_file(&rotated);
            std::fs::rename(&path, &rotated)?;
            eprintln!("rotated after {n} lines");
        }
        let (level, msg) = match n % 20 {
            0 => ("ERROR", format!("Request {n} failed: connection reset")),
            5 | 15 => ("WARN ", format!("Slow response for request {n}")),
            7 => ("DEBUG", format!("Cache stats after {n} requests")),
            _ => ("INFO ", format!("Handled request {n}")),
        };
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{} {level} [live] app.live - {msg}", timestamp())?;
        if n % 20 == 0 {
            writeln!(file, "\tat com.example.Live.handle(Live.java:{})", n % 100)?;
        }
        drop(file);
        let next = started + interval * (n as u32 + 1);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    Ok(())
}
