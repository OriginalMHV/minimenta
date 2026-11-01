//! Progress of a running delete: the counter, the time left and the footer text.
//!
//! The delete thread adds to a shared counter. The UI thread reads it on every
//! redraw. Text builders are plain functions, so tests need no clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The rate is measured at most this often. Faster samples are noisy.
const SAMPLE_EVERY: Duration = Duration::from_millis(500);
/// No time left before this much time has passed and this share is done.
const ETA_AFTER: Duration = Duration::from_secs(1);
const ETA_MIN_PERCENT: u64 = 2;
/// Weight of the newest rate sample against the older rate.
const NEW_RATE: f64 = 0.4;

/// The UI side of the progress of one job.
pub struct JobProgress {
    pub removed: Arc<AtomicU64>,
    /// The items that a permanent delete should remove. None for the Trash.
    total: Option<u64>,
    started: Option<Instant>,
    last_at: Instant,
    last_removed: u64,
    /// Items per second, smoothed over the samples.
    rate: Option<f64>,
}

impl JobProgress {
    pub fn new(total: Option<u64>, now: Instant) -> Self {
        Self {
            removed: Arc::new(AtomicU64::new(0)),
            total,
            started: None,
            last_at: now,
            last_removed: 0,
            rate: None,
        }
    }

    pub fn total(&self) -> Option<u64> {
        self.total
    }

    pub fn removed(&self) -> u64 {
        self.removed.load(Ordering::Relaxed)
    }

    /// Marks the moment the worker begins this job.
    pub fn start(&mut self, now: Instant) {
        if self.started.is_none() {
            self.started = Some(now);
            self.last_at = now;
        }
    }

    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started.map(|s| now.saturating_duration_since(s))
    }

    /// Updates the smoothed rate, at most twice a second.
    pub fn sample(&mut self, now: Instant) {
        if self.started.is_none() {
            return;
        }
        let dt = now.saturating_duration_since(self.last_at);
        if dt < SAMPLE_EVERY {
            return;
        }
        let removed = self.removed();
        let seen = removed.saturating_sub(self.last_removed) as f64 / dt.as_secs_f64();
        self.rate = Some(
            self.rate
                .map_or(seen, |r| r * (1.0 - NEW_RATE) + seen * NEW_RATE),
        );
        (self.last_at, self.last_removed) = (now, removed);
    }

    /// The time left, or None while the guess would be poor.
    pub fn time_left(&self, now: Instant) -> Option<Duration> {
        let elapsed = self.elapsed(now)?;
        let total = self.total?;
        let removed = self.removed().min(total);
        if elapsed < ETA_AFTER || removed * 100 < total * ETA_MIN_PERCENT {
            return None;
        }
        let rate = self
            .rate
            .unwrap_or_else(|| removed as f64 / elapsed.as_secs_f64());
        if rate <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64((total - removed) as f64 / rate))
    }
}

/// The jobs behind the running one.
pub struct Queue {
    pub jobs: usize,
    /// The items of all queued jobs, when each one is a permanent delete.
    pub items: Option<u64>,
}

impl Queue {
    fn text(&self, with_items: bool) -> String {
        match self.items {
            Some(n) if with_items => format!("{} more queued ({})", self.jobs, items(n)),
            _ => format!("{} more queued", self.jobs),
        }
    }
}

pub fn items(n: u64) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{} items", thousands(n))
    }
}

/// 21340 becomes "21,340".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "4 s" or "2 min 5 s".
pub fn elapsed_text(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min {} s", secs / 60, secs % 60)
    }
}

/// A calm estimate: "less than 5 s left", "about 40 s left", "about 2 min left".
pub fn left_text(d: Duration) -> String {
    let secs = d.as_secs_f64().round() as u64;
    let near = (secs + 2) / 5 * 5;
    if secs < 5 {
        "less than 5 s left".into()
    } else if near < 60 {
        format!("about {near} s left")
    } else if secs < 90 * 60 {
        format!("about {} min left", (secs + 30) / 60)
    } else {
        format!("about {} h left", (secs + 1800) / 3600)
    }
}

