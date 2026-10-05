//! The benchmark driver behind `scripts/bench/run.sh` (A-P-1, A-P-2, A-P-5, A-P-6, A-P-7,
//! and the phase 2 checks that need the binary on a pty, a separate process, or the copy
//! engine). It drives the release binary on a pty and reads its `--log`, or runs the engine
//! directly. Without a subcommand (as under `cargo test`) it exits at once.
//!
//! Subcommands (after `--`):
//!   first-frame BIN LEFT RIGHT RUNS [TSV]    median and max first-full-frame time, ms; with
//!                                            TSV, that `dirs.tsv` in the state directory
//!   navigate BIN DIR ENTRY KEYS [COPY DST]   p99 key-to-flush, ms; with COPY, during a copy
//!   idle BIN DIR SECS                        context switches and CPU ticks over SECS
//!   rss BIN LEFT RIGHT [REFRESHES]           resident set with both panels loaded, MB; then
//!                                            after REFRESHES Ctrl+R
//!   copy SRC_DIR NAME DST                    engine copy, seconds
//!   move SRC_DIR NAME DST                    engine move, seconds
//!   fixture KIND                             creates a phase 2 fixture, prints its path(s)
//!   find ROOT NAME TEXT CASE RUNS            engine search (P-10, P-11): TEXT `-` for none,
//!                                            CASE `case` or `fold`; complete and first-batch
//!                                            time, ms (one run: for hyperfine)
//!   rss-results BIN TREE DIR N RESTATS       P-6b: RSS with a results tab of the N entries of
//!                                            TREE, then with both panels on DIR; key-to-flush
//!                                            of RESTATS Ctrl+R in the results tab, and the
//!                                            RSS after them
//!   filter BIN DIR ENTRY KEYS                P-12: key-to-flush of quick-filter keystrokes
//!   dirs-dialog BIN DIR TSV KEYS             P-15: key-to-flush of Ctrl+D and its keystrokes
//!                                            with TSV as `dirs.tsv`
//!   p3-* and delay-pipe                      the phase 3 checks (`benches/p3/mod.rs`)

#[allow(dead_code)]
mod common;
mod p3;

use expectrl::Session;
use manycommander::find::{self, FindMsg, FindSpec, Search};
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, run_guarded};
use manycommander::fsops::question::{Answer, Interaction, Progress, Question};
use manycommander::fsops::sys::Sys;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
        Tui::spawn_with(bin, args, None)
    }

    /// With `dirs_tsv`, that file becomes the session's frecency store (P-15).
    fn spawn_with(bin: &str, args: &[&str], dirs_tsv: Option<&Path>) -> Tui {
        let state = scratch("state").join("manycommander");
        std::fs::create_dir_all(&state).unwrap();
        let _ = std::fs::remove_file(state.join("dirs.tsv"));
        if let Some(tsv) = dirs_tsv {
            std::fs::copy(tsv, state.join("dirs.tsv")).unwrap();
        }
        // zoxide's ranking is never read: it is the user's state (P2 3.3).
        let config = scratch("config").join("manycommander");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("config.toml"), "[jump]\nzoxide = \"off\"\n").unwrap();
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            // The pty is the terminal, not the tmux the benchmark may run in: with `TMUX` or
            // `TERM_PROGRAM=tmux` inherited, the startup probe (P3 4.2) reads on after DA1
            // for a graphics reply this terminal never sends, up to its 100 ms deadline.
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TERM_PROGRAM")
            .env_remove("TERM_PROGRAM_VERSION")
            .env("PATH", no_desktop_path())
            // Session state and config of their own: a benchmark never reads the user's
            // config nor writes the user's state.toml (its tabs would be restored).
            .env("XDG_STATE_HOME", scratch("state"))
            .env("XDG_CONFIG_HOME", scratch("config"));
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

