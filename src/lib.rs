//! minimenta: an interactive disk usage analyzer for the terminal. It runs on
//! macOS, Linux and Windows.
//!
//! This crate is a program, not a library. The binaries `minimenta` and its
//! short name `mm` both call [`main`]. Install it with
//! `cargo install --locked minimenta`. The
//! [README](https://github.com/OriginalMHV/minimenta#readme) explains the keys
//! and shows the speed results.

// Loading and updating a cache needs FSEvents, so only macOS uses most of it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod cache;
#[cfg(target_os = "macos")]
mod fsevents;
mod scan;
mod tree;
mod ui;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use cache::Source;
use scan::{Options, Progress};
use tree::format_size;

const HELP: &str = "\
minimenta: an interactive disk usage analyzer

Usage: minimenta [OPTIONS] [DIR]
       mm [OPTIONS] [DIR]

mm is a short name for minimenta. Without DIR, it asks which directory
to scan.

On macOS, minimenta keeps the last scan of each directory in
~/Library/Caches/minimenta. The next run lists again only the
directories that FSEvents reports as changed. Press r to rescan.

Options:
  -x, --one-file-system  Do not cross file system boundaries
  -t, --threads N        Number of scan threads (default: CPU count, at least 16)
      --no-cache         Always scan everything, and do not read or write the cache
      --no-mft           Do not read the NTFS master file table (Windows)
      --cache            Use the cache with --summary too (it scans fully by default)
      --summary          Scan, print the totals, and exit
  -h, --help             Print this help
  -V, --version          Print the version";

struct Args {
    dir: Option<PathBuf>,
    opts: Options,
    summary: bool,
}

/// A scan waits on the disk and the kernel, so more threads than cores help:
/// 16 threads were about 1.5x faster than 3 on a 3-core macOS runner, cold and
/// warm, 1.28x faster cold on a 4-core Linux runner, and 1.05x faster than 10
/// on a 10-core Mac.
fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    cores.max(16)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        dir: None,
        opts: Options {
            one_fs: false,
            threads: default_threads(),
            cache: true,
            mft: true,
        },
        summary: false,
    };
    let mut cache_in_summary = false;
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-x" | "--one-file-system") => args.opts.one_fs = true,
            Some("--summary") => args.summary = true,
            Some("--no-cache") => args.opts.cache = false,
            Some("--no-mft") => args.opts.mft = false,
            Some("--cache") => cache_in_summary = true,
            Some("-t" | "--threads") => {
                let n = it.next().and_then(|v| v.to_str()?.parse::<usize>().ok());
                args.opts.threads = n
                    .filter(|&n| n > 0)
                    .ok_or("--threads needs a number above 0")?;
            }
            Some("-h" | "--help") => {
                println!("{HELP}");
                std::process::exit(0);
            }
            Some("-V" | "--version") => {
                println!("minimenta {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Some(s) if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option: {s}"));
            }
            _ if args.dir.is_none() => args.dir = Some(PathBuf::from(arg)),
            _ => return Err("only one directory can be scanned".into()),
        }
    }
    // Scripts and benchmarks expect a full scan unless they ask for the cache.
    if args.summary && !cache_in_summary {
        args.opts.cache = false;
    }
    Ok(args)
}

/// Runs minimenta with the command-line arguments of the process.
#[must_use]
pub fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("minimenta: {e}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    let code = if args.summary {
        summary(args)
    } else {
        match ui::run(args.dir, args.opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("minimenta: {e}");
                ExitCode::FAILURE
            }
        }
    };
    cache::flush();
    code
}

fn summary(args: Args) -> ExitCode {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let start = Instant::now();
    let progress = Progress::default();
    match cache::scan(&dir, &args.opts, &progress) {
        Ok(scan) => {
            let tree = scan.dir;
            let t = tree.totals();
            let from = match scan.source {
                Source::Scanned => "full scan".to_string(),
                Source::MasterFileTable => "NTFS master file table".to_string(),
                Source::Cached { listed, .. } => {
                    format!("cache, {listed} directories listed again")
                }
            };
            println!(
                "{}  disk, {}  apparent, {} items, {} errors in {:.3} s ({from})  {}",
                format_size(t.disk).trim_start(),
                format_size(t.apparent).trim_start(),
                t.items,
                progress.errors.load(std::sync::atomic::Ordering::Relaxed),
                start.elapsed().as_secs_f64(),
                dir.display()
            );
            // Freeing millions of nodes takes time and the process ends here anyway.
            std::mem::forget(tree);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("minimenta: {}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}
