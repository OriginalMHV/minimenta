//! Drawing of the search screen.

use std::fmt::Write;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::browser::{Browser, Focus, Search};
use super::display_path;
use super::view::{
    ACCENT, MIN_GAP, STATUS_SEP, bar, footer_right, notice, notice_line, popup, right_align,
    scroll, tail,
};
use crate::search::{self, MAX_HITS, PRESETS};
use crate::tree::format_size;

const BAR_WIDTH: usize = 10;

pub fn draw(frame: &mut Frame, b: &mut Browser, path: Rect, list: Rect, footer: Rect) {
    b.list_height = list.height as usize;
    let here = display_path(&b.current_path());
    let width = path.width as usize;
    let Some(s) = b.search.as_mut() else {
        return;
    };
    s.offset = scroll(s.offset, s.cursor, list.height as usize, s.hits.len());
    frame.render_widget(Line::from(path_line(s, &here, width)).dark_gray(), path);
    draw_list(frame, s, list);
    frame.render_widget(footer_line(b, width), footer);
}

fn matches_word(n: u64) -> &'static str {
    if n == 1 { "match" } else { "matches" }
}

/// "--- Search in ~/Documents: @node (37 matches, 4.2 GiB) ---"
fn path_line(s: &Search, here: &str, width: usize) -> String {
    let counts = if s.error.is_some() || s.query.trim().is_empty() || s.hits.is_empty() {
        String::new()
    } else if (s.hits.len() as u64) < s.matches {
        format!(
            " (largest {} of {} matches, {} in all)",
            s.hits.len(),
            s.matches,
            format_size(s.total).trim_start()
        )
    } else {
        format!(
            " ({} {}, {})",
            s.matches,
            matches_word(s.matches),
            format_size(s.total).trim_start()
        )
    };
    let rest = format!(": {}{counts} ", s.query);
    let room = width
        .saturating_sub("--- Search in ".len() + rest.chars().count())
        .max(8);
    let title = format!("--- Search in {}{rest}", tail(here, room));
    let fill = "-".repeat(width.saturating_sub(title.chars().count()));
    format!("{title}{fill}")
}

