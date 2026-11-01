//! The search screen of the browser: keys, the search thread and the delete
//! of results.
//!
//! The search runs on its own thread and shares the root of the tree through
//! an `Arc`, because a copy of millions of entries would cost more than the
//! search. Every change to the tree goes through `Arc::make_mut`, which would
//! copy the tree if the thread still held the root. So `freeze_search` stops
//! the thread and waits for it before any change. `step` calls it for every
//! key that is not a plain move, and `poll_jobs` calls it before it changes
//! the tree. A search that it stopped runs again after the change. A debug
//! build panics if a change finds the root shared.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, Browser, Mode};
use crate::search::{self, Hit, Outcome, Query};
use crate::tree::{dir_in, os_name};

/// The pages of the search help.
pub const HELP_PAGES: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Query,
    Results,
}

/// Where the browser view was when the search started, so Esc can show it
/// again.
struct Saved {
    cursor: usize,
    /// The name under the cursor, to find it again if the order changed.
    name: Option<Vec<u8>>,
    offset: usize,
}

struct Running {
    cancel: Arc<AtomicBool>,
    results: Receiver<Option<Outcome>>,
    handle: JoinHandle<()>,
}

pub struct Search {
    pub query: String,
    pub focus: Focus,
    pub hits: Vec<Hit>,
    /// All matches, also those that `hits` leaves out.
    pub matches: u64,
    pub total: u64,
    pub error: Option<String>,
    pub cursor: usize,
    pub offset: usize,
    pub help_page: usize,
    saved: Saved,
    running: Option<Running>,
    /// A search was stopped or the tree changed, so it must run again.
    pub(super) rerun: bool,
    /// The next result starts at the top. A rerun keeps the cursor.
    reset: bool,
    /// A result that came while a question was open. The question holds
    /// indices of `hits`, so the list must not change under it.
    pub(super) waiting: Option<Outcome>,
}

impl Search {
    fn new(saved: Saved) -> Self {
        Search {
            query: String::new(),
            focus: Focus::Query,
            hits: Vec::new(),
            matches: 0,
            total: 0,
            error: None,
            cursor: 0,
            offset: 0,
            help_page: 0,
            saved,
            running: None,
            rerun: false,
            reset: true,
            waiting: None,
        }
    }

    pub fn searching(&self) -> bool {
        self.running.is_some() || self.rerun
    }

    pub fn selected(&self) -> impl Iterator<Item = usize> + '_ {
        self.hits
            .iter()
            .enumerate()
            .filter(|(_, h)| h.selected)
            .map(|(i, _)| i)
    }

    fn clear_hits(&mut self) {
        self.hits.clear();
        (self.matches, self.total, self.cursor, self.offset) = (0, 0, 0, 0);
    }

    fn apply(&mut self, outcome: Outcome) {
        self.hits = outcome.hits;
        self.matches = outcome.matches;
        self.total = outcome.total;
        self.error = None;
        if std::mem::take(&mut self.reset) {
            (self.cursor, self.offset) = (0, 0);
        }
        self.cursor = self.cursor.min(self.hits.len().saturating_sub(1));
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.hits.len();
        if len > 0 {
            self.cursor = self.cursor.saturating_add_signed(delta).min(len - 1);
        }
    }

    /// Splits a result path into the folder below the search folder and the name.
    fn split(rel: &[u8]) -> (&[u8], &[u8]) {
        match rel.iter().rposition(|&b| b == b'/') {
            Some(i) => (&rel[..i], &rel[i + 1..]),
            None => (&[], rel),
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.freeze_search();
    }
}

impl Browser {
    pub(super) fn open_search(&mut self) {
        // Only the open folder can hold selection flags: `open` and `up`
        // clear them before they move.
        self.clear_selection();
        let dir = self.dir();
        let name = dir.entries.get(self.cursor).map(|e| dir.name(e).to_vec());
        let saved = Saved {
            cursor: self.cursor,
            name,
            offset: self.offset,
        };
        self.search = Some(Search::new(saved));
    }

