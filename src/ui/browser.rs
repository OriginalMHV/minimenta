use std::io;
use std::path::{Component, Path, PathBuf};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::progress::{self, Outcome};
use super::{trash, view};
use crate::cache::{self, Header, Source};
use crate::scan::{self, Options, Progress};
use crate::tree::{Dir, Sort, SortKey, Tree, flag, os_name};

pub enum Mode {
    Browse,
    Help,
    Confirm {
        permanent: bool,
        targets: Vec<usize>,
    },
}

/// A Shift+arrow selection in progress: the anchor row and the selection
/// that existed before the range started.
struct Range {
    anchor: usize,
    before: Vec<bool>,
}

pub struct Browser {
    pub tree: Tree,
    pub stack: Vec<usize>,
    pub cursor: usize,
    pub offset: usize,
    pub sort: Sort,
    pub mode: Mode,
    pub message: Option<String>,
    pub list_height: usize,
    range: Option<Range>,
    opts: Options,
    /// Set when a rescan or delete corrected the tree, so it is saved on quit.
    session: Option<Header>,
    changed: bool,
    /// Every move to the Trash in this session, the latest last, so `u` can
    /// put them back one after the other.
    undo: Vec<Vec<trash::Trashed>>,
}

enum Action {
    None,
    Quit,
    Delete {
        permanent: bool,
        targets: Vec<usize>,
    },
    Rescan,
    Undo,
}

pub fn run(
    terminal: &mut DefaultTerminal,
    tree: Tree,
    opts: Options,
    source: Source,
    session: Option<Header>,
) -> io::Result<()> {
    let mut browser = Browser {
        tree,
        stack: Vec::new(),
        cursor: 0,
        offset: 0,
        sort: Sort::default(),
        mode: Mode::Browse,
        message: describe(&source),
        list_height: 1,
        range: None,
        opts,
        session,
        changed: false,
        undo: Vec::new(),
    };
    browser.apply_sort();
    let result = browser.event_loop(terminal);
    // Without this, the next run would load the stale tree again.
    if browser.changed
        && let Some(session) = browser.session.take()
    {
        cache::save_in_background(session, &browser.tree.dir);
    }
    // Freeing millions of nodes takes time and the process ends right after.
    std::mem::forget(browser);
    result
}

