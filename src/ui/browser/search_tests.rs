use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::tests::{browser_with, gated_browser, key, remove, screen};
use super::*;
use crate::search::MAX_HITS;
use crate::tree::Kind;

fn send(b: &mut Browser, code: KeyCode, modifiers: KeyModifiers) {
    assert!(matches!(
        b.step(KeyEvent::new(code, modifiers)),
        Step::Continue
    ));
}

fn code(b: &mut Browser, code: KeyCode) {
    send(b, code, KeyModifiers::NONE);
}

fn ctrl_a(b: &mut Browser) {
    send(b, KeyCode::Char('a'), KeyModifiers::CONTROL);
}

fn typed(b: &mut Browser, text: &str) {
    for c in text.chars() {
        assert!(matches!(b.step(key(c)), Step::Continue));
    }
}

fn settle(b: &mut Browser, until: impl Fn(&Browser) -> bool) {
    let start = Instant::now();
    loop {
        b.poll_jobs();
        b.poll_search();
        if until(b) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn idle(b: &Browser) -> bool {
    !b.search.as_ref().is_some_and(Search::searching)
}

/// Opens the search (or clears the open query), types the query and waits
/// for the result.
fn search_for(b: &mut Browser, query: &str) {
    if b.search.is_none() {
        typed(b, "/");
    } else {
        send(b, KeyCode::Char('u'), KeyModifiers::CONTROL);
    }
    typed(b, query);
    settle(b, idle);
}

fn s(b: &Browser) -> &Search {
    b.search.as_ref().expect("a search is open")
}

fn rels(b: &Browser) -> Vec<String> {
    s(b).hits
        .iter()
        .map(|h| String::from_utf8_lossy(&h.rel).into_owned())
        .collect()
}

fn selected_in_tree(dir: &Dir) -> usize {
    dir.entries
        .iter()
        .map(|e| usize::from(e.has(flag::SELECTED)) + e.dir.as_deref().map_or(0, selected_in_tree))
        .sum()
}

const FILES: [&str; 7] = [
    "A/package.json",
    "A/node_modules/x",
    "A/src/main.rs",
    "B/package.json",
    "B/node_modules/y",
    "B/keep/z",
    "C/readme",
];

#[test]
fn slash_opens_a_search_line_and_esc_restores_the_view_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    b.set_sort(SortKey::Size);
    b.cursor = 2;
    b.offset = 1;
    b.tree.dir_at_mut(&[]).entries[0].flags |= flag::SELECTED;
    let stack = b.stack.clone();
    let name = b.dir().name(&b.dir().entries[2]).to_vec();

    typed(&mut b, "/");
    assert_eq!(s(&b).focus, Focus::Query);
    assert_eq!(
        selected_in_tree(&b.tree.dir),
        0,
        "entering clears the flags"
    );
    search_for(&mut b, "node");
    code(&mut b, KeyCode::Esc);

    assert!(b.search.is_none());
    assert_eq!(b.stack, stack);
    assert_eq!((b.cursor, b.offset), (2, 1));
    assert_eq!(b.dir().name(&b.dir().entries[b.cursor]), name.as_slice());
    assert_eq!(selected_in_tree(&b.tree.dir), 0);
}

#[test]
fn esc_in_the_results_clears_a_selection_first_and_then_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    typed(&mut b, " ");
    assert_eq!(s(&b).selected().count(), 1);
    code(&mut b, KeyCode::Esc);
    assert_eq!(s(&b).selected().count(), 0);
    assert_eq!(s(&b).focus, Focus::Results);
    code(&mut b, KeyCode::Esc);
    assert!(b.search.is_none());
}

#[test]
fn slash_in_the_results_edits_the_query_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Down);
    assert_eq!(s(&b).focus, Focus::Results);
    typed(&mut b, "/");
    assert_eq!(s(&b).focus, Focus::Query);
    typed(&mut b, "x");
    settle(&mut b, idle);
    assert!(s(&b).error.is_some(), "@nodex is no ready-made search");
    assert_eq!(s(&b).hits.len(), 0);
}

