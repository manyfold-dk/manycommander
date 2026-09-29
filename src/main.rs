#![forbid(unsafe_code)]
//! manycommander: argument parsing, then the runtime. The allocator is tuned and the signal
//! handlers are registered first, before any other thread starts.

use manycommander::app::runtime::{USAGE, parse_args, run};
use manycommander::app::signals;
use std::time::Instant;

fn main() {
    let start = Instant::now();
    // Before any thread exists (P-6).
    manycommander::fsops::sys::tune_allocator();
    let signals = match signals::register() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("manycommander: cannot register signal handlers: {e}");
            std::process::exit(1);
        }
    };
    let opts = match parse_args(std::env::args_os()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("manycommander: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    if opts.help {
        println!("{USAGE}");
        return;
    }
    if opts.version {
        println!("manycommander {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match run(opts, signals, start) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("manycommander: {e}");
            std::process::exit(1);
        }
    }
}
