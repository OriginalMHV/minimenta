use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::browser::{Browser, Mode};
use super::display_path;
use crate::tree::{Dir, Entry, Kind, SortKey, flag, format_size};

const BAR_WIDTH: usize = 10;

/// The mint accent of the logo (#44B78F), as the closest 256-color index so it
/// also works in terminals without true color.
pub const ACCENT: Color = Color::Indexed(72);

pub fn header_bar() -> Line<'static> {
    bar(&format!(
        " minimenta {} ~ Use the arrow keys to navigate, press ? for help",
        env!("CARGO_PKG_VERSION")
    ))
}

pub fn bar(text: &str) -> Line<'static> {
    Line::from(text.to_string()).style(Style::default().add_modifier(Modifier::REVERSED))
}

pub fn draw(frame: &mut Frame, b: &mut Browser) {
    let [header, path, list, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(header_bar(), header);
    let title = format!(
        "--- {} ",
        tail(
            &display_path(&b.current_path()),
            (path.width as usize).saturating_sub(6)
        )
    );
    let fill = "-".repeat((path.width as usize).saturating_sub(title.chars().count()));
    frame.render_widget(Line::from(format!("{title}{fill}")).dark_gray(), path);

    b.list_height = list.height as usize;
    let len = b.dir().entries.len();
    if b.cursor < b.offset {
        b.offset = b.cursor;
    } else if b.cursor >= b.offset + b.list_height {
        b.offset = b.cursor + 1 - b.list_height;
    }
    b.offset = b.offset.min(len.saturating_sub(b.list_height));
    draw_list(frame, b, list);
    frame.render_widget(footer_line(b), footer);

    match &b.mode {
        Mode::Browse => {}
        Mode::Help => draw_help(frame),
        Mode::Confirm { permanent, targets } => draw_confirm(frame, b, *permanent, targets),
    }
}

fn draw_list(frame: &mut Frame, b: &Browser, area: Rect) {
    let dir = b.dir();
    if dir.entries.is_empty() {
        frame.render_widget(Line::from("  (empty directory)").dark_gray(), area);
        return;
    }
    let apparent = b.sort.apparent;
    let total = dir
        .entries
        .iter()
        .map(|e| e.size(apparent))
        .sum::<u64>()
        .max(1);
    let largest = dir
        .entries
        .iter()
        .map(|e| e.size(apparent))
        .max()
        .unwrap_or(0)
        .max(1);
    let width = area.width as usize;
    let lines: Vec<Line> = dir
        .entries
        .iter()
        .enumerate()
        .skip(b.offset)
        .take(area.height as usize)
        .map(|(i, e)| row(dir, e, i == b.cursor, apparent, total, largest, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn row(
    dir: &Dir,
    e: &Entry,
    is_cursor: bool,
    apparent: bool,
    total: u64,
    largest: u64,
    width: usize,
) -> Line<'static> {
    let size = e.size(apparent);
    let filled =
        ((size as u128 * BAR_WIDTH as u128).div_ceil(largest as u128) as usize).min(BAR_WIDTH);
    let filled = if size == 0 { 0 } else { filled };
    let selected = e.has(flag::SELECTED);
    let mark = if selected { '*' } else { ' ' };
    let prefix = format!(
        "{mark}{} {} {:>5.1}% [{}{}] ",
        flag_char(e),
        format_size(size),
        size as f64 * 100.0 / total as f64,
        "#".repeat(filled),
        " ".repeat(BAR_WIDTH - filled),
    );
    let name = String::from_utf8_lossy(dir.name(e));
    let name = if e.kind == Kind::Dir {
        format!("/{name}")
    } else {
        format!(" {name}")
    };
    let used = prefix.chars().count() + name.chars().count();
    let pad = " ".repeat(width.saturating_sub(used));

    let mut style = Style::default();
    if selected {
        style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
    }
    if is_cursor {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let name_style = if e.kind == Kind::Dir {
        style.bold()
    } else {
        style
    };
    Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(name, name_style),
        Span::styled(pad, style),
    ])
}

/// The same flag letters as ncdu.
fn flag_char(e: &Entry) -> char {
    if e.has(flag::ERROR) {
        '!'
    } else if e.has(flag::SUB_ERROR) {
        '.'
    } else if e.has(flag::OTHER_FS) {
        '>'
    } else if e.has(flag::HARDLINK) {
        'H'
    } else if matches!(e.kind, Kind::Symlink | Kind::Other) {
        '@'
    } else if e.kind == Kind::Dir && e.items == 1 {
        'e'
    } else {
        ' '
    }
}

/// Keeps the end of `text`, which holds the current folder name, within `max` characters.
fn tail(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("…{}", text.chars().skip(len - keep).collect::<String>())
}

fn footer_line(b: &Browser) -> Line<'static> {
    if let Some(message) = &b.message {
        return bar(&format!(" {message}"));
    }
    let t = b.dir().totals();
    let mut text = format!(
        " Total disk usage: {}  Apparent size: {}  Items: {}",
        format_size(t.disk).trim_start(),
        format_size(t.apparent).trim_start(),
        t.items
    );
    let (count, size) = b.selected().fold((0, 0), |(n, s), i| {
        (n + 1, s + b.dir().entries[i].size(b.sort.apparent))
    });
    if count > 0 {
        text.push_str(&format!(
            "   |  {count} selected: {}",
            format_size(size).trim_start()
        ));
    }
    let sort = match (b.sort.key, b.sort.apparent) {
        (SortKey::Size, false) => "disk usage",
        (SortKey::Size, true) => "apparent size",
        (SortKey::Name, _) => "name",
        (SortKey::Items, _) => "items",
    };
    text.push_str(&format!("   |  Sorted by {sort}"));
    bar(&text)
}

fn popup(frame: &mut Frame, width: u16, height: u16, title: &str, border: Color) -> Rect {
    let area = frame.area();
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::default().fg(border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

const HELP: &[(&str, &str)] = &[
    ("Up, k / Down, j", "Move the cursor"),
    ("Shift+Up/Down, K/J", "Extend the selection"),
    ("Space", "Select or deselect, then move down"),
    ("Ctrl+A / Esc", "Select all / clear the selection"),
    ("Enter, Right, l", "Open the directory"),
    ("Left, h, Backspace", "Go to the parent directory"),
    ("d", "Move to the Trash (selection or cursor)"),
    ("D", "Delete permanently"),
    ("u", "Undo the last move to the Trash"),
    ("s / n / C", "Sort by size / name / items (again: reverse)"),
    ("a", "Show apparent size or disk usage"),
    ("r", "Rescan this directory"),
    ("q", "Quit"),
];

fn draw_help(frame: &mut Frame) {
    let inner = popup(frame, 72, HELP.len() as u16 + 6, "Help", Color::Reset);
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                Span::from(format!(" {keys:<20}")).bold(),
                Span::from(*what),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(" Flags: ! read error  . error below  > other file system").dark_gray());
    lines.push(Line::from("        H hard link  @ symlink or special  e empty").dark_gray());
    lines.push(Line::from(""));
    lines.push(Line::from(" Press any key to close").dark_gray());
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_confirm(frame: &mut Frame, b: &Browser, permanent: bool, targets: &[usize]) {
    let dir = b.dir();
    let size: u64 = targets
        .iter()
        .map(|&i| dir.entries[i].size(b.sort.apparent))
        .sum();
    let what = if targets.len() == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", targets.len())
    };
    let (title, question, border) = if permanent {
        (
            "Delete permanently",
            format!("Delete {what} permanently? This cannot be undone."),
            Color::Red,
        )
    } else {
        (
            "Move to Trash",
            format!("Move {what} to the Trash?"),
            ACCENT,
        )
    };
    let shown = targets.len().min(6);
    let inner = popup(frame, 60, shown as u16 + 7, title, border);
    let mut lines = vec![
        Line::from(format!(" {question}")).bold(),
        Line::from(format!(" {} in total", format_size(size).trim_start())),
    ];
    lines.push(Line::from(""));
    for &i in &targets[..shown] {
        let e = &dir.entries[i];
        let slash = if e.kind == Kind::Dir { "/" } else { "" };
        lines.push(Line::from(format!(
            "   {slash}{}",
            String::from_utf8_lossy(dir.name(e))
        )));
    }
    if targets.len() > shown {
        lines.push(Line::from(format!("   … and {} more", targets.len() - shown)).dark_gray());
    }
    lines.push(Line::from(""));
    lines.push(Line::from(" y yes   any other key: no").dark_gray());
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use super::tail;

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(tail("~/a/b", 10), "~/a/b");
        assert_eq!(tail("/very/long/path/to/target", 10), "…to/target");
    }
}
