//! P-18 to P-22 and P-6c: archive listing, the same process's decompress-only run, scan
//! cancel, navigation inside a cached index, extraction, and memory.

use super::pty::{BACKSPACE, DOWN, END, ENTER, ESC, HOME, Opts, Pty, TAB, log_file};
use super::{field, frames, log_lines, median, ms, rss_mb, summary};
use manycommander::archive::detect::Want;
use manycommander::archive::index::Limits;
use manycommander::archive::{self, ArchiveIndex, IndexCache, OpenRequest, PosReader};
use manycommander::fsops::group::{Group, Root};
use manycommander::fsops::job::{Dest, JobSpec, run_guarded};
use manycommander::fsops::sys::Sys;
use manycommander::panel::listing::ListingMsg;
use manycommander::provider::VPath;
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn request(path: &Path) -> OpenRequest {
    OpenRequest {
        slot: 0,
        generation: 1,
        archive: path.to_path_buf(),
        want: Want::of_name(path.file_name().unwrap().as_encoded_bytes()),
        inner: VPath::root(),
        cancel: Arc::new(AtomicBool::new(false)),
        tz: jiff::tz::TimeZone::UTC,
        limits: Limits::default(),
    }
}

/// One scan with a fresh cache, as a panel's listing thread runs it: the time to the first
/// rows, to the end of `open` (the index complete), and the index.
fn scan(path: &Path) -> (f64, f64, Arc<ArchiveIndex>, usize) {
    let first = Cell::new(None);
    let rows = Cell::new(0usize);
    let index = RefCell::new(None);
    let start = Instant::now();
    archive::open(&request(path), &IndexCache::default(), &|m| match m {
        ListingMsg::Batch { entries, .. } => {
            if first.get().is_none() {
                first.set(Some(start.elapsed()));
            }
            rows.set(rows.get() + entries.len());
        }
        ListingMsg::Opened { index: i, .. } => *index.borrow_mut() = Some(i),
        ListingMsg::Failed { error, .. } => panic!("{}: {error}", path.display()),
        _ => {}
    });
    let all = start.elapsed();
    let ix = index.into_inner().expect("opened");
    assert!(ix.is_complete(), "{}", path.display());
    (first.get().map_or(f64::NAN, ms), ms(all), ix, rows.get())
}

