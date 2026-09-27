//! The benchmark driver behind `scripts/bench/run.sh` (A-P-1, A-P-2, A-P-5, A-P-6, A-P-7).
//! It drives the release binary on a pty and reads its `--log`, or runs the engine
//! directly. Without a subcommand (as under `cargo test`) it exits at once.
//!
//! Subcommands (after `--`):
//!   first-frame BIN LEFT RIGHT RUNS          median and max first-full-frame time, ms
//!   navigate BIN DIR ENTRY KEYS [COPY DST]   p99 key-to-flush, ms; with COPY, during a copy
//!   idle BIN DIR SECS                        context switches and CPU ticks over SECS
//!   rss BIN LEFT RIGHT                       resident set with both panels loaded, MB
//!   copy SRC_DIR NAME DST                    engine copy, seconds
//!   move SRC_DIR NAME DST                    engine move, seconds

use expectrl::Session;
use manycommander::fsops::job::{JobSpec, run_guarded};
use manycommander::fsops::question::{Answer, Interaction, Progress, Question};
use manycommander::fsops::sys::Sys;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Tui {
    s: expectrl::session::OsSession,
    parser: vt100::Parser,
    raw: Vec<u8>,
    answered: usize,
    da1: bool,
}

impl Tui {
    fn spawn(bin: &str, args: &[&str]) -> Tui {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("PATH", no_desktop_path());
        let mut s = Session::spawn(cmd).expect("spawn");
        let _ = s.get_process_mut().set_window_size(160, 50);
        Tui {
            s,
            parser: vt100::Parser::new(50, 160, 0),
            raw: Vec::new(),
            answered: 0,
            da1: false,
        }
    }