    /// Stops the search thread and waits for it, so the tree may change. A
    /// search that did not finish runs again later.
    /// Returns true when it showed a new list that the user has not seen.
    pub(super) fn freeze_search(&mut self) -> bool {
        let Some(running) = self.search.as_mut().and_then(|s| s.running.take()) else {
            return false;
        };
        running.cancel.store(true, Ordering::Relaxed);
        let _ = running.handle.join();
        if let Ok(Some(outcome)) = running.results.try_recv() {
            return self.finish_search(outcome);
        }
        if let Some(s) = &mut self.search {
            s.rerun = true;
        }
        false
    }

    /// Whether the key acts on the rows of the list: a delete, a selection or
    /// opening a result. After a new list appeared under the key, the user
    /// has not seen the rows that the key would act on.
    pub(super) fn acts_on_list(&self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.search.as_ref().is_some_and(|s| match key.code {
            KeyCode::Char('a') => ctrl,
            KeyCode::Char('d' | 'D' | ' ' | 'l') | KeyCode::Enter | KeyCode::Right => {
                s.focus == Focus::Results
            }
            _ => false,
        })
    }

    /// Whether the key leaves the tree alone and the search may go on.
    pub(super) fn keeps_search_running(&self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.search.as_ref().is_some_and(|s| {
            s.focus == Focus::Results
                && match key.code {
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Char('j' | 'k' | ' ') => true,
                    KeyCode::Char('a') => ctrl,
                    _ => false,
                }
        })
    }