/// The scan's decoder alone (P-19), built as `archive::tar::decoder` builds it, over the
/// same positioned reader, with its output read in the 32 KiB pieces the tar crate skips
/// data in and discarded. Returns the decompressed bytes.
fn decompress_only(path: &Path) -> u64 {
    let file = Arc::new(std::fs::File::open(path).unwrap());
    let len = file.metadata().unwrap().len();
    let base = PosReader::new(file, len);
    let name = path.to_string_lossy();
    let mut r: Box<dyn Read> = if name.ends_with(".tar.gz") {
        Box::new(flate2::read::MultiGzDecoder::new(base))
    } else if name.ends_with(".tar.zst") {
        let mut d = zstd::stream::read::Decoder::new(base).unwrap();
        d.window_log_max(archive::ZSTD_WINDOW_LOG_MAX).unwrap();
        Box::new(d)
    } else if name.ends_with(".tar.xz") {
        Box::new(lzma_rust2::XzReader::new(
            std::io::BufReader::with_capacity(64 << 10, base),
            true,
        ))
    } else if name.ends_with(".tar.bz2") {
        Box::new(bzip2::read::MultiBzDecoder::new(base))
    } else {
        Box::new(base)
    };
    let mut buf = vec![0u8; 32 << 10];
    let mut n = 0u64;
    loop {
        match r.read(&mut buf) {
            Ok(0) => return n,
            Ok(k) => n += k as u64,
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
}

/// The system tool's `-dc` to `/dev/null`, for reference beside P-19.
fn tool_ms(path: &Path) -> Option<f64> {
    let name = path.to_string_lossy();
    let tool = [
        (".tar.gz", "gzip"),
        (".tar.zst", "zstd"),
        (".tar.xz", "xz"),
        (".tar.bz2", "bzip2"),
    ]
    .iter()
    .find(|(e, _)| name.ends_with(e))?
    .1;
    let t = Instant::now();
    let ok = std::process::Command::new(tool)
        .arg("-dc")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .ok()?
        .success();
    ok.then(|| ms(t.elapsed()))
}

/// `p3-list ARCHIVE RUNS`: P-18 and P-19. A warm-up, then RUNS scans, RUNS decompress-only
/// runs and RUNS tool runs, interleaved; medians.
pub fn list(path: &str, runs: usize) {
    let path = Path::new(path);
    let tarlike =
        !path.to_string_lossy().ends_with(".zip") && !path.to_string_lossy().ends_with(".7z");
    let (_, _, ix, _) = scan(path);
    let nodes = ix.tree().map_or(0, |t| t.len());
    if tarlike {
        decompress_only(path);
    }
    let (mut first, mut all, mut dec, mut tool) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut rows = 0;
    let mut bytes = 0;
    for _ in 0..runs {
        let (f, a, _, r) = scan(path);
        first.push(f);
        all.push(a);
        rows = r;
        if tarlike {
            let t = Instant::now();
            bytes = decompress_only(path);
            dec.push(ms(t.elapsed()));
            if let Some(t) = tool_ms(path) {
                tool.push(t);
            }
        }
    }
    let size = std::fs::metadata(path).unwrap().len();
    print!(
        "p3-list first_ms={:.2} scan_ms={:.2} scan_max_ms={:.2} nodes={nodes} root_rows={rows} bytes={size} runs={runs}",
        median(&first),
        median(&all),
        super::max(&all)
    );
    if tarlike {
        let d = median(&dec);
        print!(
            " decompress_ms={d:.2} ratio={:.3} tar_bytes={bytes}",
            median(&all) / d
        );
        if !tool.is_empty() {
            print!(" tool_ms={:.2}", median(&tool));
        }
    }
    println!();
}

/// `p3-extract ARCHIVE DST`: P-22, one run for hyperfine: scan the archive, then extract
/// every root name into DST as F5 does.
pub fn extract(path: &str, dst: &str) {
    let path = Path::new(path);
    let t = Instant::now();
    let (_, scan_ms, ix, _) = scan(path);
    let job = Instant::now();
    let r = run_guarded(
        JobSpec::Copy {
            groups: vec![Group {
                root: Root::Archive(ix.clone()),
                sub: Vec::new(),
                names: archive::root_names(&ix),
            }],
            dst: Dest::Local(dst.into()),
        },
        &Sys::default(),
        &mut crate::Silent,
    );
    assert!(r.failed == 0 && r.refused.is_none(), "{r:?}");
    println!(
        "p3-extract scan_ms={scan_ms:.1} job_s={:.3} total_s={:.3} done={} dirs={}",
        job.elapsed().as_secs_f64(),
        t.elapsed().as_secs_f64(),
        r.done,
        r.dirs_done
    );
}

/// The archive panel's title shows once the archive is open: `<archive>:/`.
fn opened(p: &mut Pty, name: &str) -> bool {
    p.screen().contains(&format!("{name}:/"))
}

/// Thread ids of `pid`'s panel listing threads (`list-<slot>`), which run archive scans.
fn threads(pid: i32) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|t| {
            std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| {
                c.trim_end()
                    .strip_prefix("list-")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .map(|t| t.file_name().to_string_lossy().into_owned())
        .collect()
}

/// `p3-cancel BIN ARCHIVE...`: P-20. Each archive alone in a directory; `End`, `Enter`,
/// 150 ms of scanning, then `Esc`. The key-to-flush of the `Esc` frame (the panel is back),
/// and how long the scan's listing thread took to end after it.
pub fn cancel(bin: &str, archives: &[&str]) {
    for a in archives {
        let src = Path::new(a);
        let name = src.file_name().unwrap().to_string_lossy().into_owned();
        let dir = super::run_dir("cancel");
        std::fs::hard_link(src, dir.join(&name)).unwrap();
        let log = log_file("cancel");
        let d = dir.to_string_lossy().into_owned();
        let mut p = Pty::spawn(
            bin,
            &["--log", log.to_str().unwrap(), &d, &d],
            &Opts::default(),
        );
        p.expect("10Quit", Duration::from_secs(10));
        p.keys(&[END], Duration::from_millis(100));
        let before = frames(&log).len();
        p.send(ENTER);
        assert!(
            p.until(Duration::from_secs(10), |p| opened(p, &name)),
            "the archive did not open:\n{}",
            p.screen()
        );
        p.idle(Duration::from_millis(150));
        let scanning = threads(p.pid());
        let complete = log_lines(&log, "archive scan done").len();
        p.send(ESC);
        let sent = Instant::now();
        let mut gone_ms = f64::NAN;
        while sent.elapsed() < Duration::from_secs(10) {
            p.pump();
            let now = threads(p.pid());
            if scanning.iter().all(|t| !now.contains(t)) {
                gone_ms = ms(sent.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        p.idle(Duration::from_millis(300));
        let back = !opened(&mut p, &name);
        let esc = frames(&log)[before..].to_vec();
        // The Enter frame, then the Esc frame: the latest key frame is the Esc's.
        let esc_ms = esc.last().copied().unwrap_or(f64::NAN);
        println!(
            "p3-cancel archive={name} esc_ms={esc_ms:.2} thread_end_ms={gone_ms:.1} back={back} scanning_at_esc={} complete_before_esc={}",
            !scanning.is_empty(),
            complete > 0
        );
        p.quit();
        drop(p);
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `p3-inside BIN ARCHIVE DIR CYCLES`: P-21. Opens ARCHIVE (its root holds DIR), waits for
/// the complete index, then enters and leaves DIR (10,000 entries) CYCLES times, and leaves
/// and re-enters the archive CYCLES times (a cache hit). Key-to-flush of every key, and the
/// scans the log shows.
pub fn inside(bin: &str, path: &str, inner: &str, cycles: usize) {
    let src = Path::new(path);
    let name = src.file_name().unwrap().to_string_lossy().into_owned();
    let dir = super::run_dir("inside");
    std::fs::hard_link(src, dir.join(&name)).unwrap();
    let log = log_file("inside");
    let d = dir.to_string_lossy().into_owned();
    let mut p = Pty::spawn(
        bin,
        &["--log", log.to_str().unwrap(), &d, &d],
        &Opts::default(),
    );
    p.expect("10Quit", Duration::from_secs(10));
    p.keys(&[END, ENTER], Duration::from_millis(100));
    assert!(
        p.until(Duration::from_secs(30), |_| !log_lines(
            &log,
            "archive scan done"
        )
        .is_empty()),
        "the scan did not end"
    );
    p.idle(Duration::from_millis(300));
    // The archive root lists the one top directory; into it.
    p.keys(&[END, ENTER], Duration::from_millis(200));
    p.idle(Duration::from_millis(300));
    let at_root = p.screen();
    assert!(at_root.contains(inner), "{inner} is not listed:\n{at_root}");
    let before = frames(&log).len();
    let mut entered = 0;
    for _ in 0..cycles {
        // `big` is the first directory after `..`.
        p.keys(&[HOME, DOWN, ENTER], Duration::from_millis(40));
        if p.until(Duration::from_secs(5), |p| {
            p.screen().contains("10000 entries")
        }) {
            entered += 1;
        }
        p.idle(Duration::from_millis(60));
        p.keys(&[BACKSPACE], Duration::from_millis(100));
    }
    let nav = frames(&log)[before..].to_vec();
    // Out of the archive and back in: the cached index.
    let before = frames(&log).len();
    for _ in 0..cycles {
        p.keys(&[BACKSPACE], Duration::from_millis(60));
        p.keys(&[BACKSPACE], Duration::from_millis(100));
        p.keys(&[END, ENTER], Duration::from_millis(100));
        p.idle(Duration::from_millis(50));
    }
    let reopen = frames(&log)[before..].to_vec();
    p.idle(Duration::from_millis(300));
    let scans = log_lines(&log, "archive scan done").len();
    let scan_ms = log_lines(&log, "archive scan done")
        .first()
        .and_then(|l| field(&l.text, "ms"))
        .unwrap_or(f64::NAN);
    println!(
        "p3-inside nav_{} entered={entered}/{cycles} reopen_{} scans={scans} first_scan_ms={scan_ms:.1}",
        summary(&nav),
        summary(&reopen)
    );
    p.quit();
    drop(p);
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `p3-rss BIN LEFT RIGHT ARCHIVE [SSHCFG]`: P-6c. The left panel opens ARCHIVE (100,000
/// entries), waits for the complete index and leaves it (the index stays cached), then goes
/// to LEFT; the right panel shows RIGHT, a local directory or, with SSHCFG, an
/// `sftp://` address through `sshd -i`. Quick view off; no preview was ever prepared.
pub fn rss(bin: &str, left: &str, right: &str, path: &str, cfg: Option<&str>) {
    let src = Path::new(path);
    let name = src.file_name().unwrap().to_string_lossy().into_owned();
    let dir = super::run_dir("rss");
    std::fs::hard_link(src, dir.join(&name)).unwrap();
    let log = log_file("rss");
    let d = dir.to_string_lossy().into_owned();
    let config = cfg.map(super::sftp::ssh_setting).unwrap_or_default();
    let remote = right.starts_with("sftp://");
    let right_start = if remote { d.as_str() } else { right };
    let mut p = Pty::spawn(
        bin,
        &["--log", log.to_str().unwrap(), &d, right_start],
        &Opts {
            config: &config,
            ..Opts::default()
        },
    );
    p.expect("10Quit", Duration::from_secs(10));
    p.keys(&[END, ENTER], Duration::from_millis(100));
    assert!(
        p.until(Duration::from_secs(60), |_| !log_lines(
            &log,
            "archive scan done"
        )
        .is_empty()),
        "the scan did not end"
    );
    p.idle(Duration::from_millis(500));
    p.keys(&[BACKSPACE], Duration::from_millis(300));
    super::sftp::run_line(&mut p, &format!("cd {left}"));
    let want = |p: &mut Pty, n: usize| {
        p.until(Duration::from_secs(120), |p| {
            p.screen().matches("100000 entries").count() >= n
        })
    };
    if remote {
        p.keys(&[TAB], Duration::from_millis(100));
        super::sftp::run_line(&mut p, &format!("cd {right}"));
    }
    assert!(want(&mut p, 2), "both panels did not load:\n{}", p.screen());
    p.idle(Duration::from_millis(1000));
    let mb = rss_mb(p.pid());
    let scans = log_lines(&log, "archive scan done").len();
    let previews = log_lines(&log, "preview stages").len();
    println!("p3-rss rss_mb={mb:.1} remote={remote} scans={scans} previews={previews}");
    p.quit();
    drop(p);
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_dir_all(&dir);
}
