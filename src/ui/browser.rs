use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

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

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent switch of the screen"
)]
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
    worker: Worker,
    /// The deletes that the worker has not finished or that wait for `done`
    /// to be applied, the oldest first.
    pending: VecDeque<Pending>,
    /// Results of the oldest pending jobs. A result that changes the tree
    /// waits here until the screen can take it.
    done: VecDeque<JobResult>,
    /// A quit waits for the pending deletes.
    quitting: bool,
    /// The message is an error from a delete. Keys that only move the cursor
    /// leave it on the screen.
    message_is_error: bool,
    /// The last delete that skipped the question, to ignore a key that is held
    /// down or bounces.
    last_quick_delete: Option<(bool, Instant)>,
    clock: Box<dyn Fn() -> Instant>,
}

/// The longest error text that finished deletes can add up to.
const MAX_ERROR_CHARS: usize = 600;

/// A second press of the same delete key within this time is ignored when
/// the question is off. It is longer than the usual delay before a held key
/// repeats (about 375 ms on macOS).
const REPEAT_GUARD: Duration = Duration::from_millis(600);

/// Frees `value` on a short-lived thread. A removed subtree can hold millions
/// of nodes, and dropping it on the UI thread would freeze the screen.
fn discard<T: Send + 'static>(value: T) {
    thread::spawn(move || drop(value));
}

/// How the event loop ended. A forced quit leaves the cache as it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Exit {
    Normal,
    Forced,
}

/// What a delete returns: the items that can be put back, and the first error.
type Deleted = (Vec<trash::Trashed>, Result<(), String>);

/// Moves or removes paths. Real code uses the Trash or `remove_all`, tests
/// pass a fake.
type Deleter = Box<dyn FnMut(&[PathBuf], bool) -> Deleted + Send>;

fn real_deleter() -> Deleter {
    Box::new(|paths, permanent| {
        if permanent {
            (Vec::new(), remove_all(paths))
        } else {
            trash::move_to_trash(paths)
        }
    })
}

/// A delete for the worker. It holds absolute paths, never tree indices,
/// because the tree changes while the job runs.
struct Job {
    paths: Vec<PathBuf>,
    /// Whether each path was a directory, so a survivor is scanned again.
    dirs: Vec<bool>,
    permanent: bool,
}

/// A taken row that is still on disk, with the fresh listing of a directory.
type Survivor = ((Vec<u8>, Entry), Option<Dir>);

enum Left {
    Gone,
    /// Still on disk. A directory comes with its fresh listing.
    Present(Option<Dir>),
}

struct JobResult {
    left: Vec<Left>,
    trashed: Vec<trash::Trashed>,
    result: Result<(), String>,
}

impl JobResult {
    /// Only a path that is still on disk changes the tree.
    fn changes_tree(&self) -> bool {
        self.left.iter().any(|l| matches!(l, Left::Present(_)))
    }
}

/// The UI side of a job. The taken entries leave the tree at once, and this
/// keeps them (in the order of `paths`) for the undo step or a put-back.
struct Pending {
    folder: PathBuf,
    paths: Vec<PathBuf>,
    taken: Vec<(Vec<u8>, Entry)>,
    permanent: bool,
    offset: usize,
}

/// One thread runs the jobs in order, so deletes keep their order and
/// Finder gets one request at a time.
struct Worker {
    jobs: Sender<Job>,
    results: Receiver<JobResult>,
}

impl Worker {
    fn spawn(opts: Options, mut deleter: Deleter) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (result_tx, results) = mpsc::channel();
        thread::spawn(move || {
            for job in job_rx {
                if result_tx.send(run_job(&job, &opts, &mut deleter)).is_err() {
                    break;
                }
            }
        });
        Worker { jobs, results }
    }
}

fn run_job(job: &Job, opts: &Options, deleter: &mut Deleter) -> JobResult {
    let (trashed, result) = deleter(&job.paths, job.permanent);
    let left = job
        .paths
        .iter()
        .zip(&job.dirs)
        .map(|(path, &is_dir)| {
            // The name may exist again, for example a new file, but the
            // deleter moved the old one.
            if trashed.iter().any(|t| t.original == *path) || path.symlink_metadata().is_err() {
                Left::Gone
            } else if is_dir {
                Left::Present(scan::scan(path, opts, &Progress::default()).ok())
            } else {
                Left::Present(None)
            }
        })
        .collect();
    JobResult {
        left,
        trashed,
        result,
    }
}

/// One move to the Trash. The removed entries keep their sizes and subtrees,
/// so undo puts them back in the tree without a rescan. `offset` is the
/// scroll position at the delete, so undo shows the same screen again.
struct UndoStep {
    items: Vec<trash::Trashed>,
    entries: Vec<(Vec<u8>, Entry)>,
    offset: usize,
}

enum Step {
    Continue,
    Rescan,
    Exit(Exit),
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
    let exit = result.as_ref().copied().unwrap_or(Exit::Normal);
    // Without this, the next run would load the stale tree again.
    if browser.saves_cache(exit)
        && let Some(session) = browser.session.take()
    {
        cache::save_in_background(&session, &browser.tree.dir);
    }
    // Freeing millions of nodes takes time and the process ends right after.
    std::mem::forget(browser);
    result.map(|_| ())
}

