use std::fmt::Write;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::browser::{Browser, Mode};
use super::display_path;
use crate::tree::{Dir, Entry, Kind, SortKey, flag, format_size};

const BAR_WIDTH: usize = 10;

/// Rows that stay visible above and below the cursor while it moves, so the
/// next items show before the cursor reaches the edge of the screen.
const SCROLL_MARGIN: usize = 5;

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
    b.offset = scroll(b.offset, b.cursor, b.list_height, b.dir().entries.len());
    draw_list(frame, b, list);
    frame.render_widget(footer_line(b, footer.width as usize), footer);

    match &b.mode {
        Mode::Browse => {}
        Mode::Help => draw_help(frame, b),
        Mode::Confirm { permanent, targets } => draw_confirm(frame, b, *permanent, targets),
    }
}

/// The first visible row for `cursor` on a screen of `height` rows, starting
/// from the current `offset`. The screen moves only when the cursor comes
/// within `SCROLL_MARGIN` rows of an edge, and the margin is at most a third
/// of the screen. At the start and the end of the list the margin shrinks, so
/// the first and the last row stay reachable.
fn scroll(offset: usize, cursor: usize, height: usize, len: usize) -> usize {
    let margin = SCROLL_MARGIN.min(height.saturating_sub(1) / 3);
    let offset = if cursor < offset + margin {
        cursor.saturating_sub(margin)
    } else if cursor + margin >= offset + height {
        (cursor + margin + 1).saturating_sub(height)
    } else {
        offset
    };
    offset.min(len.saturating_sub(height))
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
    let filled = ((u128::from(size) * BAR_WIDTH as u128).div_ceil(u128::from(largest)) as usize)
        .min(BAR_WIDTH);
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

fn footer_line(b: &Browser, width: usize) -> Line<'static> {
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
        let _ = write!(
            text,
            "   |  {count} selected: {}",
            format_size(size).trim_start()
        );
    }
    let sort = match (b.sort.key, b.sort.apparent) {
        (SortKey::Size, false) => "disk usage",
        (SortKey::Size, true) => "apparent size",
        (SortKey::Name, _) => "name",
        (SortKey::Items, _) => "items",
    };
    let _ = write!(text, "   |  Sorted by {sort}");
    let len = b.dir().entries.len();
    if len > 0 {
        let position = format!("{}/{len} ", b.cursor + 1);
        text = right_align(text, &position, width);
    }
    bar(&text)
}

/// Puts `right` at the right edge of a line of `width` columns, after `text`.
/// When both do not fit, `right` follows `text` after a separator.
fn right_align(mut text: String, right: &str, width: usize) -> String {
    let used = text.chars().count() + right.chars().count();
    if used + 3 <= width {
        text.push_str(&" ".repeat(width - used));
    } else {
        text.push_str("   |  ");
    }
    text.push_str(right);
    text
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

fn draw_help(frame: &mut Frame, b: &Browser) {
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
    lines.push(asks_first('d', b.ask_trash, 't'));
    lines.push(asks_first('D', b.ask_delete, 'p'));
    lines.push(Line::from(""));
    lines.push(Line::from(" Press any key to close").dark_gray());
    let inner = popup(frame, 72, lines.len() as u16 + 2, "Help", Color::Reset);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn asks_first(key: char, ask: bool, again: char) -> Line<'static> {
    if ask {
        Line::from(format!(" {key} asks first: yes")).dark_gray()
    } else {
        Line::from(format!(
            " {key} asks first: no, until you quit. Press {again} to ask again."
        ))
        .bold()
    }
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
    let keys = if permanent {
        " y yes   any other key: no"
    } else {
        " y or Enter: yes   any other key: no"
    };
    lines.push(Line::from(keys).dark_gray());
    lines.push(Line::from(" Shift+A: yes, and do not ask again until you quit").dark_gray());
    let inner = popup(frame, 60, lines.len() as u16 + 2, title, border);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

#[cfg(test)]
mod tests {
    use super::{right_align, scroll, tail};

    #[test]
    fn moving_down_keeps_five_rows_below_the_cursor() {
        // 70 rows on screen, 400 items.
        assert_eq!(scroll(0, 64, 70, 400), 0);
        assert_eq!(scroll(0, 65, 70, 400), 1);
        assert_eq!(scroll(1, 66, 70, 400), 2);
    }

    #[test]
    fn moving_up_keeps_five_rows_above_the_cursor() {
        assert_eq!(scroll(100, 105, 70, 400), 100);
        assert_eq!(scroll(100, 104, 70, 400), 99);
        assert_eq!(scroll(3, 2, 70, 400), 0);
    }

    #[test]
    fn the_margin_shrinks_at_the_ends_of_the_list() {
        assert_eq!(scroll(0, 0, 70, 400), 0);
        assert_eq!(scroll(330, 399, 70, 400), 330);
        assert_eq!(scroll(0, 9, 70, 10), 0);
    }

    #[test]
    fn a_short_screen_keeps_at_most_a_third_as_margin() {
        // 7 rows: the margin is 2, so the cursor scrolls at row 5.
        assert_eq!(scroll(0, 4, 7, 100), 0);
        assert_eq!(scroll(0, 5, 7, 100), 1);
        // 2 rows: no margin.
        assert_eq!(scroll(0, 1, 2, 100), 0);
    }

    #[test]
    fn the_position_sits_at_the_right_edge_when_it_fits() {
        assert_eq!(
            right_align(" Items: 3".into(), "2/5 ", 20),
            " Items: 3       2/5 "
        );
        assert_eq!(
            right_align(" Items: 3".into(), "2/5 ", 12),
            " Items: 3   |  2/5 "
        );
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(tail("~/a/b", 10), "~/a/b");
        assert_eq!(tail("/very/long/path/to/target", 10), "…to/target");
    }
}