/// A per-run directory under the temp directory.
fn scratch(what: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mc-bench-{what}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `PATH` with a no-op `gio` and `xdg-open` first: a benchmark never opens anything on the
/// desktop.
fn no_desktop_path() -> std::ffi::OsString {
    let dir = std::env::temp_dir().join(format!("mc-bench-stub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["gio", "xdg-open"] {
        let stub = dir.join(name);
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut p = dir.into_os_string();
    p.push(":");
    p.push(std::env::var_os("PATH").unwrap_or_default());
    p
}

/// Removes this run's directories under the temp directory.
fn clean_scratch() {
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    for what in ["state", "config", "stub"] {
        let _ = std::fs::remove_dir_all(tmp.join(format!("mc-bench-{what}-{pid}")));
    }
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

/// `MC_BENCH_TABS=N`: open N-1 more tabs on both sides first (M2 re-runs of A-P-1 and
/// A-P-6). Each new tab duplicates the current one; the hidden ones release their listing.
fn open_tabs(t: &mut Tui) {
    let n: usize = std::env::var("MC_BENCH_TABS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    for _side in 0..2 {
        for _ in 1..n {
            t.send(b"\x1b[116;5u");
            std::thread::sleep(Duration::from_millis(300));
            t.pump();
        }
        t.send(b"\t");
        std::thread::sleep(Duration::from_millis(100));
    }
    if n > 1 {
        assert!(
            t.wait_for(&format!("{n}:"), Duration::from_secs(10)),
            "tab bar"
        );
        println!("tabs per panel: {n}");
    }
}

fn first_frame(bin: &str, left: &str, right: &str, runs: usize, dirs_tsv: Option<&Path>) {
    let mut v = Vec::new();
    for _ in 0..runs {
        let log = temp_log("ff");
        let mut t = Tui::spawn_with(
            bin,
            &[
                "--log",
                log.to_str().unwrap(),
                "--exit-after-first-frame",
                left,
                right,
            ],
            dirs_tsv,
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
    open_tabs(&mut t);
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
    t.send(CTRL_E);
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

/// RSS with both panels loaded; then, with `refreshes`, after that many Ctrl+R (both
/// panels re-listed), one per 1.5 s.
fn rss(bin: &str, left: &str, right: &str, refreshes: usize) {
    let mut t = Tui::spawn(bin, &[left, right]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)));
    open_tabs(&mut t);
    wait_entries(&mut t, "100000 entries", 2);
    std::thread::sleep(Duration::from_millis(500));
    t.pump();
    print!("rss_mb={:.1}", rss_mb(t.pid()));
    if refreshes > 0 {
        for _ in 0..refreshes {
            t.send(CTRL_R);
            std::thread::sleep(Duration::from_millis(1500));
            t.pump();
        }
        print!(
            " after_refreshes_mb={:.1} refreshes={refreshes}",
            rss_mb(t.pid())
        );
    }
    println!();
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
}

/// The key-to-flush times of every frame logged so far, ms, in order.
fn frames(log: &Path) -> Vec<f64> {
    log_values(log, " frame", "key_to_flush_us")
        .iter()
        .map(|u| u / 1000.0)
        .collect()
}

/// `p50`, `p99` and `max` of `v`, ms.
fn summary(v: &[f64]) -> String {
    let mut v = v.to_vec();
    let max = v.iter().cloned().fold(0.0, f64::max);
    format!(
        "p50_ms={:.2} p99_ms={:.2} max_ms={max:.2} samples={}",
        percentile(&mut v, 0.5),
        percentile(&mut v, 0.99),
        v.len()
    )
}

/// Waits until the listing of a 100k-entry directory is complete on `panels` panels.
fn wait_entries(t: &mut Tui, text: &str, panels: usize) {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        t.pump();
        if t.parser.screen().contents().matches(text).count() >= panels {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "{panels} panels did not show {text:?}:\n{}",
        t.parser.screen().contents()
    );
}

fn rss_mb(pid: i32) -> f64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let kb: f64 = s
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap();
    kb / 1024.0
}

const CTRL_R: &[u8] = b"\x1b[114;5u";
const CTRL_F: &[u8] = b"\x1b[102;5u";
/// Gives the command line the focus: typing goes to the quick filter without it.
const CTRL_E: &[u8] = b"\x1b[101;5u";
const CTRL_D: &[u8] = b"\x1b[100;5u";
const CTRL_1: &[u8] = b"\x1b[49;5u";
const CTRL_2: &[u8] = b"\x1b[50;5u";
const ALT_F7: &[u8] = b"\x1b[18;3~";
const ESC: &[u8] = b"\x1b[27u";
const BACKSPACE: &[u8] = b"\x7f";

/// P-6b and the re-stat's UI share. A find of every name below `tree` (`n` entries) opens
/// a results tab on the left, beside `dir` on the right (`results_mb`). The left panel's
/// directory tab then goes to `dir`, so both panels show 100k entries while the hidden
/// results tab keeps its results, as results tabs are not released (`dirs_mb`: P-6b).
/// Back in the results tab, `restats` Ctrl+R, one per 1.5 s, re-stat the results and
/// re-list the right panel: the key-to-flush of each (`restat_*`), and the RSS after them
/// in the same two states (`restats_mb`, `dirs_after_mb`).
fn rss_results(bin: &str, tree: &str, dir: &str, n: usize, restats: usize) {
    let log = temp_log("rssr");
    let mut t = Tui::spawn(bin, &["--log", log.to_str().unwrap(), tree, dir]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)), "no UI");
    wait_entries(&mut t, "100000 entries", 1);
    // Alt+F7, then Enter: an empty name finds every entry below the panel's directory.
    t.send(ALT_F7);
    assert!(
        t.wait_for("Find files", Duration::from_secs(5)),
        "no find form"
    );
    t.send(b"\r");
    let done = format!("{n} results");
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        t.pump();
        let s = t.parser.screen().contents();
        if s.contains(&done) && !s.contains("(searching)") {
            break;
        }
        assert!(Instant::now() < end, "the search did not finish:\n{s}");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    t.pump();
    let with_results = rss_mb(t.pid());
    // The first tab, into `dir`.
    let to_dirs = |t: &mut Tui| {
        t.send(CTRL_1);
        std::thread::sleep(Duration::from_millis(300));
        t.send(CTRL_E);
        t.send(format!("cd {dir}\r").as_bytes());
        wait_entries(t, "100000 entries", 2);
        std::thread::sleep(Duration::from_millis(500));
        t.pump();
        rss_mb(t.pid())
    };
    let with_dirs = to_dirs(&mut t);
    // Back to the results tab (showing it re-stats it once), then Ctrl+R.
    t.send(CTRL_2);
    assert!(t.wait_for(&done, Duration::from_secs(10)), "no results tab");
    std::thread::sleep(Duration::from_millis(1500));
    t.pump();
    let before = frames(&log).len();
    for _ in 0..restats {
        t.send(CTRL_R);
        std::thread::sleep(Duration::from_millis(1500));
        t.pump();
    }
    let lat = frames(&log)[before..].to_vec();
    assert!(
        t.parser.screen().contents().contains(&done),
        "the re-stat lost results:\n{}",
        t.parser.screen().contents()
    );
    let after_restats = rss_mb(t.pid());
    let dirs_after = to_dirs(&mut t);
    println!(
        "rss-results results_mb={with_results:.1} dirs_mb={with_dirs:.1} restats_mb={after_restats:.1} dirs_after_mb={dirs_after:.1} restat_{}",
        summary(&lat)
    );
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
    let _ = std::fs::remove_file(&log);
}

/// P-12 on a pty: in `dir/entry` (100k entries), Ctrl+F and `keys` keystrokes, 30 ms apart:
/// `f0123` typed, then deleted, in turn.
fn filter(bin: &str, dir: &str, entry: &str, keys: usize) {
    let log = temp_log("filter");
    let mut t = Tui::spawn(bin, &["--log", log.to_str().unwrap(), dir, dir]);
    assert!(t.wait_for("10Quit", Duration::from_secs(10)), "no UI");
    t.send(CTRL_E);
    t.send(format!("cd {entry}\r").as_bytes());
    wait_entries(&mut t, "100000 entries", 1);
    std::thread::sleep(Duration::from_millis(300));
    let before = frames(&log).len();
    t.send(CTRL_F);
    std::thread::sleep(Duration::from_millis(100));
    let typed = b"f0123";
    for i in 0..keys {
        let k = i % (2 * typed.len());
        if k < typed.len() {
            t.send(&typed[k..k + 1]);
        } else {
            t.send(BACKSPACE);
        }
        std::thread::sleep(Duration::from_millis(30));
        t.pump();
        if i == typed.len() - 1 {
            assert!(
                t.wait_for("(filter: f0123)", Duration::from_secs(5)),
                "the filter is not applied:\n{}",
                t.parser.screen().contents()
            );
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    let lat = frames(&log)[before..].to_vec();
    println!("filter {}", summary(&lat));
    t.send(ESC);
    std::thread::sleep(Duration::from_millis(100));
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
    let _ = std::fs::remove_file(&log);
}

/// P-15 on a pty: with `tsv` as the frecency store, Ctrl+D (the dialog ranks the store),
/// then `keys` keystrokes, 30 ms apart: `src mo` typed, then deleted, in turn.
fn dirs_dialog(bin: &str, dir: &str, tsv: &str, keys: usize) {
    let log = temp_log("dirs");
    let mut t = Tui::spawn_with(
        bin,
        &["--log", log.to_str().unwrap(), dir, dir],
        Some(Path::new(tsv)),
    );
    assert!(t.wait_for("10Quit", Duration::from_secs(10)), "no UI");
    // The store loads after the first frame.
    std::thread::sleep(Duration::from_millis(1000));
    t.pump();
    let before = frames(&log).len();
    t.send(CTRL_D);
    assert!(
        t.wait_for("Go to directory", Duration::from_secs(5)),
        "no dialog"
    );
    // The store's entries are listed (they live under /home/me).
    assert!(
        t.wait_for("/home/me/", Duration::from_secs(5)),
        "the store is not listed:\n{}",
        t.parser.screen().contents()
    );
    std::thread::sleep(Duration::from_millis(200));
    let open = frames(&log)[before..].to_vec();
    let before = frames(&log).len();
    let typed = b"src mo";
    for i in 0..keys {
        let k = i % (2 * typed.len());
        if k < typed.len() {
            t.send(&typed[k..k + 1]);
        } else {
            t.send(BACKSPACE);
        }
        std::thread::sleep(Duration::from_millis(30));
        t.pump();
    }
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    let lat = frames(&log)[before..].to_vec();
    assert!(
        !t.parser.screen().contents().contains("loading"),
        "the store did not load"
    );
    println!(
        "dirs-dialog open_ms={:.2} {}",
        open.iter().cloned().fold(0.0, f64::max),
        summary(&lat)
    );
    t.send(ESC);
    std::thread::sleep(Duration::from_millis(100));
    t.send(F10);
    t.wait_exit(Duration::from_secs(10));
    let _ = std::fs::remove_file(&log);
}

/// One engine search: complete time, first batch time (none without results), results.
fn search_once(spec: &FindSpec) -> (f64, Option<f64>, u64) {
    let search = Search::new(1, spec.clone());
    let first = Mutex::new(None);
    let results = AtomicU64::new(0);
    let start = Instant::now();
    find::run(&search, &|m| {
        if let FindMsg::Batch { entries, .. } = m {
            let mut f = first.lock().unwrap();
            if f.is_none() {
                *f = Some(start.elapsed().as_secs_f64() * 1000.0);
            }
            results.fetch_add(entries.len() as u64, Ordering::Relaxed);
        }
    });
    let total = start.elapsed().as_secs_f64() * 1000.0;
    let stats = search.stats().expect("finished");
    assert!(stats.error.is_none() && stats.errors == 0, "{stats:?}");
    (
        total,
        first.into_inner().unwrap(),
        results.load(Ordering::Relaxed),
    )
}

/// P-10, P-11: the find engine as the find form starts it (hidden entries, stay on this
/// filesystem). One run for hyperfine; otherwise a warm-up and `runs` timed runs.
fn find_bench(root: &str, name: &str, text: &str, case: &str, runs: usize) {
    let spec = FindSpec {
        root: root.into(),
        name: name.as_bytes().to_vec(),
        content: (text != "-").then(|| text.as_bytes().to_vec()),
        hidden: true,
        stay_on_fs: true,
        match_case: case == "case",
    };
    if runs <= 1 {
        let (t, _, n) = search_once(&spec);
        println!("find complete_ms={t:.2} results={n}");
        return;
    }
    search_once(&spec);
    let (mut all, mut first, mut n) = (Vec::new(), Vec::new(), 0);
    for _ in 0..runs {
        let (t, f, r) = search_once(&spec);
        all.push(t);
        first.push(f.unwrap_or(f64::NAN));
        n = r;
    }
    let max = all.iter().cloned().fold(0.0, f64::max);
    println!(
        "find complete_ms={:.2} complete_max_ms={max:.2} first_ms={:.2} results={n} runs={runs} workers={}",
        percentile(&mut all, 0.5),
        percentile(&mut first, 0.5),
        find::workers()
    );
}

/// `fixture KIND`: creates a phase 2 fixture (full size) and prints its path(s).
fn fixture(kind: &str) {
    match kind {
        "tree" => println!("{}", common::tree(100_000).display()),
        "text" => println!("{}", common::text_tree(10_000, 1 << 30).display()),
        "hardlinks" => {
            let (hl, nohl) = common::hardlinks(10_000, 4096);
            println!("{} {}", hl.display(), nohl.display());
        }
        "sparse" => println!("{}", common::sparse("sparse16g", 16 << 30, 8).display()),
        "dirs-tsv" => {
            let p = common::p2().join("dirs5000.tsv");
            std::fs::create_dir_all(common::p2()).unwrap();
            std::fs::write(&p, common::dirs_tsv(5000, manycommander::dirs::now())).unwrap();
            println!("{}", p.display());
        }
        "needle" => println!("{}", common::NEEDLE),
        _ => panic!("unknown fixture {kind}"),
    }
}

struct Silent;

impl Interaction for Silent {
    fn ask(&mut self, q: Question) -> Answer {
        panic!("unexpected question: {q:?}");
    }
    fn progress(&mut self, _: Progress) {}
}

fn engine(moving: bool, src: &str, name: &str, dst: &str) {
    let groups = vec![Group::new(src, vec![name.into()])];
    let spec = if moving {
        JobSpec::Move {
            groups,
            dst: dst.into(),
        }
    } else {
        JobSpec::Copy {
            groups,
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
        ["first-frame", bin, l, r, runs] => first_frame(bin, l, r, runs.parse().unwrap(), None),
        ["first-frame", bin, l, r, runs, tsv] => {
            first_frame(bin, l, r, runs.parse().unwrap(), Some(Path::new(tsv)))
        }
        ["navigate", bin, dir, entry, keys] => {
            navigate(bin, dir, entry, keys.parse().unwrap(), None)
        }
        ["navigate", bin, dir, entry, keys, copy, dst] => {
            navigate(bin, dir, entry, keys.parse().unwrap(), Some((copy, dst)))
        }
        ["idle", bin, dir, secs] => idle(bin, dir, secs.parse().unwrap()),
        ["rss", bin, l, r] => rss(bin, l, r, 0),
        ["rss", bin, l, r, refreshes] => rss(bin, l, r, refreshes.parse().unwrap()),
        ["copy", src, name, dst] => engine(false, src, name, dst),
        ["move", src, name, dst] => engine(true, src, name, dst),
        ["fixture", kind] => fixture(kind),
        ["find", root, name, text, case, runs] => {
            find_bench(root, name, text, case, runs.parse().unwrap())
        }
        ["rss-results", bin, tree, dir, n, restats] => {
            rss_results(bin, tree, dir, n.parse().unwrap(), restats.parse().unwrap())
        }
        ["filter", bin, dir, entry, keys] => filter(bin, dir, entry, keys.parse().unwrap()),
        ["dirs-dialog", bin, dir, tsv, keys] => dirs_dialog(bin, dir, tsv, keys.parse().unwrap()),
        // The phase 3 checks; `cargo test --all-targets` runs benches without arguments.
        a => {
            p3::run(a);
        }
    }
    clean_scratch();
}