/// Never 100% while the job runs, because the numbers on disk can differ
/// from the tree (hard links, changes).
pub fn percent(removed: u64, total: u64) -> u64 {
    (removed.min(total) * 100)
        .checked_div(total)
        .map_or(0, |p| p.min(99))
}

/// The footer text of a running permanent delete. Parts drop in this order
/// until the text fits `budget`: items of the queue, the counts, the queue,
/// then the label gets short. Percent and time left stay.
pub fn deleting_text(
    label: &str,
    removed: u64,
    total: u64,
    left: Option<Duration>,
    queue: Option<&Queue>,
    budget: usize,
) -> String {
    let removed = removed.min(total);
    let mut head = format!("{}% done", percent(removed, total));
    if let Some(left) = left {
        head = format!("{head}, {}", left_text(left));
    }
    let detail = format!(" ({} of {})", thousands(removed), items(total));
    let queued =
        |with_items: bool| queue.map_or_else(String::new, |q| format!(". {}", q.text(with_items)));
    let build = |label: &str, detail: &str, queue: String| {
        format!("Deleting {label}: {head}{detail}{queue}")
    };
    let variants = |label: &str| {
        [
            build(label, &detail, queued(true)),
            build(label, &detail, queued(false)),
            build(label, "", queued(false)),
            build(label, "", String::new()),
        ]
    };
    if let Some(text) = variants(label)
        .into_iter()
        .find(|t| t.chars().count() <= budget)
    {
        return text;
    }
    let bare = build("", "", String::new());
    let room = budget.saturating_sub(bare.chars().count());
    build(&shorten(label, room.max(8)), "", String::new())
}

