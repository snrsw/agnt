//! How long the embedded shell takes to be ready to type into.
//!
//! Run it when shell mode feels slow to open:
//!
//! ```sh
//! cargo run --example shell_timing            # the shell agnt would pick
//! cargo run --example shell_timing -- /bin/sh # something to compare against
//! ```
//!
//! The time is almost always the shell's own configuration rather than agnt —
//! a `direnv` hook that sources a nix dev shell costs seconds every time a
//! shell starts, in a terminal as much as here. Comparing against `/bin/sh`,
//! which loads nothing, says which of the two it is.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use agnt::shell::{self, PtyShell};

/// How long the shell must stay quiet before its prompt is called finished.
/// Long enough to sit through a slow `direnv`, which goes quiet between the
/// lines it prints.
const SETTLED: Duration = Duration::from_secs(3);

fn main() {
    let cwd = std::env::current_dir().unwrap();
    let mut shells: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if shells.is_empty() {
        shells.push(shell::program());
    }

    println!("cwd: {}", cwd.display());
    println!("(a shell's startup is its own configuration; agnt only waits for it)\n");
    for sh in &shells {
        time_one(sh, &cwd);
    }
}

fn time_one(program: &Path, cwd: &Path) {
    let started = Instant::now();
    // Microseconds since the start, written by the reader thread.
    let first = Arc::new(AtomicU64::new(0));
    let last = Arc::new(AtomicU64::new(0));
    let (f, l) = (Arc::clone(&first), Arc::clone(&last));

    let shell = PtyShell::spawn(program, cwd, 12, 100, move || {
        let us = started.elapsed().as_micros() as u64;
        let _ = f.compare_exchange(0, us, Ordering::SeqCst, Ordering::SeqCst);
        l.store(us, Ordering::SeqCst);
    });
    let Ok(shell) = shell else {
        println!("{}: could not start", program.display());
        return;
    };

    let mut seen = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let now = shell
            .parser()
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default();
        if now != seen {
            // Each step the shell takes, so a long pause has a line before it
            // naming what the shell was doing when it stopped.
            println!(
                "  {:>6.2}s  {}",
                started.elapsed().as_secs_f64(),
                now.lines().next_back().unwrap_or("").trim()
            );
            seen = now;
        }

        let idle = last.load(Ordering::SeqCst);
        if idle > 0 && started.elapsed().as_micros() as u64 - idle > SETTLED.as_micros() as u64 {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            println!("  gave up waiting");
            break;
        }
    }

    let secs = |us: u64| us as f64 / 1_000_000.0;
    println!(
        "\n{}: first output {:.2}s, ready {:.2}s\n",
        program.display(),
        secs(first.load(Ordering::SeqCst)),
        secs(last.load(Ordering::SeqCst)),
    );
}