impl Browser {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            terminal.draw(|frame| view::draw(frame, self))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match self.handle(key) {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Delete { permanent, targets } => {
                    self.delete(terminal, permanent, &targets)?
                }
                Action::Rescan => self.rescan(terminal)?,
                Action::Undo => self.undo(terminal)?,
            }
        }
    }

    pub fn dir(&self) -> &Dir {
        self.tree.dir_at(&self.stack)
    }

    pub fn current_path(&self) -> PathBuf {
        let mut path = self.tree.path.clone();
        let mut dir = &*self.tree.dir;
        for &i in &self.stack {
            let e = &dir.entries[i];
            path.push(os_name(dir.name(e)));
            dir = e.dir.as_ref().expect("stack points to a directory");
        }
        path
    }

    pub fn selected(&self) -> impl Iterator<Item = usize> + '_ {
        self.dir()
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.has(flag::SELECTED))
            .map(|(i, _)| i)
    }

    fn handle(&mut self, key: KeyEvent) -> Action {
        self.message = None;
        match self.mode {
            Mode::Help => {
                self.mode = Mode::Browse;
                return Action::None;
            }
            Mode::Confirm { .. } => {
                let Mode::Confirm { permanent, targets } =
                    std::mem::replace(&mut self.mode, Mode::Browse)
                else {
                    unreachable!()
                };
                return match key.code {
                    KeyCode::Char('y' | 'Y') => Action::Delete { permanent, targets },
                    _ => Action::None,
                };
            }
            Mode::Browse => {}
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.list_height.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('a') if ctrl => self.select_all(),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1, shift),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1, shift),
            KeyCode::Char('K') => self.move_cursor(-1, true),
            KeyCode::Char('J') => self.move_cursor(1, true),
            KeyCode::PageUp => self.move_cursor(-page, shift),
            KeyCode::PageDown => self.move_cursor(page, shift),
            KeyCode::Home => self.move_cursor(isize::MIN / 2, shift),
            KeyCode::End => self.move_cursor(isize::MAX / 2, shift),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h' | '<') => self.up(),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Esc => self.clear_selection(),
            KeyCode::Char('s') => self.set_sort(SortKey::Size),
            KeyCode::Char('n') => self.set_sort(SortKey::Name),
            KeyCode::Char('C') => self.set_sort(SortKey::Items),
            KeyCode::Char('a') => {
                self.sort.apparent = !self.sort.apparent;
                self.apply_sort();
            }
            KeyCode::Char('d') => self.confirm(false),
            KeyCode::Char('D') => self.confirm(true),
            KeyCode::Char('r') => return Action::Rescan,
            KeyCode::Char('u') => return Action::Undo,
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Action::None
    }

    fn move_cursor(&mut self, delta: isize, extend: bool) {
        let len = self.dir().entries.len();
        if len == 0 {
            return;
        }
        if !extend {
            self.range = None;
        } else if self.range.is_none() {
            let before = self
                .dir()
                .entries
                .iter()
                .map(|e| e.has(flag::SELECTED))
                .collect();
            self.range = Some(Range {
                anchor: self.cursor,
                before,
            });
        }
        self.cursor = self.cursor.saturating_add_signed(delta).min(len - 1);
        if let Some(range) = &self.range {
            let (lo, hi) = (range.anchor.min(self.cursor), range.anchor.max(self.cursor));
            let dir = self.tree.dir_at_mut(&self.stack);
            for (i, e) in dir.entries.iter_mut().enumerate() {
                set_selected(e, range.before[i] || (lo..=hi).contains(&i));
            }
        }
    }

    fn toggle(&mut self) {
        self.range = None;
        let cursor = self.cursor;
        if let Some(e) = self.tree.dir_at_mut(&self.stack).entries.get_mut(cursor) {
            e.flags ^= flag::SELECTED;
            self.move_cursor(1, false);
        }
    }

    fn select_all(&mut self) {
        self.range = None;
        for e in &mut self.tree.dir_at_mut(&self.stack).entries {
            e.flags |= flag::SELECTED;
        }
    }

    fn clear_selection(&mut self) {
        self.range = None;
        for e in &mut self.tree.dir_at_mut(&self.stack).entries {
            e.flags &= !flag::SELECTED;
        }
    }

    fn open(&mut self) {
        let cursor = self.cursor;
        if self
            .dir()
            .entries
            .get(cursor)
            .is_some_and(|e| e.dir.is_some())
        {
            self.clear_selection();
            self.stack.push(cursor);
            self.cursor = 0;
            self.offset = 0;
            self.apply_sort();
        }
    }

    fn up(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        self.clear_selection();
        self.cursor = self.stack.pop().expect("stack is not empty");
        self.offset = 0;
        self.apply_sort();
    }

    fn set_sort(&mut self, key: SortKey) {
        self.sort.reverse = self.sort.key == key && !self.sort.reverse;
        self.sort.key = key;
        self.apply_sort();
    }

    /// Sorts the current directory if its order is stale.
    fn apply_sort(&mut self) {
        if self.dir().sort != Some(self.sort) {
            self.resort();
        }
    }

    /// Sorts the current directory and keeps the cursor on the same entry.
    fn resort(&mut self) {
        self.range = None;
        let (sort, cursor) = (self.sort, self.cursor);
        let dir = self.tree.dir_at_mut(&self.stack);
        let id = dir.entries.get(cursor).map(|e| e.id());
        dir.sort(sort);
        if let Some(id) = id {
            self.cursor = dir.entries.iter().position(|e| e.id() == id).unwrap_or(0);
        }
    }

    fn confirm(&mut self, permanent: bool) {
        let mut targets: Vec<usize> = self.selected().collect();
        if targets.is_empty() && self.cursor < self.dir().entries.len() {
            targets.push(self.cursor);
        }
        if !targets.is_empty() {
            self.mode = Mode::Confirm { permanent, targets };
        }
    }

    fn delete(
        &mut self,
        terminal: &mut DefaultTerminal,
        permanent: bool,
        targets: &[usize],
    ) -> io::Result<()> {
        let base = self.current_path();
        let paths: Vec<PathBuf> = {
            let dir = self.dir();
            targets
                .iter()
                .map(|&i| base.join(os_name(dir.name(&dir.entries[i]))))
                .collect()
        };
        let verb = if permanent {
            "Deleting"
        } else {
            "Moving to the Trash:"
        };
        self.message = Some(format!("{verb} {} …", count(paths.len())));
        terminal.draw(|frame| view::draw(frame, self))?;

        let (trashed, result) = if permanent {
            (Vec::new(), remove_all(&paths))
        } else {
            trash::move_to_trash(&paths)
        };
        let undoable = !trashed.is_empty();
        if undoable {
            self.undo.push(trashed);
        }

        // Reconcile with the disk: drop what is gone, rescan what is still there.
        let mut pairs: Vec<(usize, PathBuf)> = targets.iter().copied().zip(paths).collect();
        pairs.sort_unstable_by_key(|&(i, _)| std::cmp::Reverse(i));
        let total = pairs.len();
        let mut gone = 0;
        for (i, path) in pairs {
            let dir = self.tree.dir_at_mut(&self.stack);
            if path.symlink_metadata().is_err() {
                dir.entries.remove(i);
                gone += 1;
            } else if dir.entries[i].dir.is_some()
                && let Ok(sub) = scan::scan(&path, &self.opts, &Progress::default())
            {
                let e = &mut dir.entries[i];
                e.flags &= !(flag::SUB_ERROR | flag::ERROR);
                (e.disk, e.apparent) = (0, 0);
                dir.attach(i, sub, 0);
            }
        }
        self.tree.refresh_totals(&self.stack);
        self.changed = true;
        self.clear_selection();
        self.cursor = self.cursor.min(self.dir().entries.len().saturating_sub(1));
        self.resort();

        let done = if permanent {
            "Deleted"
        } else {
            "Moved to the Trash:"
        };
        let hint = if undoable { " Press u to undo." } else { "" };
        self.message = Some(match result {
            Ok(()) => format!("{done} {}.{hint}", count(gone)),
            Err(e) => format!("{done} {gone} of {}.{hint} Error: {e}", count(total)),
        });
        Ok(())
    }

    /// Puts the latest batch from the Trash back, then shows the folder it
    /// came back to, with the cursor on the first restored item.
    fn undo(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let Some(batch) = self.undo.pop() else {
            self.message = Some("Nothing to undo in this session".into());
            return Ok(());
        };
        let first = batch
            .first()
            .and_then(|t| t.original.file_name())
            .map(|n| n.as_encoded_bytes().to_vec());
        let folder = batch
            .first()
            .and_then(trash::parent_of)
            .map(Path::to_path_buf);
        let result = trash::restore(&batch);
        if let Some(stack) = folder.and_then(|f| self.stack_for(&f)) {
            self.clear_selection();
            self.stack = stack;
            self.offset = 0;
            self.rescan(terminal)?;
            if let Some(i) = first.and_then(|name| self.dir().find(&name)) {
                self.cursor = i;
            }
        }
        let left = match self.undo.len() {
            0 => String::new(),
            n => format!(" Press u to undo {n} more."),
        };
        self.message = Some(match result {
            Ok(n) => format!("Restored {}.{left}", count(n)),
            Err(e) => format!("Could not restore everything: {e}.{left}"),
        });
        Ok(())
    }

    /// The stack of entry indices that leads to `folder`, if it is in the tree.
    fn stack_for(&self, folder: &Path) -> Option<Vec<usize>> {
        let mut stack = Vec::new();
        let mut dir = &*self.tree.dir;
        for component in folder.strip_prefix(&self.tree.path).ok()?.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            let i = dir.find(name.as_encoded_bytes())?;
            stack.push(i);
            dir = dir.entries[i].dir.as_deref()?;
        }
        Some(stack)
    }

    fn rescan(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let path = self.current_path();
        let opts = Options {
            cache: false,
            ..self.opts
        };
        match progress::scan(terminal, &path, opts)? {
            Outcome::Done(fresh) => {
                *self.tree.dir_at_mut(&self.stack) = fresh.dir;
                self.tree.refresh_totals(&self.stack);
                self.changed = true;
                if self.stack.is_empty()
                    && let Some(session) = &mut self.session
                {
                    session.full_scan_at = cache::now();
                }
                self.range = None;
                self.cursor = self.cursor.min(self.dir().entries.len().saturating_sub(1));
                self.resort();
                self.message = Some("Rescanned".into());
            }
            Outcome::Failed(e) => self.message = Some(format!("Rescan failed: {e}")),
            Outcome::Cancelled | Outcome::Quit => self.message = Some("Rescan cancelled".into()),
        }
        Ok(())
    }
}

