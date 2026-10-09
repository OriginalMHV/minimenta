//! Decides when the Windows scanner starts the NTFS master file table (MFT)
//! reader next to the directory listing. The decision is pure, so its tests
//! need no clock and no disk, and they run on every platform.
//!
//! The reader always reads the table of the whole volume, whatever the folder
//! is. For a 1.3 GB table this takes about 3 s to 4 s. It keeps 16 reads of
//! 4 MiB in flight, so the small reads of the listing wait behind them. This
//! gives two cases:
//!
//! - A whole volume. The listing must visit every item, and the reader wins
//!   by a wide margin. The reader starts at once.
//! - A folder. A folder that lists within a few seconds is faster without the
//!   reader, because both compete for the disk. The reader starts only when
//!   the listing has run for `FOLDER_DELAY` and is still slow.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use super::Progress;

/// How long the listing of a folder runs alone before the reader may start.
pub(super) const FOLDER_DELAY: Duration = Duration::from_secs(2);

/// The listing rate is the item count of one window divided by its length.
const WINDOW: Duration = Duration::from_millis(250);

/// A listing below this rate, in items per second, is slow. On the Windows
/// runner, a warm listing of `C:\Program Files` ran at about 460,000 items/s
/// and a cold one at about 18,000 items/s.
const SLOW_RATE: f64 = 150_000.0;

/// Whether the MFT reader starts now.
///
/// - `volume_root`: the scan covers a whole volume.
/// - `elapsed`: the time since the listing started.
/// - `delay`: the time a folder listing runs alone.
/// - `recent_rate`: the listing rate over the last full window, in items per
///   second, or `None` when no window has passed yet.
pub(super) fn should_start(
    volume_root: bool,
    elapsed: Duration,
    delay: Duration,
    recent_rate: Option<f64>,
) -> bool {
    volume_root || (elapsed >= delay && recent_rate.is_some_and(|rate| rate < SLOW_RATE))
}

/// Measures the listing rate from samples of the listed item count.
struct RateCheck {
    /// Time and item count at the start of the current window.
    from: (Instant, u64),
}

impl RateCheck {
    fn new(start: Instant) -> Self {
        RateCheck { from: (start, 0) }
    }

    /// The rate of the window that ends at `now` with `items` listed, in
    /// items per second. It starts the next window. Before the current window
    /// has passed, it returns `None` and changes nothing.
    fn sample(&mut self, now: Instant, items: u64) -> Option<f64> {
        let elapsed = now.saturating_duration_since(self.from.0);
        if elapsed < WINDOW {
            return None;
        }
        let rate = items.saturating_sub(self.from.1) as f64 / elapsed.as_secs_f64();
        self.from = (now, items);
        Some(rate)
    }

    /// The time left in the current window at `now`.
    fn remaining(&self, now: Instant) -> Duration {
        WINDOW.saturating_sub(now.saturating_duration_since(self.from.0))
    }
}

/// Waits until the MFT reader should start, and returns false when `done` is
/// set first. The thread that sets `done` must unpark the waiting thread.
/// Small and warm folders finish or list fast, so they pay nothing for the
/// reader.
pub(super) fn wait(
    progress: &Progress,
    done: &AtomicBool,
    volume_root: bool,
    delay: Duration,
) -> bool {
    let start = Instant::now();
    let mut check = RateCheck::new(start);
    loop {
        if done.load(Relaxed) {
            return false;
        }
        let now = Instant::now();
        let rate = check.sample(now, progress.items.load(Relaxed));
        if should_start(
            volume_root,
            now.saturating_duration_since(start),
            delay,
            rate,
        ) {
            return true;
        }
        std::thread::park_timeout(check.remaining(Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELAY: Duration = Duration::from_secs(2);

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn a_whole_volume_starts_the_reader_at_once() {
        assert!(should_start(true, ms(0), DELAY, None));
        assert!(should_start(true, ms(10), DELAY, Some(900_000.0)));
    }

    #[test]
    fn a_folder_never_starts_the_reader_before_the_delay() {
        // 100 items/s is far below the slow rate.
        assert!(!should_start(false, ms(0), DELAY, Some(100.0)));
        assert!(!should_start(false, ms(1_999), DELAY, Some(100.0)));
        assert!(!should_start(false, ms(1_999), DELAY, Some(0.0)));
    }

    #[test]
    fn a_folder_starts_the_reader_after_the_delay_when_the_listing_is_slow() {
        assert!(should_start(false, DELAY, DELAY, Some(18_000.0)));
        assert!(should_start(false, ms(9_000), DELAY, Some(0.0)));
    }

    #[test]
    fn a_folder_with_a_fast_listing_never_starts_the_reader() {
        assert!(!should_start(false, ms(2_000), DELAY, Some(460_000.0)));
        assert!(!should_start(false, ms(2_000), DELAY, Some(SLOW_RATE)));
        assert!(should_start(false, ms(2_250), DELAY, Some(20_000.0)));
    }

    #[test]
    fn a_folder_needs_a_measured_rate_to_start_the_reader() {
        assert!(!should_start(false, ms(2_000), DELAY, None));
    }

    #[test]
    fn the_rate_comes_from_one_full_window() {
        let start = Instant::now();
        let at = |t| start + ms(t);
        let mut check = RateCheck::new(start);
        assert_eq!(
            check.sample(at(100), 5_000),
            None,
            "the window has not passed"
        );
        assert_eq!(check.remaining(at(100)), ms(150));
        // 25,000 items in 250 ms is 100,000 items/s.
        assert_eq!(check.sample(at(250), 25_000), Some(100_000.0));
        // The next window counts from the last sample: 2,500 items in 250 ms.
        assert_eq!(check.sample(at(500), 27_500), Some(10_000.0));
        assert_eq!(check.sample(at(600), 99_000), None);
    }

    #[test]
    fn a_stalled_listing_has_a_rate_of_zero() {
        let start = Instant::now();
        let mut check = RateCheck::new(start);
        assert_eq!(check.sample(start + ms(250), 0), Some(0.0));
        assert_eq!(check.remaining(start + ms(250)), WINDOW);
    }

    #[test]
    fn a_clock_that_runs_back_does_not_end_the_window() {
        let earlier = Instant::now();
        let mut check = RateCheck::new(earlier + ms(1_000));
        assert_eq!(check.sample(earlier, 10), None);
        assert_eq!(check.remaining(earlier), WINDOW);
    }

    #[test]
    fn a_whole_volume_stops_waiting_at_once() {
        assert!(wait(
            &Progress::default(),
            &AtomicBool::new(false),
            true,
            FOLDER_DELAY
        ));
    }

    #[test]
    fn a_stalled_folder_stops_waiting_after_the_delay() {
        assert!(wait(
            &Progress::default(),
            &AtomicBool::new(false),
            false,
            ms(300)
        ));
    }

    #[test]
    fn a_finished_listing_never_starts_the_reader() {
        for volume_root in [false, true] {
            assert!(!wait(
                &Progress::default(),
                &AtomicBool::new(true),
                volume_root,
                FOLDER_DELAY
            ));
        }
    }
}
