use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::progress::{self, Outcome};
use super::{trash, view};
use crate::cache::{self, Header, Source};
use crate::scan::{self, Options, Progress};
use crate::tree::{Dir, Entry, Sort, SortKey, Tree, flag, os_name};

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
    undo: Vec<UndoStep>,
    /// Whether `d` and `D` ask first. `A` in the question turns one off
    /// until quit, and the help screen turns it back on.
    pub ask_trash: bool,
    pub ask_delete: bool,
}

/// One move to the Trash. The removed entries keep their sizes and subtrees,
/// so undo puts them back in the tree without a rescan. `offset` is the
/// scroll position at the delete, so undo shows the same screen again.
struct UndoStep {
    items: Vec<trash::Trashed>,
    entries: Vec<(Vec<u8>, Entry)>,
    offset: usize,
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
    source: &Source,
    session: Option<Header>,
) -> io::Result<()> {
    let mut browser = Browser::new(tree, opts, source, session);
    let result = browser.event_loop(terminal);
    // Without this, the next run would load the stale tree again.
    if browser.changed
        && let Some(session) = browser.session.take()
    {
        cache::save_in_background(&session, &browser.tree.dir);
    }
    // Freeing millions of nodes takes time and the process ends right after.
    std::mem::forget(browser);
    result
}

