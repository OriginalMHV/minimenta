use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{display_path, view};

const PROMPT: &str = "  > ";

pub struct Prompt {
    input: Vec<char>,
    cursor: usize,
    error: Option<String>,
    candidates: Vec<String>,
}

impl Prompt {
    pub fn new() -> io::Result<Self> {
        let mut prompt = Prompt {
            input: Vec::new(),
            cursor: 0,
            error: None,
            candidates: Vec::new(),
        };
        prompt.set_path(&std::env::current_dir()?);
        Ok(prompt)
    }

    pub fn set_path(&mut self, path: &Path) {
        let mut text = display_path(path);
        if !text.ends_with('/') {
            text.push('/');
        }
        self.input = text.chars().collect();
        self.cursor = self.input.len();
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Returns the directory to scan, or `None` when the user quits.
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Option<PathBuf>> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match self.handle(key) {
                Step::Continue => {}
                Step::Quit => return Ok(None),
                Step::Scan(path) => return Ok(Some(path)),
            }
        }
    }

    fn handle(&mut self, key: KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code != KeyCode::Tab {
            self.candidates.clear();
        }
        match key.code {
            KeyCode::Esc => return Step::Quit,
            KeyCode::Char('c') if ctrl => return Step::Quit,
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab => self.complete(),
            KeyCode::Char('u') if ctrl => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let mut start = self.cursor;
                while start > 0 && self.input[start - 1] == '/' {
                    start -= 1;
                }
                while start > 0 && self.input[start - 1] != '/' {
                    start -= 1;
                }
                self.input.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.input.len(),
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.input.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.input.len() => {
                self.input.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            _ => {}
        }
        self.error = None;
        Step::Continue
    }

    fn text(&self) -> String {
        self.input.iter().collect()
    }

    fn submit(&mut self) -> Step {
        let text = self.text();
        let text = text.trim();
        let path = if text.is_empty() {
            PathBuf::from(".")
        } else {
            expand_home(text)
        };
        match path.canonicalize() {
            Ok(path) if path.is_dir() => Step::Scan(path),
            Ok(_) => {
                self.error = Some(format!("{text} is not a directory"));
                Step::Continue
            }
            Err(e) => {
                self.error = Some(format!("{text}: {e}"));
                Step::Continue
            }
        }
    }

    /// Completes the last path component from the directories that exist.
    fn complete(&mut self) {
        let text = self.text();
        let split = text.rfind('/').map_or(0, |i| i + 1);
        let (parent, prefix) = text.split_at(split);
        let lookup = if parent.is_empty() {
            PathBuf::from(".")
        } else {
            expand_home(parent)
        };
        let Ok(entries) = fs::read_dir(lookup) else {
            return;
        };
        let lower = prefix.to_lowercase();
        let mut matches: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| prefix.starts_with('.') || !name.starts_with('.'))
            .filter(|name| name.to_lowercase().starts_with(&lower))
            .collect();
        matches.sort_by_key(|name| name.to_lowercase());

        let completed = match matches.as_slice() {
            [] => return,
            [only] => format!("{parent}{only}/"),
            [first, rest @ ..] => {
                self.candidates = matches.clone();
                let common = rest
                    .iter()
                    .map(|name| common_len(first, name))
                    .min()
                    .unwrap_or(0);
                if common <= prefix.chars().count() {
                    return;
                }
                format!("{parent}{}", first.chars().take(common).collect::<String>())
            }
        };
        self.input = completed.chars().collect();
        self.cursor = self.input.len();
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        let [header, _, question, _, input, _, hint, _, extra, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(view::header_bar(), header);
        frame.render_widget(
            Line::from("  Which directory do you want to scan?").bold(),
            question,
        );

        let text = self.text();
        let width = (input.width as usize).saturating_sub(PROMPT.len() + 1);
        let skip = self.cursor.saturating_sub(width);
        let visible: String = text.chars().skip(skip).take(width).collect();
        frame.render_widget(Line::from(format!("{PROMPT}{visible}")), input);
        frame.set_cursor_position((
            input.x + (PROMPT.len() + self.cursor - skip) as u16,
            input.y,
        ));

        frame.render_widget(
            Line::from("  Enter scan   Tab complete   Ctrl+U clear   Esc quit").dark_gray(),
            hint,
        );
        let lines: Vec<Line> = match &self.error {
            Some(error) => vec![Line::styled(
                format!("  {error}"),
                Style::default().fg(Color::Red),
            )],
            None => self
                .candidates
                .iter()
                .map(|c| Line::from(format!("    {c}/")).dark_gray())
                .collect(),
        };
        frame.render_widget(Paragraph::new(lines), extra);
        frame.render_widget(view::bar(""), footer);
    }
}

enum Step {
    Continue,
    Quit,
    Scan(PathBuf),
}

/// Length of the shared start of two names, ignoring case.
fn common_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x.to_lowercase().eq(y.to_lowercase()))
        .count()
}

fn expand_home(text: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(text: &str) -> Prompt {
        Prompt {
            input: text.chars().collect(),
            cursor: text.chars().count(),
            error: None,
            candidates: vec![],
        }
    }

    #[test]
    fn tab_completes_a_unique_directory_and_ignores_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("Projects")).unwrap();
        fs::write(tmp.path().join("Pictures.txt"), "").unwrap();
        let mut p = prompt(&format!("{}/pro", tmp.path().display()));
        p.complete();
        assert_eq!(p.text(), format!("{}/Projects/", tmp.path().display()));
    }

    #[test]
    fn tab_extends_to_the_common_prefix_and_lists_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("target-debug")).unwrap();
        fs::create_dir(tmp.path().join("target-release")).unwrap();
        let mut p = prompt(&format!("{}/t", tmp.path().display()));
        p.complete();
        assert_eq!(p.text(), format!("{}/target-", tmp.path().display()));
        assert_eq!(p.candidates, ["target-debug", "target-release"]);
    }

    #[test]
    fn hidden_directories_need_a_leading_dot() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir(tmp.path().join("src")).unwrap();
        let mut p = prompt(&format!("{}/", tmp.path().display()));
        p.complete();
        assert_eq!(p.text(), format!("{}/src/", tmp.path().display()));
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/x"), home.join("x"));
        assert_eq!(expand_home("~x"), PathBuf::from("~x"));
    }
}
