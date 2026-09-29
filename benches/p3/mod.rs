//! The phase 3 benchmarks (P3 7.1: P-18 to P-27, P-5b, P-6c), as subcommands of the
//! driver, which `scripts/bench/run.sh` runs. The fixtures are generated below
//! `<bench dir>/p3` ([`fixtures`]) and compressed with the system tools; `run.sh` removes
//! them after a run unless `MC_BENCH_KEEP=1`.
//!
//! Subcommands (after `--`):
//!   p3-fixture KIND                        creates a phase 3 fixture, prints its path(s)
//!   p3-list ARCHIVE RUNS                   P-18, P-19: first rows, full scan and the same
//!                                          process's decompress-only run, ms (medians)
//!   p3-extract ARCHIVE DST                 P-22: scan and extract into DST (one run, for
//!                                          hyperfine)
//!   p3-cancel BIN ARCHIVE...               P-20: Esc during the scan of each, on a pty
//!   p3-inside BIN ARCHIVE DIR CYCLES       P-21: enter and leave DIR inside ARCHIVE
//!   p3-preview BIN DIR PROTOCOL [RUNS]     P-23: first preview after the debounce, cache hit
//!   p3-burst BIN DIR IMAGES                P-24: key-to-frame through IMAGES images
//!   p3-probe BIN LEFT RIGHT RUNS TERM      P-25: first full frame with the probe
//!   p3-rss BIN LEFT RIGHT ARCHIVE [SSHCFG] P-6c: RSS with a cached index
//!   p3-idle BIN REMOTE ARCHIVE SSHCFG SECS P-5b: idle with a session, an index, an image
//!   p3-sftp-* ...                          P-26, P-27 and the small trees ([`sftp`])
//!   delay-pipe MS CMD ARGS...              the latency helper: CMD behind a link that
//!                                          delays each direction by MS

pub mod archive;
pub mod fixtures;
pub mod preview;
pub mod pty;
pub mod sftp;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Runs a phase 3 subcommand; `false` when `a` is none.
pub fn run(a: &[&str]) -> bool {
    if a.first().is_some_and(|s| s.starts_with("p3-")) {
        // The allocator as the binary sets it, first thing in `main` (P-6): the in-process
        // checks allocate what the app allocates, with the same mmap threshold.
        manycommander::fsops::sys::tune_allocator();
    }
    match a {
        ["p3-fixture", kind] => fixtures::main(kind),
        ["p3-list", archive, runs] => archive::list(archive, runs.parse().unwrap()),
        ["p3-extract", archive, dst] => archive::extract(archive, dst),
        ["p3-cancel", bin, rest @ ..] => archive::cancel(bin, rest),
        ["p3-inside", bin, archive, dir, cycles] => {
            archive::inside(bin, archive, dir, cycles.parse().unwrap())
        }
        ["p3-preview", bin, dir, protocol] => preview::latency(bin, dir, protocol, 1),
        ["p3-preview", bin, dir, protocol, runs] => {
            preview::latency(bin, dir, protocol, runs.parse().unwrap())
        }
        ["p3-burst", bin, dir, images] => preview::burst(bin, dir, images.parse().unwrap()),
        ["p3-probe", bin, l, r, runs, term] => {
            preview::probe(bin, l, r, runs.parse().unwrap(), term)
        }
        ["p3-rss", bin, l, r, archive] => archive::rss(bin, l, r, archive, None),
        ["p3-rss", bin, l, r, archive, cfg] => archive::rss(bin, l, r, archive, Some(cfg)),
        ["p3-idle", bin, remote, archive, cfg, secs] => {
            sftp::idle(bin, remote, archive, cfg, secs.parse().unwrap())
        }
        ["delay-pipe", ms, cmd @ ..] => sftp::delay_pipe(ms.parse().unwrap(), cmd),
        [sub, rest @ ..] if sub.starts_with("p3-sftp-") => sftp::main(sub, rest),
        _ => return false,
    }
    true
}

/// `p3` below the bench directory.
pub fn p3() -> PathBuf {
    crate::common::base().join("p3")
}

/// Seconds since the epoch, as the log's timestamps count them.
pub fn wall() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The median of `v` (NaN when empty).
pub fn median(v: &[f64]) -> f64 {
    pct(v, 0.5)
}

/// The `p` quantile of `v`, nearest rank (NaN when empty).
pub fn pct(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

pub fn max(v: &[f64]) -> f64 {
    v.iter().cloned().fold(f64::NAN, f64::max)
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// One line of `--log`: its wall time, thread name and text.
pub struct LogLine {
    pub at: f64,
    pub thread: String,
    pub text: String,
}

/// The lines of a `--log` file that contain `marker`. The subscriber writes
/// `<RFC 3339 time> <LEVEL> <thread> <target>: <message> <fields>`.
pub fn log_lines(log: &Path, marker: &str) -> Vec<LogLine> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter(|l| l.contains(marker))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let ts: jiff::Timestamp = w.next()?.parse().ok()?;
            let _level = w.next()?;
            let thread = w.next()?.to_owned();
            Some(LogLine {
                at: ts.as_microsecond() as f64 / 1e6,
                thread,
                text: l.to_owned(),
            })
        })
        .collect()
}

/// The number after `field=` in a log line.
pub fn field(line: &str, name: &str) -> Option<f64> {
    let key = format!(" {name}=");
    let v = line.split(&key).nth(1)?.split_whitespace().next()?;
    v.trim_matches('"').parse().ok()
}

/// The key-to-flush of every frame logged so far, ms, in order.
pub fn frames(log: &Path) -> Vec<f64> {
    log_lines(log, " frame")
        .iter()
        .filter(|l| l.text.contains(" frame key_to_flush_us="))
        .filter_map(|l| field(&l.text, "key_to_flush_us"))
        .map(|us| us / 1000.0)
        .collect()
}

/// `p50_ms=.. p99_ms=.. max_ms=.. samples=..` of `v`.
pub fn summary(v: &[f64]) -> String {
    format!(
        "p50_ms={:.2} p99_ms={:.2} max_ms={:.2} samples={}",
        pct(v, 0.5),
        pct(v, 0.99),
        max(v),
        v.len()
    )
}

/// Resident set of `pid`, MB.
pub fn rss_mb(pid: i32) -> f64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(f64::NAN)
        / 1024.0
}

/// A directory of its own for one run below `p3/run`, empty.
pub fn run_dir(what: &str) -> PathBuf {
    let d = p3()
        .join("run")
        .join(format!("{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// No core dumps from this process or anything it spawns (`sshd`, `ssh`, `sftp-server`):
/// a kill at the end of a run must never reach the desktop as a crash report.
pub fn no_core_dumps() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let max = getrlimit(Resource::Core).maximum;
    let _ = setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: max,
        },
    );
}
