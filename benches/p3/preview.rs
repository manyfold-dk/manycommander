//! P-23, P-24 and P-25: the quick view on a pty whose terminal answers the probe as
//! Ghostty (kitty graphics), foot (sixel) or a truecolor terminal without graphics
//! (halfblocks) does.
//!
//! "On screen" is the wall time at which the terminal side has read the frame's last image
//! bytes: the kitty placement, the end of the sixel, or the last halfblock cell. "After
//! the debounce" starts at the log's `preview request` line, which the event loop writes
//! when the debounce deadline passes and the request goes to the preview thread.

use super::pty::{CTRL_Q, DOWN, Opts, Pty, Term, UP, log_file};
use super::{field, frames, log_lines, max, median, summary, wall};
use std::time::Duration;

/// The terminal size that gives the quick view a 100 x 50-cell image area (P-23).
pub const COLS: u16 = 204;
pub const ROWS: u16 = 55;

/// What shows an image for `protocol`: the count of kitty placements, of sixel ends, or of
/// halfblock cells.
fn signal(p: &Pty, protocol: &str) -> usize {
    match protocol {
        "kitty" => p.seen.placements.len(),
        "sixel" => p.seen.sixels.len(),
        _ => p.halfblocks(),
    }
}

/// Waits for the image the last key asked for; its wall time on screen.
fn wait_image(p: &mut Pty, protocol: &str, before: usize) -> Option<f64> {
    let t = Duration::from_secs(10);
    match protocol {
        "kitty" => p
            .until(t, |p| p.seen.placements.len() > before)
            .then(|| p.seen.placements[before]),
        "sixel" => p
            .until(t, |p| p.seen.sixels.len() > before)
            .then(|| p.seen.sixels[before]),
        _ => {
            // The card shows first (no halfblocks), then the image's cells; the image is on
            // screen when their count stops growing.
            if !p.until(t, |p| p.halfblocks() < 50) {
                return None;
            }
            let mut last = (0usize, wall());
            let ok = p.until(t, |p| {
                let n = p.halfblocks();
                if n != last.0 {
                    last = (n, wall());
                }
                n > 500 && wall() - last.1 > 0.15
            });
            ok.then_some(last.1)
        }
    }
}

/// The wall time of the last `preview request` before `at`.
fn request_at(log: &std::path::Path, at: f64) -> f64 {
    log_lines(log, "preview request")
        .iter()
        .filter(|l| l.at <= at)
        .map(|l| l.at)
        .fold(f64::NAN, f64::max)
}

/// `p3-preview BIN DIR PROTOCOL [RUNS]`: P-23. DIR holds `a.txt` and `photo1.jpg` ..
/// `photo4.jpg` (12 MP). With the quick view on, the cursor rests on the text file, then on
/// each photo (four first previews), then goes back up over them (three cache hits). For
/// each: request to on screen, ms. RUNS (default 1) such sessions, each a fresh process
/// with an empty cache; the numbers cover all of them.
pub fn latency(bin: &str, dir: &str, protocol: &str, runs: usize) {
    let mut cold = Vec::new();
    let mut hits = Vec::new();
    let (mut total, mut decode, mut prepare, mut bytes) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut chosen = String::new();
    for _ in 0..runs.max(1) {
        let log = log_file("preview");
        let term = Term::by_name(protocol);
        let mut p = Pty::spawn(
            bin,
            &["--log", log.to_str().unwrap(), dir, dir],
            &Opts {
                term,
                cols: COLS,
                rows: ROWS,
                config: "",
            },
        );
        p.expect("10Quit", Duration::from_secs(10));
        p.keys(&[CTRL_Q], Duration::from_millis(200));
        // `..`, then a.txt: the card.
        p.keys(&[DOWN], Duration::from_millis(400));
        let mut run_hits = 0;
        for dir_key in [DOWN, UP] {
            for _ in 0..4 {
                if dir_key == UP && run_hits == 3 {
                    break;
                }
                let before = signal(&p, protocol);
                p.send(dir_key);
                let shown = wait_image(&mut p, protocol, before)
                    .unwrap_or_else(|| panic!("no image on screen:\n{}", p.screen()));
                let asked = request_at(&log, shown);
                let v = (shown - asked) * 1000.0;
                if dir_key == DOWN {
                    cold.push(v);
                } else {
                    hits.push(v);
                    run_hits += 1;
                }
                p.idle(Duration::from_millis(300));
            }
        }
        let stages = log_lines(&log, "preview stages");
        let values =
            |key: &str| -> Vec<f64> { stages.iter().filter_map(|l| field(&l.text, key)).collect() };
        total.extend(values("total_ms"));
        decode.extend(values("decode_ms"));
        prepare.extend(values("prepare_ms"));
        bytes.extend(values("bytes"));
        let probe = log_lines(&log, "terminal probe");
        chosen = probe
            .first()
            .and_then(|l| l.text.split(" protocol=").nth(1))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("?")
            .trim_matches('"')
            .to_owned();
        p.quit();
        drop(p);
        let _ = std::fs::remove_file(&log);
    }
    println!(
        "p3-preview protocol={protocol} chosen={chosen} runs={runs} cold_ms={} cold_median_ms={:.1} cold_max_ms={:.1} hit_ms={} hit_max_ms={:.1} worker_ms={:.1} decode_ms={:.1} prepare_ms={:.1} bytes={:.0}",
        list(&cold),
        median(&cold),
        max(&cold),
        list(&hits),
        max(&hits),
        median(&total),
        median(&decode),
        median(&prepare),
        median(&bytes)
    );
}