impl Browser {
    fn new(tree: Tree, opts: Options, source: &Source, session: Option<Header>) -> Self {
        Self::with_deleter(tree, opts, source, session, real_deleter())
    }

    fn with_deleter(
        tree: Tree,
        opts: Options,
        source: &Source,
        session: Option<Header>,
        deleter: Deleter,
    ) -> Self {
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
            worker: Worker::spawn(opts, deleter),
            pending: VecDeque::new(),
            done: VecDeque::new(),
            quitting: false,
            message_is_error: false,
            last_quick_delete: None,
            clock: Box::new(Instant::now),
        };
        browser.apply_sort();
        browser
    }

    /// Whether the tree may be saved. A forced quit can leave rows out of
    /// the tree that are still on disk, so it saves nothing.
    fn saves_cache(&self, exit: Exit) -> bool {
        exit == Exit::Normal && self.pending.is_empty() && self.changed
    }

    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// The state of the delete queue, shown while the user browses.
    pub fn status(&self) -> Option<String> {
        let first = self.pending.front()?;
        if self.quitting {
            let n = self.pending.len();
            let deletes = if n == 1 { "delete" } else { "deletes" };
            return Some(format!(
                "Finishing {n} {deletes} … press q again to quit now or Esc to stay"
            ));
        }
        if !self.done.is_empty() {
            return Some("Waiting to update the list …".into());
        }
        let items = count(first.paths.len());
        let mut text = if first.permanent {
            format!("Deleting {items} …")
        } else {
            format!("Moving {items} to the Trash …")
        };
        if self.pending.len() > 1 {
            text = format!("{text} {} more queued", self.pending.len() - 1);
        }
        Some(text)
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Exit> {
        loop {
            self.poll_jobs();
            if self.quitting && self.pending.is_empty() {
                return Ok(Exit::Normal);
            }
            terminal.draw(|frame| view::draw(frame, self))?;
            // Nothing changes by itself without a job, so wait for a key.
            if !self.pending.is_empty() && !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match self.step(key) {
                Step::Continue => {}
                Step::Rescan => self.rescan(terminal)?,
                Step::Exit(exit) => return Ok(exit),
            }
        }
    }

    /// Handles one key press. Everything but a rescan, which needs the
    /// terminal, happens here so tests can drive it.
    fn step(&mut self, key: KeyEvent) -> Step {
        if self.quitting {
            if quits(key) {
                return Step::Exit(Exit::Forced);
            }
            if key.code == KeyCode::Esc {
                self.quitting = false;
            }
            return Step::Continue;
        }
        match self.handle(key) {
            Action::None => {}
            Action::Quit if self.pending.is_empty() => return Step::Exit(Exit::Normal),
            Action::Quit => self.quitting = true,
            Action::Delete { permanent, targets } => self.enqueue(permanent, &targets),
            Action::Rescan => return Step::Rescan,
            Action::Undo => self.undo(),
        }
        Step::Continue
    }

    /// Takes the results that the worker has sent and applies those that the
    /// screen can take now, in the order of the deletes.
    fn poll_jobs(&mut self) {
        loop {
            match self.worker.results.try_recv() {
                Ok(result) => self.done.push_back(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.fail_unfinished();
                    break;
                }
            }
        }
        // Indices in `stack`, `cursor`, a question and a range must stay true,
        // so a put-back waits for a quiet screen. A quit has no screen left.
        let quiet = self.quitting || (matches!(self.mode, Mode::Browse) && self.range.is_none());
        while let Some(result) = self.done.front() {
            if result.changes_tree() && !quiet {
                break;
            }
            let result = self.done.pop_front().expect("front exists");
            let job = self.pending.pop_front().expect("a result has a job");
            self.apply(job, result);
        }
    }

    /// The worker thread ended, so no result will come. Puts every row back.
    fn fail_unfinished(&mut self) {
        while self.done.len() < self.pending.len() {
            let n = self.pending[self.done.len()].paths.len();
            self.done.push_back(JobResult {
                left: (0..n).map(|_| Left::Present(None)).collect(),
                trashed: Vec::new(),
                result: Err("the delete thread stopped".into()),
            });
        }
    }

    /// Takes the rows out of the tree at once and sends their paths to the
    /// worker. The entries stay in `pending` for the undo step or a put-back.
    fn enqueue(&mut self, permanent: bool, targets: &[usize]) {
        let folder = self.current_path();
        let mut targets = targets.to_vec();
        targets.sort_unstable();
        targets.dedup();
        targets.retain(|&i| i < self.dir().entries.len());
        if targets.is_empty() {
            return;
        }
        let dir = self.dir();
        let paths: Vec<PathBuf> = targets
            .iter()
            .map(|&i| folder.join(os_name(dir.name(&dir.entries[i]))))
            .collect();
        let dirs = targets
            .iter()
            .map(|&i| dir.entries[i].dir.is_some())
            .collect();
        let dir = self.tree.dir_at_mut(&self.stack);
        // From the end, so the indices that are left stay true.
        let mut taken: Vec<_> = targets.iter().rev().map(|&i| dir.take(i)).collect();
        taken.reverse();
        self.tree.refresh_totals(&self.stack);
        self.clear_selection();
        self.cursor = self.cursor.min(self.dir().entries.len().saturating_sub(1));
        self.resort();
        self.pending.push_back(Pending {
            folder,
            paths: paths.clone(),
            taken,
            permanent,
            offset: self.offset,
        });
        // A failed send means the worker stopped. `poll_jobs` finds out.
        let _ = self.worker.jobs.send(Job {
            paths,
            dirs,
            permanent,
        });
    }

    /// Settles one finished delete. The rows that are gone become the undo
    /// step. The rows that are still on disk come back.
    fn apply(&mut self, job: Pending, result: JobResult) {
        let total = job.paths.len();
        let mut removed = Vec::new();
        let mut back = Vec::new();
        for (entry, left) in job.taken.into_iter().zip(result.left) {
            match left {
                Left::Gone => removed.push(entry),
                Left::Present(fresh) => back.push((entry, fresh)),
            }
        }
        let gone = removed.len();
        if !back.is_empty() {
            self.restore_rows(&job.folder, back);
        }
        self.changed = true;
        let undoable = !result.trashed.is_empty();
        if undoable {
            self.undo.push(UndoStep {
                items: result.trashed,
                entries: removed,
                offset: job.offset,
            });
        } else {
            discard(removed);
        }
        let asks = *self.ask_mut(job.permanent);
        let text = delete_message(job.permanent, gone, total, undoable, asks, &result.result);
        self.show_delete_message(text, result.result.is_err());
    }

    /// Shows the end of a delete. An error stays when a later job ends
    /// before the user has seen it.
    fn show_delete_message(&mut self, text: String, is_error: bool) {
        let old = self.message.take().filter(|_| self.message_is_error);
        (self.message, self.message_is_error) = match (old, is_error) {
            (Some(old), _) => {
                let all = format!("{old} {text}");
                let extra = all.chars().count().saturating_sub(MAX_ERROR_CHARS);
                let all = if extra == 0 {
                    all
                } else {
                    format!("…{}", all.chars().skip(extra + 1).collect::<String>())
                };
                (Some(all), true)
            }
            (None, _) => (Some(text), is_error),
        };
    }

    /// Lists rows again that the delete did not remove, in `folder`. A
    /// directory gets its fresh listing. The screen stays where it is: the
    /// open folder and the cursor are found again by name.
    fn restore_rows(&mut self, folder: &Path, back: Vec<Survivor>) {
        let Some(target) = self.stack_for(folder) else {
            discard(back);
            return;
        };
        let here = self.current_path();
        let dir = self.tree.dir_at_mut(&target);
        let mut dropped = Vec::new();
        let mut old_dirs = Vec::new();
        for ((name, mut entry), fresh) in back {
            // A new item with the same name may exist now. Keep it.
            if dir.find(&name).is_some() {
                dropped.push((entry, fresh));
                continue;
            }
            entry.flags &= !flag::SELECTED;
            dir.insert(&name, entry);
            if let Some(sub) = fresh {
                let i = dir.entries.len() - 1;
                let e = &mut dir.entries[i];
                e.flags &= !(flag::SUB_ERROR | flag::ERROR);
                (e.disk, e.apparent) = (0, 0);
                old_dirs.extend(e.dir.take());
                dir.attach(i, sub, 0);
            }
        }
        discard((dropped, old_dirs));
        self.tree.refresh_totals(&target);
        if target == self.stack {
            self.resort();
        }
        self.stack = self.stack_for(&here).unwrap_or_default();
        let len = self.dir().entries.len();
        self.cursor = self.cursor.min(len.saturating_sub(1));
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
        let moves = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Char('j' | 'k')
        );
        if !(self.message_is_error && moves && matches!(self.mode, Mode::Browse)) {
            self.message = None;
            self.message_is_error = false;
        }
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
            KeyCode::Char('r' | 'u') if !self.pending.is_empty() => {
                let what = if key.code == KeyCode::Char('r') {
                    "a rescan"
                } else {
                    "an undo"
                };
                self.message = Some(format!("Wait for the delete to finish before {what}"));
            }
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
            let now = (self.clock)();
            let repeat = self.last_quick_delete.is_some_and(|(was, at)| {
                was == permanent && now.saturating_duration_since(at) < REPEAT_GUARD
            });
            self.last_quick_delete = Some((permanent, now));
            if repeat {
                let key = if permanent { 'D' } else { 'd' };
                self.message = Some(format!(
                    "Ignored a quick repeat of {key}. Pause a moment to delete the next row."
                ));
                Action::None
            } else {
                Action::Delete { permanent, targets }
            }
        }
    }

    fn ask_mut(&mut self, permanent: bool) -> &mut bool {
        if permanent {
            &mut self.ask_delete
        } else {
            &mut self.ask_trash
        }
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
        let mut dropped = Vec::new();
        for (name, mut entry) in entries {
            if present.contains(&name) || folder.join(os_name(&name)).symlink_metadata().is_err() {
                dropped.push(entry);
                continue;
            }
            entry.flags &= !flag::SELECTED;
            restored.insert(dir.insert(&name, entry));
        }
        discard(dropped);
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

/// The status line after a delete. The hint about the help screen comes
/// last and only without an error, so an error text keeps its place.
fn delete_message(
    permanent: bool,
    gone: usize,
    total: usize,
    undoable: bool,
    asks: bool,
    result: &Result<(), String>,
) -> String {
    let done = if permanent {
        "Deleted"
    } else {
        "Moved to the Trash:"
    };
    let undo = if undoable { " Press u to undo." } else { "" };
    match result {
        Ok(()) => {
            let again = if asks {
                ""
            } else {
                " Press ? to ask first again."
            };
            format!("{done} {}.{undo}{again}", count(gone))
        }
        Err(e) => format!("{done} {gone} of {}.{undo} Error: {e}", count(total)),
    }
}

fn quits(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && (key.code == KeyCode::Char('q')
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)))
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
        // A path that is gone already is what the user wants.
        if let Err(e) = result
            && e.kind() != io::ErrorKind::NotFound
        {
            first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

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

    /// True when the action deletes item 0 the given way.
    fn deletes_first(action: &Action, permanent: bool) -> bool {
        matches!(action, Action::Delete { permanent: p, targets } if *p == permanent && *targets == [0])
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
            assert!(deletes_first(&browser.handle(key('A')), permanent));
            // Only the question that was answered stops.
            assert_eq!(
                (browser.ask_trash, browser.ask_delete),
                (permanent, !permanent)
            );
            // The same key now deletes at once, and the other key still asks.
            assert!(deletes_first(&browser.handle(key(same)), permanent));
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

    #[test]
    fn the_help_screen_turns_on_only_the_question_it_names() {
        let mut browser = browser_with(in_disk_order());
        (browser.ask_trash, browser.ask_delete) = (false, false);
        browser.mode = Mode::Help;
        browser.handle(key('p'));
        assert!(!browser.ask_trash && browser.ask_delete);
        browser.handle(key('t'));
        assert!(browser.ask_trash && browser.ask_delete);
        (browser.ask_trash, browser.ask_delete) = (false, false);
        browser.handle(key('t'));
        assert!(browser.ask_trash && !browser.ask_delete);
        assert!(matches!(browser.mode, Mode::Help));
    }

    #[test]
    fn d_in_an_empty_folder_does_nothing_even_when_the_question_is_off() {
        let mut browser = browser_with(Dir::default());
        browser.stack = vec![0];
        (browser.ask_trash, browser.ask_delete) = (false, false);
        assert!(matches!(browser.handle(key('d')), Action::None));
        assert!(matches!(browser.handle(key('D')), Action::None));
        assert!(matches!(browser.mode, Mode::Browse));
    }

    /// The text of an 80x24 terminal, the smallest common size.
    fn screen(browser: &mut Browser) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
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

    /// The row below the first row that contains `text`.
    fn row_below<'a>(shown: &'a str, text: &str) -> &'a str {
        let mut rows = shown.lines().skip_while(|row| !row.contains(text));
        rows.next();
        rows.next().unwrap_or("")
    }

    #[test]
    fn the_help_screen_shows_a_question_that_is_off_under_its_key() {
        let mut browser = browser_with(in_disk_order());
        browser.mode = Mode::Help;
        let shown = screen(&mut browser);
        assert!(shown.contains("Press any key to close"));
        assert!(!shown.contains("No question"));
        (browser.ask_trash, browser.ask_delete) = (false, false);
        let shown = screen(&mut browser);
        let trash = row_below(&shown, "Move to the Trash (selection or cursor)");
        let delete = row_below(&shown, "Delete permanently");
        assert!(trash.contains("No question until quit. Press t to ask again."));
        assert!(delete.contains("No question until quit. Press p to ask again."));
        assert!(shown.contains("Press any other key to close"));
        browser.ask_trash = true;
        let shown = screen(&mut browser);
        assert!(!shown.contains("Press t to ask again"));
        assert!(shown.contains("Press p to ask again"));
    }

    #[test]
    fn the_question_shows_its_keys_and_whole_long_names() {
        let mut folder = Dir::default();
        for i in 0..8 {
            folder.push(&[b'f', b'0' + i], Kind::File, 4096, 4096, 0);
        }
        let spaced = format!("{} {} {}", "w".repeat(35), "v".repeat(35), "u".repeat(35));
        folder.push(spaced.as_bytes(), Kind::File, 4096, 4096, 0);
        folder.push(&[b'x'; 70], Kind::File, 4096, 4096, 0);
        let mut browser = browser_with(folder);
        browser.stack = vec![0];
        for (permanent, targets) in [(true, (0..8).collect()), (false, vec![8, 9])] {
            browser.mode = Mode::Confirm { permanent, targets };
            let shown = screen(&mut browser);
            assert!(shown.contains("Shift+A: yes, and stop asking until you quit"));
            assert!(shown.contains("Any other key: no"));
            if permanent {
                assert!(shown.contains("and 2 more"));
            } else {
                // The popup is 58 columns wide inside, and the names start
                // after 3 spaces.
                let rows = [
                    format!("│   {}│", "x".repeat(55)),
                    format!("│{}{}│", "x".repeat(15), " ".repeat(43)),
                    format!("│   {} {}│", "w".repeat(35), "v".repeat(19)),
                    format!("│{} {}{}│", "v".repeat(16), "u".repeat(35), " ".repeat(6)),
                ];
                for row in rows {
                    assert!(shown.contains(&row), "missing popup row {row}");
                }
            }
        }
    }

    #[test]
    fn the_status_line_names_the_help_only_after_a_delete_without_errors() {
        assert_eq!(
            delete_message(true, 2, 2, false, false, &Ok(())),
            "Deleted 2 items. Press ? to ask first again."
        );
        assert_eq!(
            delete_message(false, 1, 1, true, true, &Ok(())),
            "Moved to the Trash: 1 item. Press u to undo."
        );
        assert_eq!(
            delete_message(true, 1, 2, false, false, &Err("busy".into())),
            "Deleted 1 of 2 items. Error: busy"
        );
    }

    #[test]
    fn only_q_and_ctrl_c_quit_while_a_delete_finishes() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(quits(key('q')) && quits(ctrl_c));
        assert!(!quits(key('D')) && !quits(key('c')) && !quits(key('A')));
    }

    #[test]
    fn the_status_line_suggests_an_elevated_run_only_when_it_helps() {
        assert_eq!(
            describe(&Source::ScannedNotElevated).as_deref(),
            Some(cache::ELEVATE_HINT)
        );
        assert_eq!(describe(&Source::Scanned), None);
    }

    type Seen = Arc<Mutex<Vec<Vec<PathBuf>>>>;

    /// A deleter that waits for one `()` per job, records its paths, and
    /// lets `act` do the work on each path. It never touches the Trash.
    fn gated(
        gate: mpsc::Receiver<()>,
        seen: &Seen,
        act: impl Fn(&Path) -> Result<(), String> + Send + 'static,
    ) -> Deleter {
        let seen = Arc::clone(seen);
        Box::new(move |paths, permanent| {
            let _ = gate.recv();
            seen.lock().unwrap().push(paths.to_vec());
            let mut first = None;
            let mut trashed = Vec::new();
            for path in paths {
                match act(path) {
                    Ok(()) if !permanent && path.symlink_metadata().is_err() => {
                        trashed.push(trash::Trashed::fake(path.clone()));
                    }
                    Ok(()) => {}
                    Err(e) => {
                        first.get_or_insert(e);
                    }
                }
            }
            (trashed, first.map_or(Ok(()), Err))
        })
    }

    fn remove(path: &Path) -> Result<(), String> {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.or_else(|e| match e.kind() {
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(e.to_string()),
        })
    }

    fn file_name(path: &Path) -> &str {
        path.file_name().and_then(|n| n.to_str()).unwrap()
    }

    /// A browser on real files, sorted by name, with a gated deleter. Each
    /// `()` on the returned sender lets one job run.
    fn gated_browser(
        root: &Path,
        files: &[&str],
        act: impl Fn(&Path) -> Result<(), String> + Send + 'static,
    ) -> (Browser, mpsc::Sender<()>, Seen) {
        for (n, file) in files.iter().enumerate() {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![0u8; 1000 * (n + 1)]).unwrap();
        }
        let dir = scan::scan(root, &opts(), &Progress::default()).unwrap();
        let tree = Tree {
            path: root.to_path_buf(),
            dir: Box::new(dir),
        };
        let (gate, rx) = mpsc::channel();
        let seen = Seen::default();
        let deleter = gated(rx, &seen, act);
        let mut browser = Browser::with_deleter(tree, opts(), &Source::Scanned, None, deleter);
        browser.set_sort(SortKey::Name);
        (browser, gate, seen)
    }

    fn go_to(browser: &mut Browser, name: &str) {
        point_at(browser, name);
        browser.open();
    }

    fn point_at(browser: &mut Browser, name: &str) {
        browser.cursor = browser.dir().find(name.as_bytes()).unwrap();
    }

    fn press(browser: &mut Browser, keys: &str) {
        for c in keys.chars() {
            assert!(matches!(browser.step(key(c)), Step::Continue));
        }
    }

    fn names(browser: &Browser) -> Vec<String> {
        let dir = browser.dir();
        dir.entries
            .iter()
            .map(|e| String::from_utf8_lossy(dir.name(e)).into_owned())
            .collect()
    }

    fn wait(browser: &mut Browser, until: impl Fn(&Browser) -> bool) {
        let start = std::time::Instant::now();
        loop {
            browser.poll_jobs();
            if until(browser) {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn keys_work_while_a_delete_is_blocked_and_the_rows_are_already_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2", "A/a3", "top"], remove);
        go_to(&mut b, "A");
        let before = b.dir().totals();
        point_at(&mut b, "a2");
        press(&mut b, "dy");
        assert_eq!(names(&b), ["a1", "a3"]);
        assert!(b.dir().totals().items < before.items);
        assert!(b.dir().totals().disk < before.disk);
        assert_eq!(b.status().as_deref(), Some("Moving 1 item to the Trash …"));
        press(&mut b, "jk");
        assert_eq!(name_at_cursor(&b), b"a1");
        assert!(b.message.is_none());
        assert!(b.status().is_some(), "the queue state outlives key presses");
        assert!(tmp.path().join("A/a2").exists(), "the job has not run");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert!(!tmp.path().join("A/a2").exists());
        assert_eq!(
            b.message.as_deref(),
            Some("Moved to the Trash: 1 item. Press u to undo.")
        );
        assert_eq!(b.undo.len(), 1);
        assert!(b.status().is_none());
    }

    #[test]
    fn three_deletes_in_two_folders_keep_the_view_and_the_undo_order() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = |p: &Path| match file_name(p) {
            "b2" => Err("busy".to_string()),
            _ => remove(p),
        };
        let files = ["A/a1", "A/a2", "B/b1", "B/b2", "B/b3"];
        let (mut b, gate, _) = gated_browser(tmp.path(), &files, busy);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        press(&mut b, "h");
        go_to(&mut b, "B");
        press(&mut b, "s");
        point_at(&mut b, "b3");
        press(&mut b, "dy");
        point_at(&mut b, "b2");
        press(&mut b, "dy");
        point_at(&mut b, "b1");
        let (stack, here) = (b.stack.clone(), name_at_cursor(&b).to_vec());
        assert_eq!(
            b.status().unwrap(),
            "Moving 1 item to the Trash … 2 more queued"
        );
        for _ in 0..3 {
            gate.send(()).unwrap();
        }
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(
            (b.stack.clone(), name_at_cursor(&b).to_vec()),
            (stack, here)
        );
        assert_eq!(names(&b).len(), 2, "b2 came back");
        assert_eq!(b.undo.len(), 2);
        assert!(b.undo[0].items[0].original.ends_with("A/a1"));
        assert!(b.undo[1].items[0].original.ends_with("B/b3"));
    }

    #[test]
    fn a_put_back_waits_while_the_question_is_open_and_does_not_change_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = |p: &Path| match file_name(p) {
            "a1" => Err("busy".to_string()),
            _ => remove(p),
        };
        let (mut b, gate, seen) = gated_browser(tmp.path(), &["A/a1", "A/a2", "A/a3"], busy);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        point_at(&mut b, "a2");
        press(&mut b, "d");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.done.len() == 1);
        // The result stays put, so the indices in the question stay true.
        assert!(matches!(&b.mode, Mode::Confirm { targets, .. } if targets == &[0]));
        assert_eq!(names(&b), ["a2", "a3"]);
        assert_eq!(b.pending.len(), 1);
        press(&mut b, "y");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        let jobs = seen.lock().unwrap().clone();
        assert_eq!(jobs[1], [tmp.path().join("A/a2")]);
        assert_eq!(names(&b), ["a1", "a3"]);
        assert!(tmp.path().join("A/a1").exists());
    }

    #[test]
    fn a_put_back_waits_while_a_range_is_active() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = |p: &Path| match file_name(p) {
            "a1" => Err("busy".to_string()),
            _ => remove(p),
        };
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2", "A/a3"], busy);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        press(&mut b, "J");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.done.len() == 1);
        assert_eq!(names(&b), ["a2", "a3"]);
        press(&mut b, "j");
        b.poll_jobs();
        assert_eq!(names(&b), ["a1", "a2", "a3"]);
    }

    #[test]
    fn a_put_back_into_an_ancestor_keeps_the_open_folder_and_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = |p: &Path| match file_name(p) {
            "a1" => Err("busy".to_string()),
            _ => remove(p),
        };
        // By name, a1 sorts before sub, so a sort of the parent after the
        // put-back would move sub.
        let files = ["A/sub/s1", "A/sub/s2", "A/sub/s3", "A/a1"];
        let (mut b, gate, _) = gated_browser(tmp.path(), &files, busy);
        go_to(&mut b, "A");
        assert_eq!(names(&b), ["a1", "sub"]);
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        go_to(&mut b, "sub");
        press(&mut b, "j");
        let (path, here) = (b.current_path(), name_at_cursor(&b).to_vec());
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(b.current_path(), path);
        assert_eq!(name_at_cursor(&b), here);
        assert_eq!(b.stack_for(&path), Some(b.stack.clone()));
        assert!(b.dir().find(b"a1").is_none(), "a1 belongs to the parent");
        press(&mut b, "h");
        assert_eq!(names(&b), ["a1", "sub"]);
        assert_eq!(name_at_cursor(&b), b"sub");
    }

    #[test]
    fn a_partly_failed_delete_undoes_what_is_gone_and_rescans_what_is_left() {
        let tmp = tempfile::tempdir().unwrap();
        let act = |p: &Path| match file_name(p) {
            "sub" => {
                std::fs::remove_file(p.join("s1")).unwrap();
                Err("busy".to_string())
            }
            _ => remove(p),
        };
        let files = ["A/a1", "A/sub/s1", "A/sub/s2"];
        let (mut b, gate, _) = gated_browser(tmp.path(), &files, act);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "  dy");
        assert_eq!(b.dir().entries.len(), 0);
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(names(&b), ["sub"]);
        let sub = b.dir().entries[0].dir.as_ref().unwrap();
        assert_eq!((sub.find(b"s1"), sub.find(b"s2").is_some()), (None, true));
        assert_eq!(b.dir().entries[0].items, 2, "the folder and s2");
        assert_eq!(b.undo.len(), 1);
        assert_eq!(b.undo[0].entries.len(), 1);
        assert_eq!(b.undo[0].entries[0].0, b"a1");
        assert_eq!(
            b.message.as_deref(),
            Some("Moved to the Trash: 1 of 2 items. Press u to undo. Error: busy")
        );
    }

    #[test]
    fn rescan_and_undo_wait_for_pending_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2"], remove);
        go_to(&mut b, "A");
        press(&mut b, "dy");
        press(&mut b, "r");
        assert_eq!(
            b.message.as_deref(),
            Some("Wait for the delete to finish before a rescan")
        );
        press(&mut b, "u");
        assert_eq!(
            b.message.as_deref(),
            Some("Wait for the delete to finish before an undo")
        );
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert!(matches!(b.step(key('r')), Step::Rescan));
    }

    #[test]
    fn a_normal_quit_waits_and_saves_and_a_forced_quit_saves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2"], remove);
        assert!(matches!(b.step(key('q')), Step::Exit(Exit::Normal)));
        go_to(&mut b, "A");
        press(&mut b, "dy");
        press(&mut b, "q");
        assert!(b.quitting());
        assert_eq!(
            b.status().as_deref(),
            Some("Finishing 1 delete … press q again to quit now or Esc to stay")
        );
        assert!(!b.saves_cache(Exit::Normal), "the delete has not finished");
        assert!(!b.saves_cache(Exit::Forced));
        press(&mut b, "j");
        assert!(matches!(b.step(key('q')), Step::Exit(Exit::Forced)));
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert!(b.saves_cache(Exit::Normal));
        assert!(!b.saves_cache(Exit::Forced));
    }

    #[test]
    fn a_delete_of_a_parent_counts_a_pending_child_that_is_already_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let act = |p: &Path| match file_name(p) {
            // The child job removes the whole folder, as a parent delete would.
            "a1" => remove(p.parent().unwrap()),
            _ => remove(p),
        };
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2", "top"], act);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        press(&mut b, "h");
        point_at(&mut b, "A");
        press(&mut b, "dy");
        gate.send(()).unwrap();
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(names(&b), ["top"]);
        assert!(!tmp.path().join("A").exists());
        assert_eq!(
            b.message.as_deref(),
            Some("Moved to the Trash: 1 item. Press u to undo.")
        );
        assert_eq!(b.undo.len(), 2);
    }

    #[test]
    fn remove_all_counts_a_missing_path_as_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let paths = [tmp.path().join("missing"), file.clone()];
        assert_eq!(remove_all(&paths), Ok(()));
        assert!(!file.exists());
    }

    #[test]
    fn a_path_that_the_deleter_trashed_is_gone_even_when_the_name_exists_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (kept, other) = (tmp.path().join("kept"), tmp.path().join("other"));
        std::fs::write(&kept, b"new").unwrap();
        std::fs::write(&other, b"x").unwrap();
        let job = Job {
            paths: vec![kept.clone(), other],
            dirs: vec![false, false],
            permanent: false,
        };
        let mut deleter: Deleter =
            Box::new(move |_, _| (vec![trash::Trashed::fake(kept.clone())], Ok(())));
        let result = run_job(&job, &opts(), &mut deleter);
        assert!(matches!(result.left[0], Left::Gone));
        assert!(matches!(result.left[1], Left::Present(None)));
        assert!(result.changes_tree());
    }

    #[test]
    fn a_held_key_cannot_delete_several_rows_without_the_question() {
        let tmp = tempfile::tempdir().unwrap();
        let files = ["A/a1", "A/a2", "A/a3", "A/a4"];
        let (mut b, gate, seen) = gated_browser(tmp.path(), &files, remove);
        let base = Instant::now();
        let at = Arc::new(Mutex::new(base));
        let clock = Arc::clone(&at);
        b.clock = Box::new(move || *clock.lock().unwrap());
        (b.ask_trash, b.ask_delete) = (false, false);
        go_to(&mut b, "A");
        let press_at = |b: &mut Browser, ms: u64, keys: &str| {
            *at.lock().unwrap() = base + Duration::from_millis(ms);
            press(b, keys);
        };
        press_at(&mut b, 0, "d");
        assert_eq!(names(&b), ["a2", "a3", "a4"]);
        // The first auto-repeat of a held key on macOS comes after about 375 ms.
        press_at(&mut b, 400, "d");
        assert_eq!(names(&b), ["a2", "a3", "a4"], "a repeat is ignored");
        assert_eq!(
            b.message.as_deref(),
            Some("Ignored a quick repeat of d. Pause a moment to delete the next row.")
        );
        // The ignored press at 400 moved the stamp, so 800 is still too soon.
        press_at(&mut b, 800, "d");
        assert_eq!(names(&b), ["a2", "a3", "a4"]);
        // D is another key.
        press_at(&mut b, 850, "D");
        assert_eq!(names(&b), ["a3", "a4"]);
        press_at(&mut b, 900, "D");
        assert!(b.message.as_deref().unwrap().contains("repeat of D"));
        press_at(&mut b, 1500, "d");
        assert_eq!(names(&b), ["a4"]);
        for _ in 0..3 {
            gate.send(()).unwrap();
        }
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_added_error_text_has_a_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, _gate, _) = gated_browser(tmp.path(), &["A/a1"], remove);
        for _ in 0..50 {
            b.show_delete_message("e".repeat(100), true);
        }
        let len = b.message.as_ref().unwrap().chars().count();
        assert!(len <= MAX_ERROR_CHARS);
    }

    #[test]
    fn the_footer_shows_the_message_and_the_queue_state_together() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2"], remove);
        go_to(&mut b, "A");
        press(&mut b, "dy");
        press(&mut b, "r");
        let shown = screen(&mut b);
        let footer = shown.lines().last().unwrap();
        assert!(footer.contains("Wait for the delete to finish before a rescan"));
        assert!(footer.contains("Moving 1 item to the Trash …"));
        b.message = Some("x".repeat(100));
        let shown = screen(&mut b);
        let footer = shown.lines().last().unwrap();
        assert!(footer.contains('…') && footer.contains("Moving 1 item"));
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
    }

    #[test]
    fn a_delete_error_survives_cursor_keys_and_other_keys_clear_it() {
        let tmp = tempfile::tempdir().unwrap();
        let busy = |p: &Path| match file_name(p) {
            "a1" => Err("busy".to_string()),
            _ => remove(p),
        };
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2", "A/a3"], busy);
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        point_at(&mut b, "a2");
        press(&mut b, "dy");
        gate.send(()).unwrap();
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        let message = b.message.clone().unwrap();
        assert!(message.contains("Error: busy"));
        assert!(message.contains("Moved to the Trash: 1 item"), "both texts");
        press(&mut b, "jk");
        assert_eq!(b.message.as_ref(), Some(&message));
        press(&mut b, "s");
        assert!(b.message.is_none());
    }

    #[test]
    fn a_finished_result_that_waits_says_so_instead_of_moving() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2", "A/a3"], |p| {
            match file_name(p) {
                "a1" => Err("busy".to_string()),
                _ => remove(p),
            }
        });
        go_to(&mut b, "A");
        point_at(&mut b, "a1");
        press(&mut b, "dy");
        press(&mut b, "d");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.done.len() == 1);
        assert_eq!(b.status().as_deref(), Some("Waiting to update the list …"));
        press(&mut b, "n");
        wait(&mut b, |b| b.pending.is_empty());
        assert!(b.status().is_none());
    }

    #[test]
    fn esc_cancels_a_quit_that_waits() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2"], remove);
        go_to(&mut b, "A");
        press(&mut b, "dy");
        press(&mut b, "q");
        assert!(b.quitting());
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(b.step(esc), Step::Continue));
        assert!(!b.quitting());
        assert_eq!(b.status().as_deref(), Some("Moving 1 item to the Trash …"));
        press(&mut b, "j");
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
    }

    #[test]
    fn a_stopped_worker_puts_every_row_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut b, gate, _) = gated_browser(tmp.path(), &["A/a1", "A/a2"], |_| {
            panic!("the deleter failed on purpose")
        });
        go_to(&mut b, "A");
        press(&mut b, "dy");
        assert_eq!(names(&b), ["a2"]);
        gate.send(()).unwrap();
        wait(&mut b, |b| b.pending.is_empty());
        assert_eq!(names(&b), ["a1", "a2"]);
        assert!(
            b.message
                .as_deref()
                .unwrap()
                .contains("the delete thread stopped")
        );
        assert!(b.undo.is_empty());
    }

    #[test]
    fn a_put_back_keeps_a_new_item_with_the_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut b = browser_on_disk(tmp.path());
        let i = b.dir().find(b"middle").unwrap();
        let taken = b.tree.dir_at_mut(&[]).take(i);
        b.tree.dir_at_mut(&[]).push(b"middle", Kind::File, 7, 7, 0);
        b.restore_rows(tmp.path(), vec![(taken, None)]);
        let dir = b.dir();
        assert_eq!(dir.entries.len(), 3);
        let kept = &dir.entries[dir.find(b"middle").unwrap()];
        assert_eq!(kept.disk, 7, "the new item stays");
    }
}