pub fn trashing_text(count: &str, elapsed: Option<Duration>, queue: Option<&Queue>) -> String {
    let mut text = format!("Moving {count} to the Trash …");
    let secs = elapsed.filter(|d| *d >= Duration::from_secs(1));
    if let Some(d) = secs {
        text = format!("{text} {}", elapsed_text(d));
    }
    if let Some(q) = queue {
        let sep = if secs.is_some() { "." } else { "" };
        text = format!("{text}{sep} {}", q.text(false));
    }
    text
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn running(total: u64, base: Instant) -> JobProgress {
        let mut p = JobProgress::new(Some(total), base);
        p.start(base);
        p
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(21340), "21,340");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(items(1), "1 item");
        assert_eq!(items(100_000), "100,000 items");
    }

    #[test]
    fn no_time_left_before_one_second_or_two_percent() {
        let base = Instant::now();
        let p = running(1000, base);
        p.removed.store(500, Ordering::Relaxed);
        assert_eq!(p.time_left(at(base, 999)), None, "under 1 s");
        assert!(p.time_left(at(base, 1000)).is_some());
        p.removed.store(19, Ordering::Relaxed);
        assert_eq!(p.time_left(at(base, 5000)), None, "under 2%");
        p.removed.store(20, Ordering::Relaxed);
        assert!(p.time_left(at(base, 5000)).is_some());
    }

    #[test]
    fn no_time_left_for_the_trash_or_a_job_that_has_not_started() {
        let base = Instant::now();
        let trash = JobProgress::new(None, base);
        assert_eq!(trash.time_left(at(base, 9000)), None);
        let queued = JobProgress::new(Some(100), base);
        queued.removed.store(50, Ordering::Relaxed);
        assert_eq!(queued.time_left(at(base, 9000)), None);
    }

    #[test]
    fn time_left_follows_the_rate_so_far() {
        let base = Instant::now();
        let p = running(100_000, base);
        p.removed.store(20_000, Ordering::Relaxed);
        // 20,000 items in 10 s, 80,000 to go.
        assert_eq!(p.time_left(at(base, 10_000)), Some(secs(40)));
    }

    #[test]
    fn the_smoothed_rate_does_not_jump_on_one_fast_sample() {
        let base = Instant::now();
        let mut p = running(1_000_000, base);
        p.removed.store(1000, Ordering::Relaxed);
        p.sample(at(base, 1000));
        assert_eq!(p.rate, Some(1000.0));
        p.removed.store(11_000, Ordering::Relaxed);
        p.sample(at(base, 1100));
        assert_eq!(
            p.rate,
            Some(1000.0),
            "samples under 500 ms apart are skipped"
        );
        p.sample(at(base, 1500));
        // 10,000 items in 0.5 s is 20,000 per s, but the rate moves by 40% only.
        let rate = p.rate.unwrap();
        assert!((rate - 8600.0).abs() < 1.0, "{rate}");
    }

    #[test]
    fn the_time_left_is_rounded_and_calm() {
        assert_eq!(left_text(secs(0)), "less than 5 s left");
        assert_eq!(left_text(secs(4)), "less than 5 s left");
        assert_eq!(left_text(secs(5)), "about 5 s left");
        assert_eq!(left_text(secs(38)), "about 40 s left");
        assert_eq!(left_text(secs(57)), "about 55 s left");
        assert_eq!(left_text(secs(58)), "about 1 min left");
        assert_eq!(left_text(secs(100)), "about 2 min left");
        assert_eq!(left_text(secs(25 * 60)), "about 25 min left");
        assert_eq!(left_text(secs(3 * 3600)), "about 3 h left");
    }

    #[test]
    fn the_percent_stays_under_100_until_the_job_ends() {
        assert_eq!(percent(0, 100), 0);
        assert_eq!(percent(21_340, 100_000), 21);
        assert_eq!(percent(999, 1000), 99);
        assert_eq!(percent(1000, 1000), 99);
        assert_eq!(percent(1500, 1000), 99);
        assert_eq!(percent(5, 0), 0);
    }

    #[test]
    fn elapsed_text_has_seconds_and_minutes() {
        assert_eq!(elapsed_text(secs(4)), "4 s");
        assert_eq!(elapsed_text(secs(125)), "2 min 5 s");
    }

    #[test]
    fn the_trash_shows_the_elapsed_time_after_one_second() {
        assert_eq!(
            trashing_text("1 item", Some(Duration::from_millis(900)), None),
            "Moving 1 item to the Trash …"
        );
        assert_eq!(
            trashing_text("1 item", Some(secs(4)), None),
            "Moving 1 item to the Trash … 4 s"
        );
        let queue = Queue {
            jobs: 2,
            items: Some(9),
        };
        assert_eq!(
            trashing_text("1 item", Some(secs(4)), Some(&queue)),
            "Moving 1 item to the Trash … 4 s. 2 more queued"
        );
        assert_eq!(
            trashing_text("1 item", None, Some(&queue)),
            "Moving 1 item to the Trash … 2 more queued"
        );
    }

    #[test]
    fn the_progress_text_has_counts_percent_time_and_queue() {
        let queue = Queue {
            jobs: 2,
            items: Some(45_000),
        };
        assert_eq!(
            deleting_text("big", 21_340, 100_000, Some(secs(38)), Some(&queue), 200),
            "Deleting big: 21% done, about 40 s left (21,340 of 100,000 items). \
             2 more queued (45,000 items)"
        );
        assert_eq!(
            deleting_text("3 items", 0, 50, None, None, 200),
            "Deleting 3 items: 0% done (0 of 50 items)"
        );
        let mixed = Queue {
            jobs: 1,
            items: None,
        };
        assert_eq!(
            deleting_text("big", 5, 10, None, Some(&mixed), 200),
            "Deleting big: 50% done (5 of 10 items). 1 more queued"
        );
    }

    #[test]
    fn the_footer_text_keeps_percent_and_time_left_on_80_columns() {
        let queue = Queue {
            jobs: 2,
            items: Some(45_000),
        };
        let long = "a-very-long-folder-name-that-takes-room-in-the-footer";
        // The browser leaves room for a short message and the separator.
        for budget in [79, 62] {
            let text = deleting_text(long, 21_340, 100_000, Some(secs(38)), Some(&queue), budget);
            assert!(text.chars().count() <= budget, "{text}");
            assert!(text.contains("21% done, about 40 s left"), "{text}");
        }
        let text = deleting_text("big", 21_340, 100_000, Some(secs(38)), Some(&queue), 62);
        assert_eq!(
            text,
            "Deleting big: 21% done, about 40 s left. 2 more queued"
        );
    }
}
