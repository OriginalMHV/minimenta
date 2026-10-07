mod scan;
mod tree;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use scan::{Options, Progress};
use tree::format_size;

const HELP: &str = "\
minimenta: an interactive disk usage analyzer

Usage: minimenta [OPTIONS] [DIR]

Without DIR, minimenta asks which directory to scan.

Options:
  -x, --one-file-system  Do not cross file system boundaries
  -t, --threads N        Number of scan threads (default: CPU count)
      --summary          Scan, print the totals, and exit
  -h, --help             Print this help
  -V, --version          Print the version";

struct Args {
    dir: Option<PathBuf>,
    opts: Options,
    summary: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        dir: None,
        opts: Options {
            one_fs: false,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        },
        summary: false,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-x" | "--one-file-system") => args.opts.one_fs = true,
            Some("--summary") => args.summary = true,
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
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("minimenta: {e}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    if args.summary {
        return summary(args);
    }
    match ui::run(args.dir, args.opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("minimenta: {e}");
            ExitCode::FAILURE
        }
    }
}

fn summary(args: Args) -> ExitCode {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let start = Instant::now();
    let progress = Progress::default();
    match scan::scan(&dir, &args.opts, &progress) {
        Ok(tree) => {
            let t = tree.totals();
            println!(
                "{}  disk, {}  apparent, {} items, {} errors in {:.3} s  {}",
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