fn list(v: &[f64]) -> String {
    v.iter()
        .map(|x| format!("{x:.1}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// `p3-burst BIN DIR IMAGES`: P-24 and P-1 while previews load. DIR holds IMAGES JPEGs of
/// 0.75 to 12 MP; kitty graphics. With the quick view on, bursts of 10 cursor moves at 30
/// keys/s, each followed by a rest of 300 ms, through all of them. Key-to-flush of every
/// key frame; for each kitty transmit, the log's `preview transmit` to the placement that
/// ends that frame on the terminal side; the threads that decoded.
pub fn burst(bin: &str, dir: &str, images: usize) {
    let log = log_file("burst");
    let mut p = Pty::spawn(
        bin,
        &["--log", log.to_str().unwrap(), dir, dir],
        &Opts {
            term: Term::ghostty(),
            cols: COLS,
            rows: ROWS,
            config: "",
        },
    );
    p.expect("10Quit", Duration::from_secs(10));
    p.keys(&[CTRL_Q], Duration::from_millis(300));
    let before = frames(&log).len();
    let mut moved = 0;
    while moved < images {
        for _ in 0..10.min(images - moved) {
            p.send(DOWN);
            p.idle(Duration::from_micros(33_333));
            moved += 1;
        }
        p.idle(Duration::from_millis(300));
    }
    p.idle(Duration::from_millis(1000));
    let keys = frames(&log)[before..].to_vec();
    let transmits = log_lines(&log, "preview transmit");
    let mut tx_ms = Vec::new();
    for t in &transmits {
        if let Some(pl) = p.seen.placements.iter().find(|&&x| x >= t.at) {
            tx_ms.push((pl - t.at) * 1000.0);
        }
    }
    let tx_bytes: Vec<f64> = transmits
        .iter()
        .filter_map(|l| field(&l.text, "bytes"))
        .collect();
    let stages = log_lines(&log, "preview stages");
    let mut threads: Vec<String> = stages.iter().map(|l| l.thread.clone()).collect();
    threads.sort();
    threads.dedup();
    let requests = log_lines(&log, "preview request").len();
    println!(
        "p3-burst keys_{} transmits={} transmit_frame_median_ms={:.1} transmit_frame_max_ms={:.1} transmit_bytes_median={:.0} requests={requests} prepared={} decode_threads={}",
        summary(&keys),
        tx_ms.len(),
        median(&tx_ms),
        max(&tx_ms),
        median(&tx_bytes),
        stages.len(),
        threads.join(",")
    );
    p.quit();
    drop(p);
    let _ = std::fs::remove_file(&log);
}

/// `p3-probe BIN LEFT RIGHT RUNS TERM`: P-25. Start to the first full frame, with the probe
/// answered as TERM answers it (`ghostty`, `foot`, `silent`), RUNS starts.
pub fn probe(bin: &str, left: &str, right: &str, runs: usize, term: &str) {
    let mut v = Vec::new();
    let mut probe_us = Vec::new();
    let mut chosen = String::new();
    for _ in 0..runs {
        let log = log_file("probe");
        let mut p = Pty::spawn(
            bin,
            &[
                "--log",
                log.to_str().unwrap(),
                "--exit-after-first-frame",
                left,
                right,
            ],
            &Opts {
                term: Term::by_name(term),
                ..Opts::default()
            },
        );
        p.until(Duration::from_secs(10), |_| {
            !log_lines(&log, "first full frame").is_empty()
        });
        p.idle(Duration::from_millis(50));
        let ff = log_lines(&log, "first full frame");
        v.push(
            ff.first()
                .and_then(|l| field(&l.text, "first_full_frame_us"))
                .expect("first-frame line")
                / 1000.0,
        );
        if let Some(l) = log_lines(&log, "terminal probe").first() {
            probe_us.push(field(&l.text, "probe_us").unwrap_or(f64::NAN) / 1000.0);
            chosen = l
                .text
                .split(" protocol=")
                .nth(1)
                .and_then(|x| x.split_whitespace().next())
                .unwrap_or("?")
                .trim_matches('"')
                .to_owned();
        }
        drop(p);
        let _ = std::fs::remove_file(&log);
    }
    println!(
        "p3-probe term={term} protocol={chosen} median={:.2} max={:.2} probe_ms={:.2} runs={runs}",
        median(&v),
        max(&v),
        median(&probe_us)
    );
}