#[test]
fn results_show_the_path_below_the_open_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        "P/app/package.json",
        "P/app/node_modules/a",
        "P/app/src/b",
        "P/other/package.json",
        "P/other/node_modules/c",
    ];
    let (mut b, _gate, _) = gated_browser(tmp.path(), &files, remove);
    super::tests::go_to(&mut b, "P");
    search_for(&mut b, "@node");
    assert_eq!(rels(&b), ["other/node_modules", "app/node_modules"]);
    assert!(s(&b).hits.iter().all(|h| h.is_dir));
    let shown = screen(&mut b);
    assert!(shown.contains("other/node_modules/"), "{shown}");
    assert!(shown.contains("app/node_modules/"));
    assert!(shown.contains("Search in "));
    assert!(shown.contains(": @node (2 matches, "));
    let footer = shown.lines().last().unwrap();
    assert!(footer.starts_with(" Search: @node_"), "{footer}");
}

#[test]
fn the_footer_of_the_results_shows_the_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    let shown = screen(&mut b);
    let footer = shown.lines().last().unwrap();
    assert!(footer.starts_with(" 2 matches: "), "{footer}");
    ctrl_a(&mut b);
    let shown = screen(&mut b);
    assert!(shown.lines().last().unwrap().contains("2 selected: "));
}

#[test]
fn a_long_path_is_cut_from_the_left() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = "d/".repeat(60);
    let files = [
        format!("{deep}package.json"),
        format!("{deep}node_modules/x"),
    ];
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &files, remove);
    search_for(&mut b, "@node");
    let shown = screen(&mut b);
    let row = shown.lines().find(|l| l.contains("node_modules/")).unwrap();
    assert!(row.contains('…'), "{row}");
    assert!(row.trim_end().ends_with("node_modules/"), "{row}");
}

#[test]
fn more_matches_than_the_limit_are_counted_and_said() {
    let mut folder = Dir::default();
    for i in 0..MAX_HITS + 25 {
        folder.push(format!("f{i}").as_bytes(), Kind::File, 1 + i as u64, 1, 0);
    }
    let mut b = browser_with(folder);
    typed(&mut b, "l");
    search_for(&mut b, "f");
    assert_eq!(s(&b).hits.len(), MAX_HITS);
    assert_eq!(s(&b).matches, MAX_HITS as u64 + 25);
    let shown = screen(&mut b);
    assert!(
        shown.contains(&format!("largest {MAX_HITS} of {} matches", MAX_HITS + 25)),
        "{shown}"
    );
}

#[test]
fn a_bad_word_shows_an_error_in_the_footer_and_no_results() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    for bad in ["@nope", "size>x", "type:link"] {
        search_for(&mut b, bad);
        assert_eq!(s(&b).hits.len(), 0);
        let shown = screen(&mut b);
        let footer = shown.lines().last().unwrap();
        assert!(footer.starts_with(" Search: "), "{footer}");
        assert!(footer.contains("  |  "), "{footer}");
    }
}

#[test]
fn an_at_sign_lists_the_ready_made_searches_and_tab_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@ru");
    let shown = screen(&mut b);
    assert!(shown.contains("@rust"), "{shown}");
    assert!(!shown.contains("@node"));
    code(&mut b, KeyCode::Tab);
    assert_eq!(s(&b).query, "@rust ");
    search_for(&mut b, "@");
    let shown = screen(&mut b);
    for p in crate::search::PRESETS {
        assert!(shown.contains(&format!("@{}", p.name)));
    }
}

