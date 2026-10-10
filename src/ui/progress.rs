use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{display_path, view};
use crate::cache::{self, Scan};
use crate::scan::{Options, Progress};
use crate::tree::format_size;

/// The screen shows no estimate in the first seconds of counting, when the
/// rate still swings.
const MIN_COUNTING: Duration = Duration::from_secs(2);

pub enum Outcome {
    Done(Box<Scan>),
    Cancelled,
    Quit,
    Failed(io::Error),
}

/// Scans `path` on a background thread. The progress screen appears only
/// when the scan takes longer than a moment, so fast scans do not flicker.
/// `expected` is the item count of an earlier scan of `path`, or 0 when there
/// is none.
pub fn scan(
    terminal: &mut DefaultTerminal,
    path: &Path,
    opts: Options,
    expected: u64,
) -> io::Result<Outcome> {
    let progress = Arc::new(Progress::default());
    progress.expected_items.store(expected, Relaxed);
    let (tx, rx) = mpsc::channel();
    {
        let progress = Arc::clone(&progress);
        let path = path.to_path_buf();
        thread::spawn(move || {
            let _ = tx.send(cache::scan(&path, &opts, &progress));
        });
    }
    let start = Instant::now();
    // The counters start after the cache check, so the rate counts from there.
    let mut counting_since = start;
    let mut quit = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(scan)) => return Ok(Outcome::Done(Box::new(scan))),
            Ok(Err(_)) if progress.cancel.load(Relaxed) => {
                return Ok(if quit {
                    Outcome::Quit
                } else {
                    Outcome::Cancelled
                });
            }
            Ok(Err(e)) => return Ok(Outcome::Failed(e)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("scan thread stopped"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let checking = progress.checking_cache.load(Relaxed);
        if checking {
            counting_since = Instant::now();
        }
        let lines = lines(
            path,
            &progress,
            checking,
            start.elapsed(),
            counting_since.elapsed(),
        );
        terminal.draw(|frame| {
            let [header, _, body, _, footer] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(lines.len() as u16),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(frame.area());
            frame.render_widget(view::header_bar(), header);
            frame.render_widget(Paragraph::new(lines), body);
            frame.render_widget(view::bar(" Esc cancel   q quit"), footer);
        })?;
        while event::poll(Duration::ZERO)? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_c || key.code == KeyCode::Char('q') {
                quit = true;
            }
            if quit || key.code == KeyCode::Esc {
                progress.cancel.store(true, Relaxed);
            }
        }
    }
}

/// The text of the progress screen. `counting` is the time since the
/// counters started, after the cache check.
fn lines(
    path: &Path,
    progress: &Progress,
    checking: bool,
    elapsed: Duration,
    counting: Duration,
) -> Vec<Line<'static>> {
    let items = progress.items.load(Relaxed);
    let expected = progress.expected_items.load(Relaxed);
    let mut lines = vec![Line::from(format!("  Scanning {}", display_path(path)))];
    if checking {
        lines.push(Line::from(
            "  Checking the cache for changes since the last scan",
        ));
    } else if progress.cache_unusable.load(Relaxed) {
        lines.push(Line::from(
            "  minimenta could not use the cache and scans all folders again",
        ));
    }
    let (last, note) = if checking {
        let note = match progress.check_limit_ms.load(Relaxed) {
            0 => String::new(),
            ms => format!(
                "   (the check waits until {} s at the latest)",
                ms.div_ceil(1000)
            ),
        };
        (String::new(), note)
    } else {
        let last = match expected {
            0 => String::new(),
            n => format!("   (last scan: {n})"),
        };
        let note = time_left(items, expected, counting)
            .map(|left| format!("   ({})", format_left(left)))
            .unwrap_or_default();
        (last, note)
    };
    lines.extend([
        Line::from(""),
        Line::from(format!("  Items:  {items}{last}")),
        Line::from(format!(
            "  Size:   {}",
            format_size(progress.disk.load(Relaxed)).trim_start()
        )),
        Line::from(format!(
            "  Time:   {:.1} s{note}{}",
            elapsed.as_secs_f64(),
            match progress.errors.load(Relaxed) {
                0 => String::new(),
                n => format!("   ({n} directories could not be read)"),
            }
        )),
    ]);
    lines
}