impl Browser {
    fn new(tree: Tree, opts: Options, source: &Source, session: Option<Header>) -> Self {
        let mut browser = Browser {
            tree,
            stack: Vec::new(),
            cursor: 0,
            offset: 0,
            sort: Sort::default(),
            mode: Mode::Browse,
            message: describe(source),
            list_height: 1,
            range: None,
            opts,
            session,
            changed: false,
            undo: Vec::new(),
            ask_trash: true,
            ask_delete: true,
        };
        browser.apply_sort();
        browser
    }

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
                    self.delete(terminal, permanent, &targets)?;
                }
                Action::Rescan => self.rescan(terminal)?,
                Action::Undo => self.undo(),
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
                match key.code {
                    KeyCode::Char('t') if !self.ask_trash => self.ask_trash = true,
                    KeyCode::Char('p') if !self.ask_delete => self.ask_delete = true,
                    _ => self.mode = Mode::Browse,
                }
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
                    // Enter also opens folders, so a habit press must not
                    // delete for good. A move to the Trash can be undone.
                    KeyCode::Enter if !permanent => Action::Delete { permanent, targets },
                    // Shift is needed for the same reason: `a` switches the
                    // size in the list.
                    KeyCode::Char('A') => {
                        *self.ask_mut(permanent) = false;
                        Action::Delete { permanent, targets }
                    }
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
            KeyCode::Char('d') => return self.confirm(false),
            KeyCode::Char('D') => return self.confirm(true),
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
            self.offset = 0;
            // A folder opened for the first time is still in disk order, and
            // the sort keeps the cursor on its entry. So sort first, then put
            // the cursor on the first row.
            self.apply_sort();
            self.cursor = 0;
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
        let id = dir.entries.get(cursor).map(Entry::id);
        dir.sort(sort);
        if let Some(id) = id {
            self.cursor = dir.entries.iter().position(|e| e.id() == id).unwrap_or(0);
        }
    }

    fn confirm(&mut self, permanent: bool) -> Action {
        let mut targets: Vec<usize> = self.selected().collect();
        if targets.is_empty() && self.cursor < self.dir().entries.len() {
            targets.push(self.cursor);
        }
        if targets.is_empty() {
            Action::None
        } else if *self.ask_mut(permanent) {
            self.mode = Mode::Confirm { permanent, targets };
            Action::None
        } else {
            Action::Delete { permanent, targets }
        }
    }

    fn ask_mut(&mut self, permanent: bool) -> &mut bool {
        if permanent {
            &mut self.ask_delete
        } else {
            &mut self.ask_trash
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
        // Reconcile with the disk: drop what is gone, rescan what is still there.
        let mut pairs: Vec<(usize, PathBuf)> = targets.iter().copied().zip(paths).collect();
        pairs.sort_unstable_by_key(|&(i, _)| std::cmp::Reverse(i));
        let total = pairs.len();
        let mut gone = 0;
        let mut removed = Vec::new();
        for (i, path) in pairs {
            let dir = self.tree.dir_at_mut(&self.stack);
            if path.symlink_metadata().is_err() {
                removed.push(dir.take(i));
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
        let undoable = !trashed.is_empty();
        if undoable {
            self.undo.push(UndoStep {
                items: trashed,
                entries: removed,
                offset: self.offset,
            });
        }

        let done = if permanent {
            "Deleted"
        } else {
            "Moved to the Trash:"
        };
        let mut hint = if undoable { " Press u to undo." } else { "" }.to_string();
        if !*self.ask_mut(permanent) {
            hint.push_str(" Press ? to ask first again.");
        }
        self.message = Some(match result {
            Ok(()) => format!("{done} {}.{hint}", count(gone)),
            Err(e) => format!("{done} {gone} of {}.{hint} Error: {e}", count(total)),
        });
        Ok(())
    }

    /// Puts the latest move to the Trash back, then shows the folder it came
    /// back to, with the cursor on the first restored item.
    fn undo(&mut self) {
        let Some(step) = self.undo.pop() else {
            self.message = Some("Nothing to undo in this session".into());
            return;
        };
        let folder = step
            .items
            .first()
            .and_then(trash::parent_of)
            .map(Path::to_path_buf);
        let result = trash::restore(&step.items);
        if let Some(folder) = folder
            && let Some(stack) = self.stack_for(&folder)
        {
            self.put_back(&folder, stack, step.offset, step.entries);
        }
        let left = match self.undo.len() {
            0 => String::new(),
            n => format!(" Press u to undo {n} more."),
        };
        self.message = Some(match result {
            Ok(n) => format!("Restored {}.{left}", count(n)),
            Err(e) => format!("Could not restore everything: {e}.{left}"),
        });
    }

    /// Shows `folder` (at `stack`) and puts the entries that are on disk again
    /// back into it, with the sizes and subtrees from before the move, so no
    /// rescan is needed. The cursor goes to the first of them in the sorted
    /// list. The screen scrolls back to `offset`, the position at the delete,
    /// so the restored rows appear where they were. When that does not show
    /// the cursor, the cursor goes to the middle of the screen.
    fn put_back(
        &mut self,
        folder: &Path,
        stack: Vec<usize>,
        offset: usize,
        entries: Vec<(Vec<u8>, Entry)>,
    ) {
        self.clear_selection();
        self.stack = stack;
        self.offset = offset;
        let dir = self.tree.dir_at_mut(&self.stack);
        let wanted: HashSet<&[u8]> = entries.iter().map(|(name, _)| name.as_slice()).collect();
        // A new item with the same name may exist now. Keep it and skip ours.
        let present: HashSet<Vec<u8>> = dir
            .entries
            .iter()
            .map(|e| dir.name(e))
            .filter(|name| wanted.contains(name))
            .map(<[u8]>::to_vec)
            .collect();
        let mut restored = HashSet::new();
        for (name, mut entry) in entries {
            if present.contains(&name) || folder.join(os_name(&name)).symlink_metadata().is_err() {
                continue;
            }
            entry.flags &= !flag::SELECTED;
            restored.insert(dir.insert(&name, entry));
        }
        if restored.is_empty() {
            return;
        }
        self.tree.refresh_totals(&self.stack);
        self.changed = true;
        self.range = None;
        let sort = self.sort;
        let dir = self.tree.dir_at_mut(&self.stack);
        dir.sort(sort);
        if let Some(i) = dir.entries.iter().position(|e| restored.contains(&e.id())) {
            self.cursor = i;
        }
        let height = self.list_height.max(1);
        if self.cursor < self.offset || self.cursor >= self.offset + height {
            self.offset = self.cursor.saturating_sub(height / 2);
        }
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
        match progress::scan(terminal, &path, opts, self.dir().totals().items)? {
            Outcome::Done(fresh) => {
                self.replace_current(fresh.dir);
                self.changed = true;
                if self.stack.is_empty()
                    && let Some(session) = &mut self.session
                {
                    session.full_scan_at = cache::now();
                }
                self.message = Some("Rescanned".into());
            }
            Outcome::Failed(e) => self.message = Some(format!("Rescan failed: {e}")),
            Outcome::Cancelled | Outcome::Quit => self.message = Some("Rescan cancelled".into()),
        }
        Ok(())
    }

    /// Puts a fresh listing in place of the current directory. The cursor
    /// stays on the entry with the same name, because the fresh listing is in
    /// disk order and its indices mean nothing for the old rows.
    fn replace_current(&mut self, fresh: Dir) {
        let (sort, cursor) = (self.sort, self.cursor);
        let dir = self.dir();
        let name = dir.entries.get(cursor).map(|e| dir.name(e).to_vec());
        *self.tree.dir_at_mut(&self.stack) = fresh;
        self.tree.refresh_totals(&self.stack);
        self.range = None;
        let dir = self.tree.dir_at_mut(&self.stack);
        dir.sort(sort);
        self.cursor = name
            .and_then(|name| dir.find(&name))
            .unwrap_or_else(|| cursor.min(dir.entries.len().saturating_sub(1)));
    }
}

/// Says when the tree came from the cache, so stale data never looks fresh.
fn describe(source: &Source) -> Option<String> {
    let Source::Cached { listed, age_secs } = *source else {
        return match source {
            Source::MasterFileTable => {
                Some("Read the NTFS master file table directly. Press r to rescan.".to_string())
            }
            Source::ScannedNotElevated => Some(cache::ELEVATE_HINT.to_string()),
            _ => None,
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;

    /// Three files in disk order. Sorted by size, `large` comes first.
    fn in_disk_order() -> Dir {
        let mut dir = Dir::default();
        dir.push(b"small", Kind::File, 4096, 4096, 0);
        dir.push(b"large", Kind::File, 1 << 20, 1 << 20, 0);
        dir.push(b"middle", Kind::File, 1 << 16, 1 << 16, 0);
        dir
    }

    /// A browser on a root with one folder that was never opened.
    fn browser_with(folder: Dir) -> Browser {
        let mut root = Dir::default();
        root.push(b"folder", Kind::Dir, 0, 0, 0);
        root.attach(0, folder, 0);
        let tree = Tree {
            path: PathBuf::from("/root"),
            dir: Box::new(root),
        };
        Browser::new(tree, opts(), &Source::Scanned, None)
    }

    fn opts() -> Options {
        Options {
            one_fs: false,
            threads: 1,
            cache: false,
            mft: false,
            spread: true,
        }
    }

    /// A browser on a real folder with the three files of `in_disk_order`,
    /// so `put_back` finds them on disk without the Trash.
    fn browser_on_disk(folder: &Path) -> Browser {
        for name in ["small", "large", "middle"] {
            std::fs::write(folder.join(name), b"").unwrap();
        }
        let tree = Tree {
            path: folder.to_path_buf(),
            dir: Box::new(in_disk_order()),
        };
        Browser::new(tree, opts(), &Source::Scanned, None)
    }

    fn name_at_cursor(browser: &Browser) -> &[u8] {
        let dir = browser.dir();
        dir.name(&dir.entries[browser.cursor])
    }

    #[test]
    fn opening_a_folder_for_the_first_time_puts_the_cursor_on_the_top_row() {
        let mut browser = browser_with(in_disk_order());
        browser.open();
        assert_eq!(browser.cursor, 0);
        assert_eq!(name_at_cursor(&browser), b"large");
    }

    #[test]
    fn a_rescan_keeps_the_cursor_on_the_same_name() {
        let mut browser = browser_with(in_disk_order());
        browser.open();
        browser.move_cursor(1, false);
        assert_eq!(name_at_cursor(&browser), b"middle");
        browser.replace_current(in_disk_order());
        assert_eq!(name_at_cursor(&browser), b"middle");
    }

    #[test]
    fn a_rescan_keeps_the_cursor_in_range_when_its_entry_is_gone() {
        let mut browser = browser_with(in_disk_order());
        browser.open();
        browser.move_cursor(2, false);
        let mut fresh = Dir::default();
        fresh.push(b"large", Kind::File, 1 << 20, 1 << 20, 0);
        browser.replace_current(fresh);
        assert_eq!(name_at_cursor(&browser), b"large");
    }

    #[test]
    fn undo_puts_the_entry_back_without_a_rescan_and_keeps_the_screen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut browser = browser_on_disk(tmp.path());
        let before = browser.dir().totals();
        let i = browser.dir().find(b"middle").unwrap();
        let taken = browser.tree.dir_at_mut(&[]).take(i);
        // The user scrolled away after the delete, which was at offset 1.
        (browser.cursor, browser.offset) = (0, 0);
        browser.put_back(tmp.path(), Vec::new(), 1, vec![taken]);
        assert_eq!(name_at_cursor(&browser), b"middle");
        assert_eq!(browser.cursor, i);
        assert_eq!(browser.dir().totals(), before);
        assert_eq!(browser.offset, 1, "the screen of the delete comes back");
    }

    #[test]
    fn undo_puts_the_cursor_in_the_middle_when_the_old_screen_hides_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut browser = browser_on_disk(tmp.path());
        browser.list_height = 2;
        let i = browser.dir().find(b"small").unwrap();
        let taken = browser.tree.dir_at_mut(&[]).take(i);
        browser.put_back(tmp.path(), Vec::new(), 0, vec![taken]);
        assert_eq!((browser.cursor, browser.offset), (2, 1));
    }

    #[test]
    fn undo_skips_entries_that_are_not_on_disk_or_already_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut browser = browser_on_disk(tmp.path());
        let mut spare = in_disk_order();
        let large = spare.find(b"large").unwrap();
        // The tree still lists "large", for example a new item with that name.
        let listed = spare.take(large);
        // Nothing named "elsewhere" is on disk.
        let (_, entry) = spare.take(0);
        let missing = (b"elsewhere".to_vec(), entry);
        let before = browser.dir().entries.len();
        browser.put_back(tmp.path(), Vec::new(), 0, vec![listed, missing]);
        assert_eq!(browser.dir().entries.len(), before);
    }

    #[test]
    fn enter_confirms_a_move_to_the_trash_but_not_a_permanent_delete() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mut browser = browser_with(in_disk_order());
        for (permanent, deletes) in [(false, true), (true, false)] {
            browser.mode = Mode::Confirm {
                permanent,
                targets: vec![0],
            };
            let action = browser.handle(enter);
            assert_eq!(matches!(action, Action::Delete { .. }), deletes);
            assert!(matches!(browser.mode, Mode::Browse));
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn shift_a_stops_one_question_until_the_help_screen_turns_it_back_on() {
        for (permanent, same, other, again) in [(false, 'd', 'D', 't'), (true, 'D', 'd', 'p')] {
            let mut browser = browser_with(in_disk_order());
            browser.mode = Mode::Confirm {
                permanent,
                targets: vec![0],
            };
            // A habit press of `a` answers no.
            assert!(matches!(browser.handle(key('a')), Action::None));
            assert!(browser.ask_trash && browser.ask_delete);
            browser.mode = Mode::Confirm {
                permanent,
                targets: vec![0],
            };
            assert!(matches!(browser.handle(key('A')), Action::Delete { .. }));
            // The same key now deletes at once, and the other key still asks.
            let action = browser.handle(key(same));
            assert!(matches!(action, Action::Delete { permanent: p, .. } if p == permanent));
            assert!(matches!(browser.mode, Mode::Browse));
            assert!(matches!(browser.handle(key(other)), Action::None));
            assert!(matches!(browser.mode, Mode::Confirm { .. }));
            browser.handle(key('n'));
            // The help screen stays open when it turns the question back on.
            browser.handle(key('?'));
            browser.handle(key(again));
            assert!(matches!(browser.mode, Mode::Help));
            assert!(browser.ask_trash && browser.ask_delete);
            browser.handle(key(again));
            assert!(matches!(browser.mode, Mode::Browse));
            browser.handle(key(same));
            assert!(matches!(browser.mode, Mode::Confirm { .. }));
        }
    }

    fn screen(browser: &mut Browser) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| view::draw(frame, browser)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_popups_show_their_last_line() {
        let mut folder = Dir::default();
        for i in 0..8 {
            folder.push(&[b'f', b'0' + i], Kind::File, 4096, 4096, 0);
        }
        let mut browser = browser_with(folder);
        browser.stack = vec![0];
        browser.mode = Mode::Help;
        assert!(screen(&mut browser).contains("Press any key to close"));
        browser.ask_delete = false;
        assert!(screen(&mut browser).contains("D asks first: no"));
        browser.mode = Mode::Confirm {
            permanent: true,
            targets: (0..8).collect(),
        };
        let shown = screen(&mut browser);
        assert!(shown.contains("and 2 more"));
        assert!(shown.contains("Shift+A: yes, and do not ask again"));
    }

    #[test]
    fn the_status_line_suggests_an_elevated_run_only_when_it_helps() {
        assert_eq!(
            describe(&Source::ScannedNotElevated).as_deref(),
            Some(cache::ELEVATE_HINT)
        );
        assert_eq!(describe(&Source::Scanned), None);
    }
}