/// Says when the tree came from the cache, so stale data never looks fresh.
fn describe(source: &Source) -> Option<String> {
    let Source::Cached { listed, age_secs } = *source else {
        return matches!(source, Source::MasterFileTable)
            .then(|| "Read the NTFS master file table directly. Press r to rescan.".to_string());
    };
    let age = match age_secs {
        0..60 => "less than a minute".to_string(),
        60..3600 => format!("{} min", age_secs / 60),
        3600..86400 => format!("{} h", age_secs / 3600),
        _ => format!("{} days", age_secs / 86400),
    };
    let dirs = if listed == 1 {
        "directory"
    } else {
        "directories"
    };
    Some(format!(
        "Cached scan: {listed} changed {dirs} listed again, last full scan {age} ago. Press r to rescan."
    ))
}

fn set_selected(e: &mut crate::tree::Entry, on: bool) {
    if on {
        e.flags |= flag::SELECTED;
    } else {
        e.flags &= !flag::SELECTED;
    }
}

fn count(n: usize) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    }
}

fn remove_all(paths: &[PathBuf]) -> Result<(), String> {
    let mut first_error = None;
    for path in paths {
        let is_dir = path.symlink_metadata().is_ok_and(|m| m.is_dir());
        let result = if is_dir {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if let Err(e) = result {
            first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
        }
    }
    first_error.map_or(Ok(()), Err)
}