    fn pid(&self) -> i32 {
        self.s.get_process().pid().as_raw()
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok(n) = self.s.try_read(&mut buf) {
            if n == 0 {
                break;
            }
            self.raw.extend_from_slice(&buf[..n]);
            self.parser.process(&buf[..n]);
        }
        if !self.da1 && self.raw.windows(3).any(|w| w == b"\x1b[c") {
            self.da1 = true;
            // A terminal with the kitty keyboard protocol, as the Omarchy terminals are.
            self.send(b"\x1b[?0u\x1b[?62;22c");
        }
        let dsr = self.raw.windows(4).filter(|w| *w == b"\x1b[6n").count();
        while self.answered < dsr {
            self.answered += 1;
            let (r, c) = self.parser.screen().cursor_position();
            self.send(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
        }
    }

    fn send(&mut self, b: &[u8]) {
        use std::io::Write;
        self.s.write_all(b).unwrap();
        self.s.flush().unwrap();
    }

    fn wait_for(&mut self, what: &str, t: Duration) -> bool {
        let end = Instant::now() + t;
        while Instant::now() < end {
            self.pump();
            if self.parser.screen().contents().contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn wait_exit(&mut self, t: Duration) {
        let end = Instant::now() + t;
        while Instant::now() < end {
            self.pump();
            if matches!(
                self.s.get_process().status(),
                Ok(expectrl::process::unix::WaitStatus::Exited(..))
            ) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self
            .s
            .get_process_mut()
            .kill(expectrl::process::unix::Signal::SIGKILL);
    }
}

/// `PATH` with a no-op `xdg-open` first: a benchmark never opens anything on the desktop.
fn no_desktop_path() -> std::ffi::OsString {
    let dir = std::env::temp_dir().join(format!("mc-bench-stub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("xdg-open");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut p = dir.into_os_string();
    p.push(":");
    p.push(std::env::var_os("PATH").unwrap_or_default());
    p
}

/// `field=value` numbers of log lines that contain `marker`.
fn log_values(log: &Path, marker: &str, field: &str) -> Vec<f64> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let key = format!("{field}=");
    text.lines()
        .filter(|l| l.contains(marker))
        .filter_map(|l| {
            let v = l.split(&key).nth(1)?.split_whitespace().next()?;
            v.parse().ok()
        })
        .collect()
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

fn temp_log(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("mc-bench-{tag}-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

const F10: &[u8] = b"\x1b[21~";

fn first_frame(bin: &str, left: &str, right: &str, runs: usize) {
    let mut v = Vec::new();
    for _ in 0..runs {
        let log = temp_log("ff");
        let mut t = Tui::spawn(
            bin,
            &[
                "--log",
                log.to_str().unwrap(),
                "--exit-after-first-frame",
                left,
                right,
            ],
        );
        t.wait_exit(Duration::from_secs(10));
        let us = log_values(&log, "first full frame", "first_full_frame_us");
        v.push(us.first().copied().expect("first-frame line") / 1000.0);
        let _ = std::fs::remove_file(&log);
    }
    let max = v.iter().cloned().fold(0.0, f64::max);
    println!(
        "first_frame_ms median={:.2} max={:.2} runs={runs}",
        percentile(&mut v, 0.5),
        max
    );
}

/// A navigation session in `dir/entry` (a 100k-entry directory): Down and PgDn keys at a
/// steady pace. With `copy`, F5 on that file in `dir` to `dst` first, and the job must still
/// run after the last sample.
fn navigate(bin: &str, dir: &str, entry: &str, keys: usize, copy: Option<(&str, &str)>) {
    let log = temp_log("nav");
    let right = copy.map(|c| c.1).unwrap_or(dir);
    let mut t = Tui::spawn(bin, &["--log", log.to_str().unwrap(), dir, right]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)), "no UI");
    std::thread::sleep(Duration::from_millis(300));
    if let Some((file, _)) = copy {
        // Quick search to the file, F5, confirm.
        t.send(b"\x1b[115;5u");
        std::thread::sleep(Duration::from_millis(50));
        t.send(file.as_bytes());
        std::thread::sleep(Duration::from_millis(100));
        t.send(b"\r");
        std::thread::sleep(Duration::from_millis(50));
        t.send(b"\x1b[15~");
        assert!(t.wait_for("Copy", Duration::from_secs(5)), "no copy dialog");
        t.send(b"\r");
        assert!(
            t.wait_for("copy ", Duration::from_secs(10)),
            "the job did not start"
        );
    }
    // Into the big directory, with the command line: `cd` never opens a file.
    t.send(format!("cd {entry}\r").as_bytes());
    // Wait until the listing is complete (the footer shows the count).
    assert!(
        t.wait_for("100000 entries", Duration::from_secs(30)),
        "the directory did not load:\n{}",
        t.parser.screen().contents()
    );
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    for i in 0..keys {
        t.send(if i % 10 == 9 { b"\x1b[6~" } else { b"\x1b[B" });
        std::thread::sleep(Duration::from_millis(20));
        t.pump();
    }
    let sampling = started.elapsed();
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    let job_running = copy.is_none() || t.parser.screen().contents().contains("copy ");
    t.send(F10);
    if copy.is_some() {
        // Quit asks to cancel the running job.
        std::thread::sleep(Duration::from_millis(200));
        t.send(b"\r");
    }
    t.wait_exit(Duration::from_secs(60));
    let mut lat: Vec<f64> = log_values(&log, " frame", "key_to_flush_us")
        .iter()
        .map(|u| u / 1000.0)
        .collect();
    // The samples of the navigation phase: the last `keys` frames before quitting.
    let n = lat.len();
    let take = keys.min(n);
    let mut samples: Vec<f64> = lat
        .drain(n.saturating_sub(take + 1)..n.saturating_sub(1))
        .collect();
    let p99 = percentile(&mut samples, 0.99);
    let p50 = percentile(&mut samples, 0.5);
    println!(
        "navigate p99_ms={p99:.2} p50_ms={p50:.2} samples={} sampling_s={:.1} job_running_after={job_running}",
        samples.len(),
        sampling.as_secs_f64()
    );
    let _ = std::fs::remove_file(&log);
}

fn proc_counters(pid: i32) -> (u64, u64) {
    let mut switches = 0;
    let mut ticks = 0;
    if let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        for t in rd.flatten() {
            let p = t.path();
            if let Ok(s) = std::fs::read_to_string(p.join("status")) {
                for l in s.lines() {
                    if let Some(v) = l.strip_prefix("voluntary_ctxt_switches:") {
                        switches += v.trim().parse::<u64>().unwrap_or(0);
                    }
                }
            }
            if let Ok(s) = std::fs::read_to_string(p.join("stat"))
                && let Some(after) = s.rsplit_once(')')
            {
                let f: Vec<&str> = after.1.split_whitespace().collect();
                // Fields 14 and 15 (utime, stime) are at 11 and 12 after the name.
                ticks += f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
            }
        }
    }
    (switches, ticks)
}

fn idle(bin: &str, dir: &str, secs: u64) {
    let mut t = Tui::spawn(bin, &[dir, dir]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)));
    std::thread::sleep(Duration::from_secs(3));
    t.pump();
    let a = proc_counters(t.pid());
    std::thread::sleep(Duration::from_secs(secs));
    let b = proc_counters(t.pid());
    println!(
        "idle secs={secs} switches_before={} switches_after={} ticks_before={} ticks_after={}",
        a.0, b.0, a.1, b.1
    );
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
}

fn rss(bin: &str, left: &str, right: &str) {
    let mut t = Tui::spawn(bin, &[left, right]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)));
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        t.pump();
        if t.parser
            .screen()
            .contents()
            .matches("100000 entries")
            .count()
            >= 2
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));
    let s = std::fs::read_to_string(format!("/proc/{}/status", t.pid())).unwrap();
    let kb: f64 = s
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap();
    println!("rss_mb={:.1}", kb / 1024.0);
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
}

struct Silent;

impl Interaction for Silent {
    fn ask(&mut self, q: Question) -> Answer {
        panic!("unexpected question: {q:?}");
    }
    fn progress(&mut self, _: Progress) {}
}

fn engine(moving: bool, src: &str, name: &str, dst: &str) {
    let spec = if moving {
        JobSpec::Move {
            src_dir: src.into(),
            names: vec![name.into()],
            dst: dst.into(),
        }
    } else {
        JobSpec::Copy {
            src_dir: src.into(),
            names: vec![name.into()],
            dst: dst.into(),
        }
    };
    let start = Instant::now();
    let r = run_guarded(spec, &Sys::default(), &mut Silent);
    let s = start.elapsed().as_secs_f64();
    assert!(r.failed == 0 && r.refused.is_none(), "{r:?}");
    println!("engine_s={s:.3} done={}", r.done);
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["first-frame", bin, l, r, runs] => first_frame(bin, l, r, runs.parse().unwrap()),
        ["navigate", bin, dir, entry, keys] => {
            navigate(bin, dir, entry, keys.parse().unwrap(), None)
        }
        ["navigate", bin, dir, entry, keys, copy, dst] => {
            navigate(bin, dir, entry, keys.parse().unwrap(), Some((copy, dst)))
        }
        ["idle", bin, dir, secs] => idle(bin, dir, secs.parse().unwrap()),
        ["rss", bin, l, r] => rss(bin, l, r),
        ["copy", src, name, dst] => engine(false, src, name, dst),
        ["move", src, name, dst] => engine(true, src, name, dst),
        // `cargo test --all-targets` runs benches without arguments.
        _ => {}
    }
}