#[test]
fn enter_on_a_result_opens_its_folder_with_the_cursor_on_the_item() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    code(&mut b, KeyCode::Down);
    assert_eq!(rels(&b)[1], "A/node_modules");
    code(&mut b, KeyCode::Enter);
    assert!(b.search.is_none());
    assert_eq!(b.current_path(), tmp.path().join("A"));
    assert_eq!(b.dir().name(&b.dir().entries[b.cursor]), b"node_modules");
}

#[test]
fn enter_on_an_item_in_the_open_folder_keeps_the_stack() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "readme");
    code(&mut b, KeyCode::Enter);
    code(&mut b, KeyCode::Enter);
    assert_eq!(b.current_path(), tmp.path().join("C"));
    assert_eq!(b.dir().name(&b.dir().entries[b.cursor]), b"readme");
}

#[test]
fn a_result_that_is_gone_from_the_tree_is_dropped_and_never_mapped_to_another_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    let cursor_rel = rels(&b)[0].clone();
    let (folder, name) = cursor_rel.split_once('/').unwrap();
    let at = b.tree.dir.find(folder.as_bytes()).unwrap();
    let dir = b.tree.dir_at_mut(&[at]);
    let i = dir.find(name.as_bytes()).unwrap();
    dir.take(i);
    b.freeze_search();
    code(&mut b, KeyCode::Enter);
    assert!(b.search.is_some(), "the search stays open");
    assert!(b.message.as_deref().unwrap().contains("gone"));
    assert_eq!(rels(&b).len(), 1);
    assert_ne!(rels(&b)[0], cursor_rel);
}

#[test]
fn selection_lives_on_the_results_and_never_on_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    assert_eq!(s(&b).selected().count(), 2);
    assert_eq!(selected_in_tree(&b.tree.dir), 0);
    // A new query shows other rows. The selection is gone with the old ones.
    typed(&mut b, "/");
    typed(&mut b, "x");
    settle(&mut b, idle);
    assert_eq!(s(&b).selected().count(), 0);
    code(&mut b, KeyCode::Esc);
    assert_eq!(selected_in_tree(&b.tree.dir), 0);
}

#[test]
fn ctrl_a_in_the_query_selects_all_results_so_d_can_delete_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    ctrl_a(&mut b);
    assert_eq!(s(&b).focus, Focus::Results);
    assert_eq!(s(&b).selected().count(), 2);
}

#[test]
fn r_and_u_are_refused_in_a_search() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    for (c, what) in [('r', "rescan"), ('u', "undo")] {
        assert!(matches!(b.step(key(c)), Step::Continue));
        assert!(b.message.as_deref().unwrap().contains(what));
        assert!(b.search.is_some());
    }
}

#[test]
fn deleting_results_from_two_folders_sends_one_job_per_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, gate, seen) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    let total_before = b.tree.dir.totals();
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    typed(&mut b, "D");
    assert!(matches!(b.mode, Mode::Confirm { .. }));
    let shown = screen(&mut b);
    assert!(shown.contains("Delete 2 items permanently"), "{shown}");
    assert!(shown.contains("B/node_modules/"));
    typed(&mut b, "y");
    assert!(s(&b).hits.is_empty(), "the results leave the list at once");
    assert_eq!(s(&b).matches, 0);
    assert_eq!(s(&b).total, 0);
    assert!(b.tree.dir.totals().disk < total_before.disk);
    let a = b.tree.dir.find(b"A").unwrap();
    assert!(b.tree.dir_at(&[a]).find(b"node_modules").is_none());

    gate.send(()).unwrap();
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty() && idle(b));
    let jobs = seen.lock().unwrap().clone();
    let root = tmp.path();
    assert_eq!(
        jobs,
        [
            vec![root.join("A/node_modules")],
            vec![root.join("B/node_modules")]
        ]
    );
    assert!(!root.join("A/node_modules").exists());
    assert!(!root.join("B/node_modules").exists());
    assert!(root.join("A/src/main.rs").exists());
    assert_eq!(s(&b).hits.len(), 0);
}

