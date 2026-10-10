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

pub enum Outcome {
    Done(Box<Scan>),
    Cancelled,
    Quit,
    Failed(io::Error),
}

/// Scans `path` on a background thread. The progress screen appears only
/// when the scan takes longer than a moment, so fast scans do not flicker.
pub fn scan(terminal: &mut DefaultTerminal, path: &Path, opts: Options) -> io::Result<Outcome> {
    let progress = Arc::new(Progress::default());
    let (tx, rx) = mpsc::channel();
    {
        let progress = Arc::clone(&progress);
        let path = path.to_path_buf();
        thread::spawn(move || {
            let _ = tx.send(cache::scan(&path, &opts, &progress));
        });
    }
    let start = Instant::now();
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
        let limit = match progress.check_limit_ms.load(Relaxed) {
            ms if checking && ms > 0 => {
                format!(
                    "   (the check waits until {} s at the latest)",
                    ms.div_ceil(1000)
                )
            }
            _ => String::new(),
        };
        lines.extend([
            Line::from(""),
            Line::from(format!("  Items:  {}", progress.items.load(Relaxed))),
            Line::from(format!(
                "  Size:   {}",
                format_size(progress.disk.load(Relaxed)).trim_start()
            )),
            Line::from(format!(
                "  Time:   {:.1} s{limit}{}",
                start.elapsed().as_secs_f64(),
                match progress.errors.load(Relaxed) {
                    0 => String::new(),
                    n => format!("   ({n} directories could not be read)"),
                }
            )),
        ]);
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
