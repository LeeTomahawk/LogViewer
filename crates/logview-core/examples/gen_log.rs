//! Synthetic log generator for testing the viewer at scale.
//!
//! ```text
//! cargo run --release -p logview-core --example gen_log -- <size_GB> <file> [--crlf] [--json] [--seed N]
//! ```
//!
//! Produces lines like `2026-01-01 00:00:00.123 INFO  [worker-3] app.db - message`,
//! with occasional multi-line stack traces after ERROR, Polish characters, tabs,
//! and rare very long (1–3 MB) JSON lines.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const LOGGERS: &[&str] = &[
    "app.http",
    "app.db.pool",
    "app.auth",
    "app.cache",
    "app.scheduler",
    "app.payments.gateway",
    "org.hibernate.SQL",
];

const USERS: &[&str] = &["anna", "bartek", "zażółć", "kasia", "admin", "guest"];

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}.{:03}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        ms % 1000
    )
}

fn message(rng: &mut Rng, level: &str) -> String {
    match (level, rng.below(8)) {
        ("ERROR", _) => format!(
            "Request {} failed: java.lang.IllegalStateException: connection reset (user={})",
            rng.next() % 1_000_000,
            rng.pick(USERS)
        ),
        ("WARN", 0..=3) => format!(
            "Slow query took {} ms\tSELECT * FROM orders WHERE id = {}",
            500 + rng.below(5000),
            rng.below(100_000)
        ),
        ("WARN", _) => format!("Retrying operation, attempt {}/5", 1 + rng.below(5)),
        (_, 0) => format!(
            "GET /api/v1/orders/{} 200 {} ms",
            rng.below(100_000),
            rng.below(300)
        ),
        (_, 1) => format!("POST /api/v1/login user={} status=ok", rng.pick(USERS)),
        (_, 2) => format!("Cache hit ratio {}.{}%", rng.below(100), rng.below(10)),
        (_, 3) => format!(
            "Pool stats: active={} idle={} waiting={}",
            rng.below(50),
            rng.below(20),
            rng.below(5)
        ),
        (_, 4) => format!(
            "Scheduled job 'cleanup-{}' finished in {} ms",
            rng.below(20),
            rng.below(10_000)
        ),
        (_, 5) => format!(
            "Użytkownik {} zalogował się z adresu 10.0.{}.{}",
            rng.pick(USERS),
            rng.below(256),
            rng.below(256)
        ),
        (_, 6) => format!(
            "Payment {:016x} authorized amount={}.{:02} PLN",
            rng.next(),
            rng.below(10_000),
            rng.below(100)
        ),
        _ => format!("Heartbeat ok, uptime {} s", rng.below(10_000_000)),
    }
}

fn long_json(rng: &mut Rng, target_len: usize) -> String {
    let mut s = String::with_capacity(target_len + 64);
    s.push_str("{\"event\":\"dump\",\"items\":[");
    let mut i = 0u64;
    while s.len() < target_len {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"id\":{i},\"value\":{},\"tag\":\"t{}\"}}",
            rng.below(1_000_000),
            rng.below(100)
        ));
        i += 1;
    }
    s.push_str("]}");
    s
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        eprintln!("usage: gen_log <size_GB> <file> [--crlf] [--json] [--seed N]");
        std::process::exit(2);
    }
    let size_gb: f64 = positional[0]
        .parse()
        .expect("size_GB must be a number, e.g. 0.5");
    let path = positional[1];
    let crlf = args.iter().any(|a| a == "--crlf");
    let json = args.iter().any(|a| a == "--json");
    let seed = args
        .iter()
        .position(|a| a == "--seed")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED_u64);
    let target = (size_gb * 1024.0 * 1024.0 * 1024.0) as u64;
    let eol = if crlf { "\r\n" } else { "\n" };

    let mut out = BufWriter::with_capacity(8 << 20, File::create(path)?);
    let mut rng = Rng(seed | 1);
    let mut ms: u64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    let mut written = 0u64;
    let mut lines = 0u64;
    let mut next_report = 1u64 << 30;
    let started = Instant::now();

    while written < target {
        ms += rng.below(40);
        let level = match rng.below(100) {
            0..=4 => "TRACE",
            5..=29 => "DEBUG",
            30..=84 => "INFO ",
            85..=94 => "WARN ",
            _ => "ERROR",
        };
        let msg = if rng.below(200_000) == 0 {
            let len = 1_000_000 + rng.below(2_000_000) as usize;
            long_json(&mut rng, len)
        } else {
            message(&mut rng, level.trim_end())
        };
        let thread = rng.below(16);
        let logger = rng.pick(LOGGERS);
        let line = if json {
            let record = serde_json::json!({
                "ts": timestamp(ms).replace(' ', "T"),
                "level": level.trim_end(),
                "thread": format!("worker-{thread}"),
                "logger": logger,
                "msg": msg,
            });
            format!("{record}{eol}")
        } else {
            format!(
                "{} {} [worker-{thread}] {logger} - {msg}{eol}",
                timestamp(ms),
                level,
            )
        };
        out.write_all(line.as_bytes())?;
        written += line.len() as u64;
        lines += 1;

        if !json && level == "ERROR" && rng.below(10) == 0 {
            for depth in 0..5 + rng.below(8) {
                let frame = format!(
                    "\tat com.example.service.Handler{}.process(Handler.java:{}){eol}",
                    depth,
                    10 + rng.below(500)
                );
                out.write_all(frame.as_bytes())?;
                written += frame.len() as u64;
                lines += 1;
            }
        }

        if written >= next_report {
            eprintln!(
                "{:.1} GB, {lines} lines…",
                written as f64 / (1u64 << 30) as f64
            );
            next_report += 1 << 30;
        }
    }
    out.flush()?;
    eprintln!(
        "done: {path}, {:.2} GB, {lines} lines in {:.1} s",
        written as f64 / (1u64 << 30) as f64,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