#[test]
fn d_deletes_the_result_under_the_cursor_when_nothing_is_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, gate, seen) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    let first = rels(&b)[0].clone();
    typed(&mut b, "dy");
    assert_eq!(s(&b).hits.len(), 1);
    assert_eq!(s(&b).matches, 1);
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty() && idle(b));
    assert_eq!(seen.lock().unwrap()[0], [tmp.path().join(first)]);
}

#[test]
fn a_failed_delete_puts_the_rows_back_and_the_search_runs_again() {
    let tmp = tempfile::tempdir().unwrap();
    let busy = |_: &Path| Err("busy".to_string());
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, busy);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    typed(&mut b, "Dy");
    assert_eq!(s(&b).hits.len(), 0);
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty() && idle(b));
    assert_eq!(rels(&b), ["B/node_modules", "A/node_modules"]);
    assert_eq!(s(&b).matches, 2);
    assert!(b.message.as_deref().unwrap().contains("busy"));
    let a = b.tree.dir.find(b"A").unwrap();
    assert!(b.tree.dir_at(&[a]).find(b"node_modules").is_some());
}

#[test]
fn a_delete_waits_for_a_search_that_is_still_running() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    b.search.as_mut().unwrap().rerun = true;
    typed(&mut b, "D");
    assert!(matches!(b.mode, Mode::Browse));
    assert!(b.message.as_deref().unwrap().contains("Wait"));
}

#[test]
fn a_result_that_comes_while_a_question_is_open_waits_for_the_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    typed(&mut b, "D");
    let targets = match &b.mode {
        Mode::Confirm { targets, .. } => targets.clone(),
        _ => panic!("a question is open"),
    };
    b.start_search(false);
    thread::sleep(Duration::from_millis(50));
    b.poll_search();
    assert!(s(&b).waiting.is_some());
    assert_eq!(s(&b).hits.len(), 2);
    assert_eq!(targets, [0]);
    typed(&mut b, "n");
    b.poll_search();
    assert!(s(&b).waiting.is_none());
}

#[test]
fn typing_starts_a_new_search_and_the_old_one_never_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    typed(&mut b, "/");
    for text in ["r", "re", "rea", "read", "readme"] {
        send(&mut b, KeyCode::Char('u'), KeyModifiers::CONTROL);
        typed(&mut b, text);
    }
    settle(&mut b, idle);
    assert_eq!(rels(&b), ["C/readme"]);
}

#[test]
fn help_has_two_pages_about_the_search() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    typed(&mut b, "?");
    let first = screen(&mut b);
    assert!(first.contains("Search keys (1/2)"), "{first}");
    assert!(first.contains("Leave the search and show the folder"));
    typed(&mut b, "x");
    let second = screen(&mut b);
    assert!(second.contains("Search words (2/2)"), "{second}");
    for p in crate::search::PRESETS {
        assert!(second.contains(p.help), "{}", p.name);
    }
    for word in ["size>1G", "type:dir", "-word", "*.log", "cache/"] {
        assert!(second.contains(word), "{word}");
    }
    assert!(second.contains("Press any key to close"));
    typed(&mut b, "x");
    assert!(matches!(b.mode, Mode::Browse));
    assert!(b.search.is_some());
}

#[test]
fn the_browser_help_names_the_search_key() {
    let mut b = browser_with(Dir::default());
    b.mode = Mode::Help;
    let shown = screen(&mut b);
    assert!(shown.contains("Search this folder"), "{shown}");
}

#[test]
fn the_help_still_fits_a_terminal_of_24_rows_with_both_questions_off() {
    let mut b = browser_with(Dir::default());
    (b.ask_trash, b.ask_delete) = (false, false);
    b.mode = Mode::Help;
    let shown = screen(&mut b);
    assert!(shown.contains("Press any other key to close"), "{shown}");
}