/// Estimates the time left from the rate since the counting began. Returns
/// `None` without an earlier count, in the first seconds of counting, and
/// once the scan has passed the earlier count.
fn time_left(items: u64, expected: u64, counting: Duration) -> Option<Duration> {
    if items == 0 || items >= expected || counting < MIN_COUNTING {
        return None;
    }
    Duration::try_from_secs_f64(counting.as_secs_f64() * (expected - items) as f64 / items as f64)
        .ok()
}

/// Whole seconds below 90 s, then whole minutes. Both round up.
fn format_left(left: Duration) -> String {
    let secs = left.as_secs_f64().ceil() as u64;
    if secs < 90 {
        format!("about {secs} s left")
    } else {
        format!("about {} min left", secs.div_ceil(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_the_time_left_from_the_rate_so_far() {
        // 1,000 of 4,000 items in 10 s is 100 items/s, so 3,000 items take 30 s.
        assert_eq!(
            time_left(1000, 4000, Duration::from_secs(10)),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn gives_no_estimate_without_a_usable_count() {
        let ten = Duration::from_secs(10);
        assert_eq!(time_left(1000, 0, ten), None, "no earlier scan");
        assert_eq!(
            time_left(4000, 4000, ten),
            None,
            "reached the earlier count"
        );
        assert_eq!(time_left(5000, 4000, ten), None, "passed the earlier count");
        assert_eq!(time_left(0, 4000, ten), None, "nothing counted yet");
        assert_eq!(
            time_left(10, 4000, Duration::from_secs(1)),
            None,
            "too early"
        );
    }

    fn text(path: &str, progress: &Progress, checking: bool, counting: u64) -> Vec<String> {
        let elapsed = Duration::from_secs(25);
        lines(
            Path::new(path),
            progress,
            checking,
            elapsed,
            Duration::from_secs(counting),
        )
        .iter()
        .map(ToString::to_string)
        .collect()
    }

    #[test]
    fn says_when_it_checks_the_cache_and_when_the_check_gave_up() {
        let progress = Progress::default();
        progress.checking_cache.store(true, Relaxed);
        progress.check_limit_ms.store(17_840, Relaxed);
        progress.expected_items.store(4000, Relaxed);
        let shown = text("/x", &progress, true, 0);
        assert_eq!(
            shown[1],
            "  Checking the cache for changes since the last scan"
        );
        assert_eq!(shown[3], "  Items:  0");
        assert_eq!(
            shown[5],
            "  Time:   25.0 s   (the check waits until 18 s at the latest)"
        );

        progress.checking_cache.store(false, Relaxed);
        progress.check_limit_ms.store(0, Relaxed);
        progress.cache_unusable.store(true, Relaxed);
        progress.items.store(1000, Relaxed);
        let shown = text("/x", &progress, false, 10);
        assert_eq!(
            shown[1],
            "  minimenta could not use the cache and scans all folders again"
        );
        assert_eq!(shown[3], "  Items:  1000   (last scan: 4000)");
        assert_eq!(shown[5], "  Time:   25.0 s   (about 30 s left)");
    }

    #[test]
    fn a_scan_without_an_earlier_count_looks_as_before() {
        let progress = Progress::default();
        progress.items.store(1000, Relaxed);
        let shown = text("/x", &progress, false, 25);
        assert_eq!(shown.len(), 5);
        assert_eq!(shown[0], "  Scanning /x");
        assert_eq!(shown[1], "");
        assert_eq!(shown[2], "  Items:  1000");
        assert_eq!(shown[4], "  Time:   25.0 s");
    }

    #[test]
    fn shows_seconds_then_minutes() {
        assert_eq!(
            format_left(Duration::from_millis(41_200)),
            "about 42 s left"
        );
        assert_eq!(format_left(Duration::from_secs(89)), "about 89 s left");
        assert_eq!(format_left(Duration::from_secs(90)), "about 2 min left");
        assert_eq!(format_left(Duration::from_secs(600)), "about 10 min left");
    }
}