    /// Starts a search for the query. `reset` puts the cursor on the first
    /// result when the result comes.
    pub(super) fn start_search(&mut self, reset: bool) {
        self.freeze_search();
        let apparent = self.sort.apparent;
        let Some(s) = &mut self.search else {
            return;
        };
        s.rerun = false;
        s.reset |= reset;
        s.error = None;
        // A last word like `@ru` shows the list of ready-made searches.
        if search::presets_for(&s.query).is_some() {
            s.clear_hits();
            return;
        }
        let query = match Query::parse(&s.query) {
            Ok(q) if !q.is_empty() => q,
            Ok(_) => return s.clear_hits(),
            Err(e) => {
                s.error = Some(e);
                return s.clear_hits();
            }
        };
        let mut base = self.current_path().as_os_str().as_encoded_bytes().to_vec();
        if !base.ends_with(b"/") && !base.ends_with(b"\\") {
            base.push(b'/');
        }
        // The thread shares the root. It is the only other owner, and
        // `freeze_search` joins it before the tree changes.
        let root = Arc::clone(&self.tree.dir);
        let stack = self.stack.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, results) = mpsc::channel();
        let handle = thread::spawn({
            let cancel = Arc::clone(&cancel);
            move || {
                let outcome = search::run(dir_in(&root, &stack), &base, &query, apparent, &cancel);
                let _ = tx.send(outcome);
            }
        });
        if let Some(s) = &mut self.search {
            s.running = Some(Running {
                cancel,
                results,
                handle,
            });
        }
    }

    /// Takes a finished search and starts one that waits to run again.
    pub(super) fn poll_search(&mut self) {
        let done = match self.search.as_ref().and_then(|s| s.running.as_ref()) {
            Some(running) => running.results.try_recv(),
            None => Err(TryRecvError::Empty),
        };
        if !matches!(done, Err(TryRecvError::Empty)) {
            if let Some(running) = self.search.as_mut().and_then(|s| s.running.take()) {
                let _ = running.handle.join();
            }
            match done {
                Ok(Some(outcome)) => {
                    self.finish_search(outcome);
                }
                Ok(None) => self.rerun_search(),
                Err(_) => {
                    if let Some(s) = &mut self.search {
                        s.error = Some("The search stopped".into());
                    }
                }
            }
        }
        let confirming = matches!(self.mode, Mode::Confirm { .. });
        let rerun = self.search.as_mut().is_some_and(|s| {
            if !confirming && let Some(outcome) = s.waiting.take() {
                s.apply(outcome);
            }
            s.rerun && s.running.is_none()
        });
        if rerun {
            self.start_search(false);
        }
    }

    /// Shows the list, or keeps it back while a question is open. Returns
    /// true when the list changed on the screen.
    fn finish_search(&mut self, outcome: Outcome) -> bool {
        let confirming = matches!(self.mode, Mode::Confirm { .. });
        let Some(s) = &mut self.search else {
            return false;
        };
        if confirming {
            s.waiting = Some(outcome);
            false
        } else {
            s.apply(outcome);
            true
        }
    }

    /// Asks the search to run again after a change to the tree.
    pub(super) fn rerun_search(&mut self) {
        if let Some(s) = &mut self.search {
            s.rerun = true;
        }
    }

    /// Leaves search and shows the browser view from before.
    fn leave_search(&mut self) {
        self.freeze_search();
        let Some(search) = self.search.take() else {
            return;
        };
        self.clear_selection();
        self.apply_sort();
        let dir = self.dir();
        let len = dir.entries.len();
        self.cursor = search
            .saved
            .name
            .and_then(|name| dir.find(&name))
            .unwrap_or_else(|| search.saved.cursor.min(len.saturating_sub(1)));
        self.offset = search.saved.offset;
    }

    /// Leaves search and shows the folder of the result under the cursor,
    /// with the cursor on the item.
    fn open_result(&mut self) {
        self.freeze_search();
        let Some(hit) = self.search.as_ref().and_then(|s| s.hits.get(s.cursor)) else {
            return;
        };
        let (parent, name) = Search::split(&hit.rel);
        let (parent, name) = (parent.to_vec(), name.to_vec());
        let folder = self.result_folder(&parent);
        let stack = self
            .stack_for(&folder)
            .filter(|stack| self.tree.dir_at(stack).find(&name).is_some());
        let Some(stack) = stack else {
            self.drop_hit_at_cursor();
            self.message = Some("That item is gone from the list".into());
            return;
        };
        self.search = None;
        self.clear_selection();
        self.stack = stack;
        self.offset = 0;
        self.apply_sort();
        self.cursor = self.dir().find(&name).unwrap_or(0);
    }

    fn drop_hit_at_cursor(&mut self) {
        if let Some(s) = &mut self.search
            && s.cursor < s.hits.len()
        {
            s.remove_hits(&[s.cursor]);
        }
    }

    /// The folder on disk for a path below the search folder.
    fn result_folder(&self, parent: &[u8]) -> PathBuf {
        let mut folder = self.current_path();
        for part in parent.split(|&b| b == b'/').filter(|p| !p.is_empty()) {
            folder.push(os_name(part));
        }
        folder
    }

    pub(super) fn handle_search(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('c') && ctrl {
            return Action::Quit;
        }
        match self.search.as_ref().map(|s| s.focus) {
            Some(Focus::Query) => self.query_key(key, ctrl),
            Some(Focus::Results) => return self.results_key(key, ctrl),
            None => {}
        }
        Action::None
    }

    fn query_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(s) = &mut self.search else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.leave_search(),
            KeyCode::Enter | KeyCode::Down => s.focus = Focus::Results,
            KeyCode::Tab => {
                if let Some(text) = search::complete(&s.query) {
                    s.query = text;
                    self.start_search(true);
                }
            }
            KeyCode::Backspace => {
                if s.query.pop().is_some() {
                    self.start_search(true);
                }
            }
            KeyCode::Char('u') if ctrl => {
                s.query.clear();
                self.start_search(true);
            }
            // The keys of the example in the help: Ctrl+A, then D.
            KeyCode::Char('a') if ctrl => {
                if s.searching() {
                    self.message = Some("Wait for the search to finish".into());
                } else {
                    s.focus = Focus::Results;
                    s.hits.iter_mut().for_each(|h| h.selected = true);
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if s.query.chars().count() < search::MAX_QUERY {
                    s.query.push(c);
                    self.start_search(true);
                } else {
                    self.message = Some("The query is as long as it can be".into());
                }
            }
            _ => {}
        }
    }

    fn results_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        let page = self.list_height.saturating_sub(1).max(1) as isize;
        let Some(s) = &mut self.search else {
            return Action::None;
        };
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc if s.selected().next().is_some() => {
                s.hits.iter_mut().for_each(|h| h.selected = false);
            }
            KeyCode::Esc => self.leave_search(),
            KeyCode::Char('/') => s.focus = Focus::Query,
            KeyCode::Up | KeyCode::Char('k') => s.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => s.move_cursor(1),
            KeyCode::PageUp => s.move_cursor(-page),
            KeyCode::PageDown => s.move_cursor(page),
            KeyCode::Home => s.move_cursor(isize::MIN / 2),
            KeyCode::End => s.move_cursor(isize::MAX / 2),
            KeyCode::Char(' ') => {
                if let Some(h) = s.hits.get_mut(s.cursor) {
                    h.selected = !h.selected;
                    s.move_cursor(1);
                }
            }
            KeyCode::Char('a') if ctrl => s.hits.iter_mut().for_each(|h| h.selected = true),
            KeyCode::Char('a') => {
                self.sort.apparent = !self.sort.apparent;
                self.start_search(false);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_result(),
            KeyCode::Char('d') => return self.confirm_results(false),
            KeyCode::Char('D') => return self.confirm_results(true),
            KeyCode::Char('r') => {
                self.message = Some("Press Esc to leave search before a rescan".into());
            }
            KeyCode::Char('u') => {
                self.message = Some("Press Esc to leave search before an undo".into());
            }
            KeyCode::Char('?') => {
                s.help_page = 0;
                self.mode = Mode::Help;
            }
            _ => {}
        }
        Action::None
    }

    /// `d` and `D` for the selected results, or the result under the cursor.
    fn confirm_results(&mut self, permanent: bool) -> Action {
        let Some(s) = &self.search else {
            return Action::None;
        };
        let mut targets: Vec<usize> = s.selected().collect();
        if targets.is_empty() && s.cursor < s.hits.len() {
            targets.push(s.cursor);
        }
        if s.searching() {
            self.message = Some("Wait for the search to finish before a delete".into());
            return Action::None;
        }
        self.confirm_targets(permanent, targets)
    }

    /// Deletes results. One job goes to the queue for each folder that holds
    /// some of them. The rows leave the tree and the list at once.
    pub(super) fn enqueue_results(&mut self, permanent: bool, targets: &[usize]) {
        self.freeze_search();
        let Some(s) = &self.search else {
            return;
        };
        let mut targets = targets.to_vec();
        targets.sort_unstable();
        targets.dedup();
        targets.retain(|&i| i < s.hits.len());
        let mut groups: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
        for &i in &targets {
            let (parent, name) = Search::split(&s.hits[i].rel);
            groups
                .entry(parent.to_vec())
                .or_default()
                .push(name.to_vec());
        }
        // The jobs of one delete share a group, so the footer and the
        // message treat them as one.
        let group = Some(self.next_group);
        self.next_group += 1;
        let mut missing = 0;
        // The folders are found one by one: a job takes rows out of a
        // folder, which can move the folders below it in the tree.
        for (parent, names) in groups {
            let folder = self.result_folder(&parent);
            let Some(stack) = self.stack_for(&folder) else {
                missing += names.len();
                continue;
            };
            let dir = self.tree.dir_at(&stack);
            let wanted: HashSet<&[u8]> = names.iter().map(Vec::as_slice).collect();
            let rows: Vec<usize> = (0..dir.entries.len())
                .filter(|&i| wanted.contains(dir.name(&dir.entries[i])))
                .collect();
            missing += names.len().saturating_sub(rows.len());
            self.enqueue_at(folder, &stack, rows, permanent, group);
        }
        if missing > 0 {
            let results = if missing == 1 { "result" } else { "results" };
            self.message = Some(format!(
                "{missing} {results} not deleted: the folder or the item is gone from the list."
            ));
            self.message_is_error = true;
        }
        if let Some(s) = &mut self.search {
            s.remove_hits(&targets);
            // A list that was held back still has the rows that left now.
            s.waiting = None;
            s.rerun = true;
        }
    }
}

impl Search {
    /// Takes results out of the list at once. The totals follow.
    fn remove_hits(&mut self, targets: &[usize]) {
        for &i in targets.iter().rev() {
            let hit = self.hits.remove(i);
            self.matches = self.matches.saturating_sub(1);
            self.total = self.total.saturating_sub(hit.size);
        }
        self.cursor = self.cursor.min(self.hits.len().saturating_sub(1));
        // More matches than the list holds: results that were left out can
        // move up now.
        if (self.hits.len() as u64) < self.matches {
            self.rerun = true;
        }
    }
}