#[test]
fn the_apparent_size_key_sorts_the_results_again() {
    let mut folder = Dir::default();
    folder.push(b"big-disk", Kind::File, 900, 1, 0);
    folder.push(b"big-apparent", Kind::File, 1, 800, 0);
    let mut b = browser_with(folder);
    typed(&mut b, "l");
    search_for(&mut b, "big");
    assert_eq!(rels(&b), ["big-disk", "big-apparent"]);
    code(&mut b, KeyCode::Enter);
    typed(&mut b, "a");
    settle(&mut b, idle);
    assert_eq!(rels(&b), ["big-apparent", "big-disk"]);
}

#[test]
fn results_are_sorted_by_size() {
    let mut folder = Dir::default();
    for (name, size) in [("b-small", 10), ("a-large", 300), ("c-middle", 40)] {
        folder.push(name.as_bytes(), Kind::File, size, size, 0);
    }
    let mut b = browser_with(folder);
    typed(&mut b, "l");
    search_for(&mut b, "-");
    assert_eq!(rels(&b), ["a-large", "c-middle", "b-small"]);
}

#[test]
fn changes_around_a_search_never_copy_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    // The delete in B fails, so its row comes back through a put-back.
    let act = |p: &Path| {
        if p.ends_with("B/node_modules") {
            Err("busy".to_string())
        } else {
            remove(p)
        }
    };
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, act);
    let root = Arc::as_ptr(&b.tree.dir);
    let unshared = |b: &Browser| {
        assert_eq!(Arc::strong_count(&b.tree.dir), 1);
        assert_eq!(Arc::as_ptr(&b.tree.dir), root, "the tree was copied");
    };

    // Typing starts and cancels searches that hold the root.
    typed(&mut b, "/");
    typed(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    b.freeze_search();
    unshared(&b);
    settle(&mut b, idle);

    // A delete takes rows out of the tree, and the failed one puts its row
    // back while a search is open.
    ctrl_a(&mut b);
    typed(&mut b, "Dy");
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty() && idle(b));
    unshared(&b);
    assert_eq!(rels(&b), ["B/node_modules"]);

    // An undo after the search.
    typed(&mut b, "/");
    typed(&mut b, "x");
    code(&mut b, KeyCode::Esc);
    code(&mut b, KeyCode::Esc);
    assert!(b.search.is_none());
    let i = b.tree.dir.find(b"C").unwrap();
    let taken = b.tree.dir_at_mut(&[]).take(i);
    b.put_back(tmp.path(), Vec::new(), 0, vec![taken]);
    unshared(&b);
}

#[test]
fn a_list_that_appears_under_the_key_is_not_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    assert_eq!(s(&b).selected().count(), 2);
    // A rerun finishes between the last redraw and the key.
    b.start_search(false);
    thread::sleep(Duration::from_millis(100));
    b.ask_delete = false;
    typed(&mut b, "D");
    assert!(matches!(b.mode, Mode::Browse));
    assert_eq!(b.pending.len(), 0, "nothing was deleted");
    assert_eq!(s(&b).hits.len(), 2);
    assert!(b.message.as_deref().unwrap().contains("The list changed"));
    // The user has seen the list now, and the key works.
    ctrl_a(&mut b);
    assert_eq!(s(&b).selected().count(), 2);
}

#[test]
fn a_list_held_back_by_a_question_does_not_bring_deleted_rows_back() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    typed(&mut b, "D");
    b.start_search(false);
    thread::sleep(Duration::from_millis(100));
    b.poll_search();
    assert!(s(&b).waiting.is_some());
    typed(&mut b, "y");
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty() && idle(b));
    assert_eq!(rels(&b), ["A/node_modules"]);
    assert_eq!(s(&b).matches, 1);
    assert!(s(&b).waiting.is_none());
}