fn draw_list(frame: &mut Frame, s: &Search, area: Rect) {
    if let Some(presets) = search::presets_for(&s.query) {
        let mut lines =
            vec![Line::from("  Ready-made searches. Tab completes the name.").dark_gray()];
        for p in presets {
            lines.push(Line::from(vec![
                Span::from(format!("  @{:<9}", p.name)).bold(),
                Span::from(p.help),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }
    if s.hits.is_empty() {
        let text = if let Some(error) = &s.error {
            frame.render_widget(Line::from(format!("  {error}")).red(), area);
            return;
        } else if s.query.trim().is_empty() {
            "  Type a name, a pattern like *.log, size>1G or type:dir. Type @ for ready-made searches."
        } else if s.searching() {
            "  Searching …"
        } else {
            "  (no matches)"
        };
        frame.render_widget(Line::from(text).dark_gray(), area);
        return;
    }
    let total = s.total.max(1);
    let largest = s.hits.iter().map(|h| h.size).max().unwrap_or(0).max(1);
    let width = area.width as usize;
    let lines: Vec<Line> = s
        .hits
        .iter()
        .enumerate()
        .skip(s.offset)
        .take(area.height as usize)
        .map(|(i, h)| {
            let cursor = i == s.cursor && s.focus == Focus::Results;
            row(h, cursor, total, largest, width)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn row(h: &search::Hit, is_cursor: bool, total: u64, largest: u64, width: usize) -> Line<'static> {
    let filled = ((u128::from(h.size) * BAR_WIDTH as u128).div_ceil(u128::from(largest)) as usize)
        .min(BAR_WIDTH);
    let filled = if h.size == 0 { 0 } else { filled };
    let mark = if h.selected { '*' } else { ' ' };
    let prefix = format!(
        "{mark}  {} {:>5.1}% [{}{}] ",
        format_size(h.size),
        h.size as f64 * 100.0 / total as f64,
        "#".repeat(filled),
        " ".repeat(BAR_WIDTH - filled),
    );
    let slash = if h.is_dir { "/" } else { "" };
    let path = format!("{}{slash}", String::from_utf8_lossy(&h.rel));
    let path = tail(&path, width.saturating_sub(prefix.chars().count()));
    let pad = " ".repeat(width.saturating_sub(prefix.chars().count() + path.chars().count()));
    let mut style = Style::default();
    if h.selected {
        style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
    }
    if is_cursor {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let path_style = if h.is_dir { style.bold() } else { style };
    Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(path, path_style),
        Span::styled(pad, style),
    ])
}

fn footer_line(b: &Browser, width: usize) -> Line<'static> {
    let Some(s) = &b.search else {
        return bar("");
    };
    let notice = notice(b, width);
    if s.focus == Focus::Query {
        let line = format!("Search: {}_", s.query);
        let note = if let Some(e) = &s.error {
            Some(e.clone())
        } else if search::presets_for(&s.query).is_some() {
            Some("Tab completes the name".to_string())
        } else if s.searching() {
            Some("Searching …".to_string())
        } else {
            None
        };
        let extra = match (note, notice) {
            (Some(n), Some(m)) => Some(format!("{n}{STATUS_SEP}{m}")),
            (n, m) => n.or(m),
        };
        return bar(&match extra {
            Some(extra) => format!(" {line}{STATUS_SEP}{extra}"),
            None => format!(" {line}"),
        });
    }
    if let Some(line) = notice_line(b, width) {
        return line;
    }
    // A query error stays on the screen after Enter.
    if let Some(error) = &s.error {
        return bar(&format!(" {error}{STATUS_SEP}/ edit the query"));
    }
    let mut text = format!(
        " {} {}: {}",
        s.matches,
        matches_word(s.matches),
        format_size(s.total).trim_start()
    );
    let (count, size) = s
        .selected()
        .fold((0, 0), |(n, sum), i| (n + 1, sum + s.hits[i].size));
    if count > 0 {
        let _ = write!(
            text,
            "   |  {count} selected: {}",
            format_size(size).trim_start()
        );
    }
    if s.searching() {
        text.push_str("   |  Searching …");
    } else {
        text.push_str("   |  / edit the query, ? help");
    }
    let position = (!s.hits.is_empty()).then(|| format!("{}/{}", s.cursor + 1, s.hits.len()));
    // The free space comes before the position, as in the browser. When the
    // line cannot hold it, the free space drops.
    let free = b.free.text();
    let fits = |right: &String| text.chars().count() + right.chars().count() + MIN_GAP <= width;
    let right = footer_right(free.as_deref(), position.as_deref())
        .filter(fits)
        .or_else(|| footer_right(None, position.as_deref()));
    match right {
        Some(right) => bar(&right_align(text, &right, width)),
        None => bar(&text),
    }
}

/// The total size and the lines of the question for the results at `targets`.
pub fn confirm_rows(s: &Search, targets: &[usize]) -> (u64, Vec<String>) {
    let hits: Vec<_> = targets.iter().filter_map(|&i| s.hits.get(i)).collect();
    let size = hits.iter().map(|h| h.size).sum();
    let names = hits
        .iter()
        .map(|h| {
            let slash = if h.is_dir { "/" } else { "" };
            format!("{}{slash}", String::from_utf8_lossy(&h.rel))
        })
        .collect();
    (size, names)
}

const KEYS: &[(&str, &str)] = &[
    ("Enter, Down", "From the query: go to the results"),
    ("Tab", "In the query: complete a @name"),
    ("Ctrl+U", "In the query: clear the line"),
    ("/", "In the results: edit the query"),
    ("Up/Down/PgUp/PgDn", "Move the cursor"),
    ("Home / End", "First / last result"),
    ("Space", "Select or deselect, then move down"),
    ("Ctrl+A", "Select all results (also from the query)"),
    ("Esc", "Clear the selection, then leave the search"),
    (
        "Enter, Right, l",
        "Leave the search and show the folder of the item",
    ),
    ("d", "Move to the Trash (selection or cursor)"),
    ("D", "Delete permanently"),
    ("a", "Show apparent size or disk usage"),
    ("r, u", "Not in a search. Press Esc first."),
];

const WORDS: &[(&str, &str)] = &[
    ("text", "The name has the text, in any case"),
    (
        "*.log, a?c",
        "Name fits a pattern (* any text, ? one letter)",
    ),
    ("cache/", "Path below this folder has the text or pattern"),
    (
        "size>1G",
        "Size above. Also <, >=, <=. Units K M G T (1024)",
    ),
    ("type:dir", "Folders only. type:file: files and links"),
    ("-word", "Leave out paths with the word. -*.git works too"),
    ("@name", "A ready-made search, see below"),
];

pub fn draw_help(frame: &mut Frame, b: &Browser) {
    let page = b.search.as_ref().map_or(0, |s| s.help_page);
    let mut lines: Vec<Line> = Vec::new();
    let table = |lines: &mut Vec<Line>, rows: &[(&str, &str)]| {
        for (keys, what) in rows {
            lines.push(Line::from(vec![
                Span::from(format!(" {keys:<19}")).bold(),
                Span::from((*what).to_string()),
            ]));
        }
    };
    let title = if page == 0 {
        table(&mut lines, KEYS);
        lines.push(Line::from(""));
        lines.push(Line::from(" The search reads the scan in memory, not the disk.").dark_gray());
        lines.push(Line::from(format!(" It shows the largest {MAX_HITS} matches.")).dark_gray());
        "Search keys (1/2)"
    } else {
        lines.push(Line::from(" Every word must match:").dark_gray());
        table(&mut lines, WORDS);
        lines.push(Line::from(" Ready-made searches (type @ to list them):").dark_gray());
        for p in PRESETS {
            lines.push(Line::from(vec![
                Span::from(format!(" @{:<12}", p.name)).bold(),
                Span::from(p.help),
            ]));
        }
        lines.push(
            Line::from(" A folder that matches hides its content, so no size counts twice.")
                .dark_gray(),
        );
        "Search words (2/2)"
    };
    if page == 0 {
        lines.push(Line::from(""));
    }
    let close = if page == 0 {
        " Press any key for the next page"
    } else {
        " Press any key to close"
    };
    lines.push(Line::from(close).dark_gray());
    let inner = popup(frame, 72, lines.len() as u16 + 2, title, Color::Reset);
    frame.render_widget(Paragraph::new(lines), inner);
}