#[test]
fn the_jobs_of_one_delete_show_one_progress_and_one_message() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    let freed: u64 = s(&b).hits.iter().map(|h| h.size).sum();
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    typed(&mut b, "Dy");
    assert_eq!(b.pending.len(), 2);
    let status = b.status(100).unwrap();
    assert!(
        status.starts_with("Deleting 2 items: 0% done (0 of 4 items)"),
        "{status}"
    );
    assert!(!status.contains("queued"), "{status}");

    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.len() == 1);
    assert_eq!(b.message, None, "the group is not done yet");
    let status = b.status(100).unwrap();
    assert!(status.starts_with("Deleting 2 items:"), "{status}");
    assert!(status.contains("of 4 items"), "{status}");

    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty());
    let freed = crate::tree::format_size(freed);
    assert_eq!(
        b.message,
        Some(format!("Deleted 2 items. Freed {}.", freed.trim_start()))
    );
}

#[test]
fn a_failed_job_of_a_group_shows_in_the_one_message() {
    let tmp = tempfile::tempdir().unwrap();
    let act = |p: &Path| {
        if p.ends_with("B/node_modules") {
            Err("busy".to_string())
        } else {
            remove(p)
        }
    };
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, act);
    search_for(&mut b, "@node");
    let freed_a = s(&b)
        .hits
        .iter()
        .find(|h| h.rel.starts_with(b"A/"))
        .unwrap()
        .size;
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    typed(&mut b, "Dy");
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty());
    let freed = crate::tree::format_size(freed_a);
    assert_eq!(
        b.message,
        Some(format!(
            "Deleted 1 of 2 items. Freed {}. Error: busy",
            freed.trim_start()
        ))
    );
}

#[test]
fn a_query_error_stays_on_the_screen_after_enter() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "size>x");
    code(&mut b, KeyCode::Enter);
    let shown = screen(&mut b);
    assert!(
        shown.lines().last().unwrap().contains("Bad size"),
        "{shown}"
    );
    assert_eq!(shown.matches("Bad size").count(), 2, "list and footer");
}

#[test]
fn a_result_whose_folder_is_gone_is_reported_not_skipped_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    b.freeze_search();
    let a = b.tree.dir.find(b"A").unwrap();
    b.tree.dir_at_mut(&[]).take(a);
    b.enqueue_results(true, &[0, 1]);
    assert_eq!(b.pending.len(), 1, "only B is deleted");
    assert!(
        b.message
            .as_deref()
            .unwrap()
            .starts_with("1 result not deleted"),
        "{:?}",
        b.message
    );
    assert!(b.message_is_error);
}

#[test]
fn the_query_has_a_length_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    typed(&mut b, "/");
    typed(&mut b, &"a".repeat(600));
    assert_eq!(s(&b).query.chars().count(), crate::search::MAX_QUERY);
    assert!(b.message.as_deref().unwrap().contains("as long as"));
}

#[test]
fn a_move_to_the_trash_from_results_frees_nothing_in_the_message() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, gate, _) = gated_browser(tmp.path(), &FILES, remove);
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    ctrl_a(&mut b);
    typed(&mut b, "dy");
    gate.send(()).unwrap();
    gate.send(()).unwrap();
    settle(&mut b, |b| b.pending.is_empty());
    let message = b.message.clone().unwrap();
    assert!(
        message.starts_with("Moved to the Trash: 2 items."),
        "{message}"
    );
    assert!(!message.contains("Freed"), "{message}");
}

#[test]
fn the_search_footer_shows_the_free_space_before_the_position() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut b, _gate, _) = gated_browser(tmp.path(), &FILES, remove);
    b.free = FreeSpace::new(Box::new(|_| Some(182 << 30)));
    b.refresh_free();
    search_for(&mut b, "@node");
    code(&mut b, KeyCode::Enter);
    let shown = screen(&mut b);
    let footer = shown.lines().last().unwrap();
    assert!(footer.starts_with(" 2 matches: "), "{footer}");
    assert!(
        footer.trim_end().ends_with("182.0 GiB free  1/2"),
        "{footer}"
    );
}
