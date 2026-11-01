//! Search in the scanned tree. It reads the tree in memory and never the
//! disk, so a search over millions of items takes a fraction of a second.
//!
//! A query is a list of words. Every word must match. [`Query::parse`] builds
//! the matchers once, and [`run`] walks the tree in parallel and keeps the
//! largest matches.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::tree::{Dir, Entry, Kind};

/// The most matches that a search keeps. They are the largest ones.
pub const MAX_HITS: usize = 10_000;

/// A folder with at least this many items splits its subfolders over the
/// threads. A smaller folder is cheaper to walk on one thread.
const PARALLEL_ITEMS: u64 = 20_000;

/// A walk looks at the cancel flag after this many entries of one folder.
const CANCEL_EVERY: usize = 4096;

/// The longest query. A longer text is a paste by mistake.
pub const MAX_QUERY: usize = 512;

/// A subfolder with fewer items is walked in place, not on its own task.
const TASK_ITEMS: u64 = 1_000;

/// What a ready-made search sees of a folder.
pub struct Probe<'a> {
    /// The path of the folder that holds the item, with a `/` at the end.
    parent_path: &'a [u8],
    parent: &'a Dir,
    name: &'a [u8],
    /// The content of the item, when it is a folder that was read.
    sub: Option<&'a Dir>,
}

impl Probe<'_> {
    /// Whether a file (not a folder) with this name is next to the item.
    fn has_sibling_file(&self, name: &[u8]) -> bool {
        has_file(self.parent, name)
    }

    fn has_inner_file(&self, name: &[u8]) -> bool {
        self.sub.is_some_and(|sub| has_file(sub, name))
    }

    /// Whether the last names of the folder path are `tail`, as whole names.
    fn parent_ends_with(&self, tail: &[&[u8]]) -> bool {
        let mut names = self.parent_names().rev();
        tail.iter().rev().all(|t| names.next() == Some(*t))
    }

    fn parent_names(&self) -> impl DoubleEndedIterator<Item = &[u8]> {
        self.parent_path
            .split(|&b| b == b'/')
            .filter(|n| !n.is_empty())
    }

    /// Places that hold other people's folders: app bundles and the
    /// extensions of editors. Their `node_modules` and caches are not
    /// the user's to clean up.
    fn in_foreign_place(&self) -> bool {
        const EDITORS: [&[u8]; 3] = [b".vscode", b".vscode-insiders", b".cursor"];
        let names: Vec<&[u8]> = self.parent_names().collect();
        names.iter().any(|n| n.ends_with(b".app"))
            || names
                .windows(2)
                .any(|w| EDITORS.contains(&w[0]) && w[1] == b"extensions")
    }
}

fn has_file(dir: &Dir, name: &[u8]) -> bool {
    dir.find(name)
        .is_some_and(|i| dir.entries[i].kind != Kind::Dir)
}

/// A ready-made search: `@name` in a query. It looks at names in memory only.
/// A match is always a folder. To add one, add a line to `PRESETS`.
pub struct Preset {
    pub name: &'static str,
    pub help: &'static str,
    test: fn(&Probe) -> bool,
}

pub static PRESETS: &[Preset] = &[
    Preset {
        name: "node",
        help: "node_modules next to a package.json",
        test: |p| p.name == b"node_modules" && p.has_sibling_file(b"package.json"),
    },
    Preset {
        name: "rust",
        help: "target next to a Cargo.toml, or with a CACHEDIR.TAG",
        test: |p| {
            p.name == b"target"
                && (p.has_sibling_file(b"Cargo.toml") || p.has_inner_file(b"CACHEDIR.TAG"))
        },
    },
    Preset {
        name: "gradle",
        help: ".gradle and build next to a build.gradle(.kts)",
        test: |p| {
            let build =
                p.has_sibling_file(b"build.gradle") || p.has_sibling_file(b"build.gradle.kts");
            let settings = p.has_sibling_file(b"settings.gradle")
                || p.has_sibling_file(b"settings.gradle.kts");
            (p.name == b".gradle" && (build || settings)) || (p.name == b"build" && build)
        },
    },
    Preset {
        name: "python",
        help: "__pycache__, and .venv or venv with a pyvenv.cfg",
        test: |p| {
            p.name == b"__pycache__"
                || ((p.name == b".venv" || p.name == b"venv") && p.has_inner_file(b"pyvenv.cfg"))
        },
    },
    Preset {
        name: "xcode",
        help: "Xcode DerivedData and * DeviceSupport",
        test: |p| {
            p.parent_ends_with(&[b"Developer", b"Xcode"])
                && (p.name == b"DerivedData" || p.name.ends_with(b" DeviceSupport"))
        },
    },
    Preset {
        name: "cache",
        help: "folders with a CACHEDIR.TAG, and Library/Caches",
        test: |p| {
            p.has_inner_file(b"CACHEDIR.TAG")
                || (p.name == b"Caches" && p.parent_ends_with(&[b"Library"]))
        },
    },
    Preset {
        name: "dev",
        help: "all of the above",
        test: dev,
    },
];

fn dev(p: &Probe) -> bool {
    PRESETS.iter().any(|x| x.name != "dev" && (x.test)(p))
}

/// The ready-made searches that a last word like `@ru` can mean, when the
/// word is not a complete name yet.
pub fn presets_for(text: &str) -> Option<Vec<&'static Preset>> {
    let word = text.split_whitespace().last()?;
    let prefix = word.strip_prefix('@')?.to_lowercase();
    if text.ends_with(char::is_whitespace) || PRESETS.iter().any(|p| p.name == prefix) {
        return None;
    }
    let list: Vec<_> = PRESETS
        .iter()
        .filter(|p| p.name.starts_with(&prefix))
        .collect();
    (!list.is_empty()).then_some(list)
}

/// Tab completion for a last word like `@ru`. One match completes the name
/// and adds a space. Several matches complete their common start.
pub fn complete(text: &str) -> Option<String> {
    let list = presets_for(text)?;
    let word = text.split_whitespace().last()?;
    let typed = word.len() - 1;
    let head = &text[..text.len() - word.len()];
    let mut common = list[0].name;
    for p in &list[1..] {
        let n = common
            .chars()
            .zip(p.name.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common = &common[..n];
    }
    if list.len() == 1 {
        return Some(format!("{head}@{} ", list[0].name));
    }
    (common.len() > typed).then(|| format!("{head}@{common}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cmp {
    Gt,
    Ge,
    Lt,
    Le,
}

/// Text that is matched without regard to case.
struct Needle {
    lower: Vec<u8>,
    ascii: bool,
}

impl Needle {
    fn new(text: &str) -> Self {
        let lower = text.to_lowercase();
        Needle {
            ascii: lower.is_ascii(),
            lower: lower.into_bytes(),
        }
    }

    fn found_in(&self, hay: &[u8]) -> bool {
        if hay.is_ascii() {
            // A needle with other characters cannot match an ASCII text.
            self.ascii && find_ignoring_case(hay, &self.lower)
        } else {
            let lower = String::from_utf8_lossy(hay).to_lowercase();
            find_ignoring_case(lower.as_bytes(), &self.lower)
        }
    }
}

/// `needle` is lower case.
fn find_ignoring_case(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, _)) = needle.split_first() else {
        return true;
    };
    let Some(last_start) = hay.len().checked_sub(needle.len()) else {
        return false;
    };
    (0..=last_start).any(|i| {
        hay[i].to_ascii_lowercase() == first
            && hay[i..i + needle.len()].eq_ignore_ascii_case(needle)
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    Star,
    One,
    Lit(char),
}

/// A pattern with `*` (any text) and `?` (one character), matched without
/// regard to case and against the whole text.
struct Glob(Vec<Tok>);

impl Glob {
    fn new(pattern: &str) -> Self {
        Glob(
            pattern
                .to_lowercase()
                .chars()
                .map(|c| match c {
                    '*' => Tok::Star,
                    '?' => Tok::One,
                    c => Tok::Lit(c),
                })
                .collect(),
        )
    }

    fn matches(&self, text: &[u8]) -> bool {
        if text.is_ascii() {
            glob_run(&self.0, text, |c, b| {
                c.is_ascii() && c as u8 == b.to_ascii_lowercase()
            })
        } else {
            let chars: Vec<char> = String::from_utf8_lossy(text)
                .to_lowercase()
                .chars()
                .collect();
            glob_run(&self.0, &chars, |c, x| c == x)
        }
    }
}

/// The usual two-pointer match. On a mismatch it goes back to the last `*`
/// and lets it take one more element.
fn glob_run<T: Copy>(pattern: &[Tok], text: &[T], eq: impl Fn(char, T) -> bool) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(Tok::Star) => {
                star = Some((p, t));
                p += 1;
            }
            Some(Tok::One) => {
                p += 1;
                t += 1;
            }
            Some(&Tok::Lit(c)) if eq(c, text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&tok| tok == Tok::Star)
}

enum Text {
    Plain(Needle),
    Glob(Glob),
}

impl Text {
    fn new(word: &str) -> Self {
        if word.contains(['*', '?']) {
            Text::Glob(Glob::new(word))
        } else {
            Text::Plain(Needle::new(word))
        }
    }

    fn matches(&self, text: &[u8]) -> bool {
        match self {
            Text::Plain(n) => n.found_in(text),
            Text::Glob(g) => g.matches(text),
        }
    }
}

enum Word {
    Size(Cmp, u64),
    Folder(bool),
    /// Matches the name.
    Name(Text),
    Preset(&'static Preset),
    /// Matches the path below the folder that the search starts in.
    Path(Text),
}

impl Word {
    /// The cheap tests come first.
    fn cost(&self) -> u8 {
        match self {
            Word::Size(..) | Word::Folder(_) => 0,
            Word::Name(_) => 1,
            Word::Preset(_) => 2,
            Word::Path(_) => 3,
        }
    }
}

/// A parsed search. Build it once and use it for the whole walk.
pub struct Query {
    words: Vec<Word>,
    /// `-word`: leaves out an item when its path has the word. A pattern
    /// is tested against each name in the path.
    not: Vec<Text>,
    needs_path: bool,
}

impl Query {
    /// Returns a short message for a word that makes no sense.
    pub fn parse(text: &str) -> Result<Query, String> {
        if text.chars().count() > MAX_QUERY {
            return Err(format!("The query is longer than {MAX_QUERY} characters"));
        }
        let mut words = Vec::new();
        let mut not = Vec::new();
        for raw in text.split_whitespace() {
            if let Some(rest) = raw.strip_prefix('-').filter(|r| !r.is_empty()) {
                not.push(Text::new(rest));
            } else if let Some(name) = raw.strip_prefix('@') {
                let name = name.to_lowercase();
                let preset = PRESETS
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| format!("No ready-made search {raw}. Type @ to see the list"))?;
                words.push(Word::Preset(preset));
            } else if raw.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("size"))
                && let Some((cmp, bytes)) = parse_size(&raw[4..])
            {
                words.push(Word::Size(cmp, bytes?));
            } else if raw
                .get(..5)
                .is_some_and(|s| s.eq_ignore_ascii_case("type:"))
            {
                match raw[5..].to_lowercase().as_str() {
                    "dir" | "folder" => words.push(Word::Folder(true)),
                    "file" => words.push(Word::Folder(false)),
                    _ => return Err(format!("Unknown type in {raw}. Use type:dir or type:file")),
                }
            } else if raw.contains('/') {
                words.push(Word::Path(Text::new(raw)));
            } else {
                words.push(Word::Name(Text::new(raw)));
            }
        }
        words.sort_by_key(Word::cost);
        let needs_path = !not.is_empty() || words.iter().any(|w| matches!(w, Word::Path(_)));
        Ok(Query {
            words,
            not,
            needs_path,
        })
    }

    /// A query without a positive word would match everything.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    fn excluded(&self, rel: &[u8]) -> bool {
        self.not.iter().any(|t| match t {
            Text::Plain(n) => n.found_in(rel),
            Text::Glob(g) => rel.split(|&b| b == b'/').any(|part| g.matches(part)),
        })
    }

    fn matches(&self, e: &Entry, probe: &Probe, rel: &[u8], apparent: bool) -> bool {
        self.words.iter().all(|w| match w {
            Word::Size(cmp, limit) => {
                let size = e.size(apparent);
                match cmp {
                    Cmp::Gt => size > *limit,
                    Cmp::Ge => size >= *limit,
                    Cmp::Lt => size < *limit,
                    Cmp::Le => size <= *limit,
                }
            }
            Word::Folder(want) => (e.kind == Kind::Dir) == *want,
            Word::Name(t) => t.matches(probe.name),
            Word::Preset(p) => e.kind == Kind::Dir && (p.test)(probe) && !probe.in_foreign_place(),
            Word::Path(t) => t.matches(rel),
        })
    }
}

/// `size>1G`: the part after `size`. `None` means this is no size word, for
/// example a file name that starts with `size`. `Some` with an error means a
/// bad number or unit.
fn parse_size(rest: &str) -> Option<(Cmp, Result<u64, String>)> {
    let (cmp, number) = if let Some(n) = rest.strip_prefix(">=") {
        (Cmp::Ge, n)
    } else if let Some(n) = rest.strip_prefix("<=") {
        (Cmp::Le, n)
    } else if let Some(n) = rest.strip_prefix('>') {
        (Cmp::Gt, n)
    } else {
        (Cmp::Lt, rest.strip_prefix('<')?)
    };
    let bytes = parse_bytes(number)
        .ok_or_else(|| format!("Bad size size{rest}. Examples: size>1G, size<=500M"));
    Some((cmp, bytes))
}

/// A number with an optional unit K, M, G or T (base 1024, also KB or KiB).
fn parse_bytes(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let digits = lower
        .strip_suffix("ib")
        .or_else(|| lower.strip_suffix('b'))
        .unwrap_or(&lower);
    let (number, shift) = match digits.chars().last()? {
        'k' => (&digits[..digits.len() - 1], 10),
        'm' => (&digits[..digits.len() - 1], 20),
        'g' => (&digits[..digits.len() - 1], 30),
        't' => (&digits[..digits.len() - 1], 40),
        _ => (digits, 0),
    };
    let value: f64 = number.parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * (1u64 << shift) as f64) as u64)
}

/// One match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The path below the folder that the search started in, with `/`
    /// between the names.
    pub rel: Vec<u8>,
    pub size: u64,
    pub is_dir: bool,
    pub selected: bool,
}

/// The result of a finished search.
pub struct Outcome {
    /// The largest matches, the largest first.
    pub hits: Vec<Hit>,
    /// All matches, also those that `hits` leaves out.
    pub matches: u64,
    /// The size of all matches.
    pub total: u64,
}

/// A match in the heap. The order is the order of the result: size first,
/// then the name, so ties come out the same every time.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Ranked {
    size: u64,
    name: Reverse<Vec<u8>>,
    is_dir: bool,
}

#[derive(Default)]
struct Found {
    count: u64,
    total: u64,
    /// The smallest kept match is on top.
    heap: BinaryHeap<Reverse<Ranked>>,
}

impl Found {
    /// `prefix` and `name` make the path of the match. The path is built
    /// only for a match that the list keeps.
    fn add(&mut self, size: u64, is_dir: bool, prefix: &[u8], name: &[u8]) {
        self.count += 1;
        self.total += size;
        let wins = self.heap.len() < MAX_HITS
            || self.heap.peek().is_some_and(|min| {
                // The same order as `Ranked`: size first, then the smaller path.
                size > min.0.size
                    || (size == min.0.size && cmp_joined(prefix, name, &min.0.name.0).is_lt())
            });
        if wins {
            let mut rel = Vec::with_capacity(prefix.len() + name.len());
            rel.extend_from_slice(prefix);
            rel.extend_from_slice(name);
            self.keep(Ranked {
                size,
                name: Reverse(rel),
                is_dir,
            });
        }
    }

    fn keep(&mut self, r: Ranked) {
        self.heap.push(Reverse(r));
        if self.heap.len() > MAX_HITS {
            self.heap.pop();
        }
    }

    fn merge(self, other: Found) -> Found {
        let (mut big, small) = if self.heap.len() >= other.heap.len() {
            (self, other)
        } else {
            (other, self)
        };
        big.count += small.count;
        big.total += small.total;
        for Reverse(r) in small.heap {
            if big.heap.len() < MAX_HITS || big.heap.peek().is_some_and(|min| r > min.0) {
                big.keep(r);
            }
        }
        big
    }
}

/// Compares `a` followed by `b` with `other`, without joining them.
fn cmp_joined(a: &[u8], b: &[u8], other: &[u8]) -> std::cmp::Ordering {
    let (head, tail) = other.split_at(a.len().min(other.len()));
    a.cmp(head).then_with(|| b.cmp(tail))
}

struct Walk<'a> {
    query: &'a Query,
    apparent: bool,
    cancel: &'a AtomicBool,
    /// The length of the path of the folder that the search starts in.
    base: usize,
}

/// Searches `root`. `base` is the absolute path of `root` with a `/` at the
/// end. The search stops early and returns `None` when `cancel` is set.
pub fn run(
    root: &Dir,
    base: &[u8],
    query: &Query,
    apparent: bool,
    cancel: &AtomicBool,
) -> Option<Outcome> {
    let walk = Walk {
        query,
        apparent,
        cancel,
        base: base.len(),
    };
    let items = root.entries.iter().map(|e| e.items).sum::<u64>();
    let found = walk.dir(root, &mut base.to_vec(), items >= PARALLEL_ITEMS);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let (count, total) = (found.count, found.total);
    let hits = found
        .heap
        .into_sorted_vec()
        .into_iter()
        .map(|Reverse(r)| Hit {
            rel: r.name.0,
            size: r.size,
            is_dir: r.is_dir,
            selected: false,
        })
        .collect();
    Some(Outcome {
        hits,
        matches: count,
        total,
    })
}

impl Walk<'_> {
    /// `path` holds the absolute path of `dir` with a `/` at the end. It
    /// has the same content when the call returns.
    fn dir(&self, dir: &Dir, path: &mut Vec<u8>, parallel: bool) -> Found {
        let mut found = Found::default();
        if self.cancel.load(Ordering::Relaxed) {
            return found;
        }
        let mut tasks: Vec<(&[u8], &Dir, u64)> = Vec::new();
        for (n, e) in dir.entries.iter().enumerate() {
            // A flat folder can hold millions of entries.
            if n % CANCEL_EVERY == CANCEL_EVERY - 1 && self.cancel.load(Ordering::Relaxed) {
                return found;
            }
            let name = dir.name(e);
            let len = path.len();
            let mut rel: &[u8] = &[];
            if self.query.needs_path {
                path.extend_from_slice(name);
                rel = &path[self.base..];
                if self.query.excluded(rel) {
                    path.truncate(len);
                    continue;
                }
            }
            let probe = Probe {
                parent_path: &path[..len],
                parent: dir,
                name,
                sub: e.dir.as_deref(),
            };
            let hit = self.query.matches(e, &probe, rel, self.apparent);
            if hit {
                let size = e.size(self.apparent);
                found.add(size, e.kind == Kind::Dir, &path[self.base..len], name);
            }
            path.truncate(len);
            // A folder that matches is not searched, so no match lies inside
            // another one.
            if !hit && let Some(sub) = e.dir.as_deref() {
                if parallel && e.items >= TASK_ITEMS {
                    tasks.push((name, sub, e.items));
                } else {
                    path.extend_from_slice(name);
                    path.push(b'/');
                    found = found.merge(self.dir(sub, path, e.items >= PARALLEL_ITEMS));
                    path.truncate(len);
                }
            }
        }
        if tasks.is_empty() {
            return found;
        }
        let parts = tasks
            .par_iter()
            .map(|&(name, sub, items)| {
                let mut path = path.clone();
                path.extend_from_slice(name);
                path.push(b'/');
                self.dir(sub, &mut path, items >= PARALLEL_ITEMS)
            })
            .reduce(Found::default, Found::merge);
        found.merge(parts)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use super::*;
    use crate::tree::Dir;

    /// Builds a tree from paths. A path that ends with `/` is an empty
    /// folder. Every other path is a file with the given size.
    pub(crate) fn build(paths: &[(&str, u64)]) -> Dir {
        type Items<'a> = Vec<(Vec<&'a str>, bool, u64)>;
        fn level(items: &[(Vec<&str>, bool, u64)]) -> Dir {
            let mut groups: BTreeMap<&str, Items> = BTreeMap::new();
            let mut dir = Dir::default();
            for (parts, folder, size) in items {
                if parts.len() == 1 && !folder {
                    dir.push(parts[0].as_bytes(), Kind::File, *size, *size, 0);
                } else {
                    groups
                        .entry(parts[0])
                        .or_default()
                        .push((parts[1..].to_vec(), *folder, *size));
                }
            }
            for (name, children) in groups {
                dir.push(name.as_bytes(), Kind::Dir, 0, 0, 0);
                let i = dir.entries.len() - 1;
                let children: Vec<_> = children.into_iter().filter(|c| !c.0.is_empty()).collect();
                dir.attach(i, level(&children), 0);
            }
            dir
        }
        let items: Vec<_> = paths
            .iter()
            .map(|&(path, size)| {
                (
                    path.split('/')
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>(),
                    path.ends_with('/'),
                    size,
                )
            })
            .collect();
        level(&items)
    }

    pub(crate) fn find(root: &Dir, query: &str) -> Vec<String> {
        let q = Query::parse(query).unwrap();
        let out = run(root, b"/r/", &q, false, &AtomicBool::new(false)).unwrap();
        out.hits
            .iter()
            .map(|h| String::from_utf8_lossy(&h.rel).into_owned())
            .collect()
    }

    fn err(query: &str) -> String {
        Query::parse(query).err().unwrap()
    }

    #[test]
    fn plain_words_match_the_name_without_regard_to_case() {
        let root = build(&[
            ("a/Node_Modules/x", 5),
            ("a/other/node", 3),
            ("b/readme", 1),
        ]);
        assert_eq!(find(&root, "node"), ["a/Node_Modules", "a/other/node"]);
        assert_eq!(find(&root, "NODE_mod"), ["a/Node_Modules"]);
    }

    #[test]
    fn plain_words_match_names_with_other_characters() {
        let root = build(&[("Ærlig/Båt.txt", 5), ("x/bat.txt", 3)]);
        assert_eq!(find(&root, "båt"), ["Ærlig/Båt.txt"]);
        assert_eq!(find(&root, "ærlig"), ["Ærlig"]);
        assert_eq!(find(&root, "bat"), ["x/bat.txt"]);
    }

    #[test]
    fn globs_match_the_whole_name_without_regard_to_case() {
        let root = build(&[
            ("a/x.DMG", 5),
            ("a/x.dmg.txt", 4),
            ("b/y.log", 3),
            ("b/yy.log", 2),
        ]);
        assert_eq!(find(&root, "*.dmg"), ["a/x.DMG"]);
        assert_eq!(find(&root, "y?.log"), ["b/yy.log"]);
        assert_eq!(find(&root, "y*.log"), ["b/y.log", "b/yy.log"]);
        assert_eq!(find(&root, "?.dmg*"), ["a/x.DMG", "a/x.dmg.txt"]);
    }

    #[test]
    fn the_glob_matcher_handles_stars_and_backtracking() {
        let g = |p: &str, t: &str| Glob::new(p).matches(t.as_bytes());
        assert!(g("*", ""));
        assert!(g("a*b*c", "aXbXbXc"));
        assert!(g("a*b", "abab"));
        assert!(!g("a*b", "abac"));
        assert!(g("**x", "x"));
        assert!(!g("?", ""));
        assert!(g("?é*", "aÉb"));
        assert!(!g("a?", "ab/"));
    }

    #[test]
    fn a_word_with_a_slash_matches_the_path_below_the_search_folder() {
        let root = build(&[("p/cache/x", 5), ("p/other/y", 3), ("cache2/z", 2)]);
        assert_eq!(find(&root, "cache/"), ["p/cache/x"]);
        assert_eq!(find(&root, "p/*/y"), ["p/other/y"]);
    }

    #[test]
    fn size_words_compare_with_the_size_in_the_chosen_mode() {
        let mut root = Dir::default();
        root.push(b"big", Kind::File, 1 << 30, 5, 0);
        root.push(b"mid", Kind::File, 500 << 20, 5, 0);
        root.push(b"small", Kind::File, 10 << 20, 5, 0);
        let q = |s: &str, apparent| {
            let q = Query::parse(s).unwrap();
            let out = run(&root, b"/r/", &q, apparent, &AtomicBool::new(false)).unwrap();
            out.hits.len()
        };
        assert_eq!(q("size>1G", false), 0);
        assert_eq!(q("size>=1g", false), 1);
        assert_eq!(q("size>=500M", false), 2);
        assert_eq!(q("size<10MiB", false), 0);
        assert_eq!(q("SIZE<=10mb", false), 1);
        assert_eq!(q("size>0", true), 3);
        assert_eq!(q("size>6", true), 0);
    }

    #[test]
    fn size_units_use_base_1024() {
        assert_eq!(parse_bytes("1K"), Some(1024));
        assert_eq!(parse_bytes("1.5m"), Some(3 << 19));
        assert_eq!(parse_bytes("2GiB"), Some(2 << 30));
        assert_eq!(parse_bytes("1TB"), Some(1 << 40));
        assert_eq!(parse_bytes("17"), Some(17));
        assert_eq!(parse_bytes("17B"), Some(17));
        assert_eq!(parse_bytes(""), None);
        assert_eq!(parse_bytes("G"), None);
        assert_eq!(parse_bytes("-1G"), None);
        assert_eq!(parse_bytes("1X"), None);
    }

    #[test]
    fn type_words_split_folders_from_everything_else() {
        let mut root = build(&[("a/f", 5), ("b/", 0)]);
        root.push(b"link", Kind::Symlink, 1, 1, 0);
        assert_eq!(find(&root, "type:dir"), ["a", "b"]);
        assert_eq!(find(&root, "TYPE:file"), ["a/f", "link"]);
    }

    #[test]
    fn a_minus_leaves_out_paths_with_the_word_or_a_matching_name() {
        let root = build(&[
            ("p/.git/x.js", 9),
            ("p/src/x.js", 5),
            ("q/vendor.git/y.js", 4),
            ("q/z.js", 3),
        ]);
        assert_eq!(find(&root, "*.js -.git"), ["p/src/x.js", "q/z.js"]);
        assert_eq!(find(&root, "*.js -*.git"), ["p/src/x.js", "q/z.js"]);
        assert_eq!(
            find(&root, "*.js -v*"),
            ["p/.git/x.js", "p/src/x.js", "q/z.js"]
        );
        assert_eq!(find(&root, "x.js -src"), ["p/.git/x.js"]);
    }

    #[test]
    fn words_must_all_match() {
        let root = build(&[
            ("a/node_modules/x", 5),
            ("b/node/y", 2),
            ("node_modules", 1),
        ]);
        assert_eq!(find(&root, "node type:file"), ["node_modules"]);
        assert_eq!(find(&root, "node size>3 type:dir"), ["a/node_modules"]);
    }

    #[test]
    fn a_bad_word_gives_a_short_message() {
        assert!(err("@nope").contains("@nope"));
        assert!(err("size>x").contains("Bad size"));
        assert!(err("size>").contains("Bad size"));
        assert!(err("size>1X").contains("Bad size"));
        assert!(err("type:link").contains("type:dir"));
        // A name that only starts with size is a name.
        assert!(Query::parse("sizes size.txt").is_ok());
        assert!(Query::parse("").unwrap().is_empty());
        assert!(Query::parse("-x").unwrap().is_empty());
    }

    #[test]
    fn node_needs_a_package_json_next_to_node_modules_folders() {
        let root = build(&[
            ("a/package.json", 1),
            ("a/node_modules/x", 5),
            ("a/src/y", 1),
            ("b/package.json/", 0),
            ("b/node_modules/x", 4),
            ("c/package.json", 1),
            ("c/node_modules", 1),
            ("lib/node_modules/x", 9),
        ]);
        // b has a folder named package.json, c has a file named node_modules,
        // and lib has no package.json (a global install).
        assert_eq!(find(&root, "@node"), ["a/node_modules"]);
    }

    #[test]
    fn app_bundles_and_editor_extensions_are_left_out_of_every_preset() {
        let root = build(&[
            ("Apps/Tool.app/Contents/Resources/package.json", 1),
            ("Apps/Tool.app/Contents/Resources/node_modules/x", 90),
            ("Apps/Tool.app/Contents/target/Cargo.toml", 1),
            ("Apps/Tool.app/Contents/__pycache__/x", 91),
            (".vscode/extensions/e/package.json", 1),
            (".vscode/extensions/e/node_modules/x", 92),
            (".vscode-insiders/extensions/__pycache__/x", 93),
            (".cursor/extensions/e/__pycache__/x", 94),
            (".vscode/other/__pycache__/x", 5),
            ("work/App/package.json", 1),
            ("work/App/node_modules/x", 6),
        ]);
        assert_eq!(
            find(&root, "@dev"),
            ["work/App/node_modules", ".vscode/other/__pycache__"]
        );
    }

    #[test]
    fn rust_needs_a_cargo_toml_next_to_target_or_a_cachedir_tag_in_it() {
        let root = build(&[
            ("a/Cargo.toml", 1),
            ("a/target/debug/x", 50),
            ("b/target/out", 40),
            ("c/target/CACHEDIR.TAG", 1),
            ("c/target/rel/x", 30),
            ("d/target/Cargo.toml", 1),
            ("e/Cargo.toml/", 0),
            ("e/target/x", 20),
        ]);
        assert_eq!(find(&root, "@rust"), ["a/target", "c/target"]);
    }

    #[test]
    fn gradle_needs_build_gradle_next_to_build() {
        let root = build(&[
            ("a/build.gradle", 1),
            ("a/build/x", 10),
            ("a/.gradle/x", 20),
            ("b/build.gradle.kts", 1),
            ("b/build/x", 11),
            ("c/build/x", 12),
            ("d/build.gradle/", 0),
            ("d/build/x", 13),
            ("e/settings.gradle.kts", 1),
            ("e/.gradle/x", 14),
            ("home/.gradle/gradle.properties", 15),
            ("home/.gradle/wrapper/dists/x", 16),
        ]);
        // The .gradle in home has no project file next to it: it holds
        // credentials and wrapper downloads, not a build cache.
        assert_eq!(
            find(&root, "@gradle"),
            ["a/.gradle", "e/.gradle", "b/build", "a/build"]
        );
    }

    #[test]
    fn python_needs_a_pyvenv_cfg_in_venv_folders() {
        let root = build(&[
            ("a/__pycache__/x", 1),
            ("a/.venv/pyvenv.cfg", 1),
            ("a/.venv/lib/x", 50),
            ("b/venv/pyvenv.cfg", 1),
            ("c/.venv/x", 60),
            ("d/venv/lib/x", 70),
        ]);
        assert_eq!(
            find(&root, "@python"),
            ["a/.venv", "a/__pycache__", "b/venv"]
        );
    }

    #[test]
    fn xcode_needs_the_developer_xcode_path() {
        let root = build(&[
            ("Library/Developer/Xcode/DerivedData/x", 5),
            ("Library/Developer/Xcode/iOS DeviceSupport/x", 6),
            ("Library/Developer/Xcode/watchOS DeviceSupport/x", 7),
            ("Library/Developer/Xcode/Archives/x", 8),
            ("Library/Other/DerivedData/x", 9),
            ("Library/Other/iOS DeviceSupport/x", 10),
        ]);
        assert_eq!(
            find(&root, "@xcode"),
            [
                "Library/Developer/Xcode/watchOS DeviceSupport",
                "Library/Developer/Xcode/iOS DeviceSupport",
                "Library/Developer/Xcode/DerivedData",
            ]
        );
    }

    #[test]
    fn xcode_and_cache_match_whole_names_in_the_path() {
        let root = build(&[
            ("xDeveloper/Xcode/DerivedData/x", 1),
            ("Developer/Xcodes/DerivedData/x", 2),
            ("Developer/Xcode/DerivedData/x", 3),
            ("MyLibrary/Caches/x", 4),
            ("Library/Caches/x", 5),
        ]);
        assert_eq!(
            find(&root, "@xcode @cache"),
            Vec::<String>::new(),
            "both words must match"
        );
        assert_eq!(find(&root, "@xcode"), ["Developer/Xcode/DerivedData"]);
        assert_eq!(find(&root, "@cache"), ["Library/Caches"]);
    }

    #[test]
    fn ties_at_the_cap_keep_the_smallest_paths_every_time() {
        let mut root = Dir::default();
        for i in (0..MAX_HITS + 100).rev() {
            root.push(format!("f{i:05}").as_bytes(), Kind::File, 7, 7, 0);
        }
        let first = find(&root, "f");
        assert_eq!(first.len(), MAX_HITS);
        assert_eq!(first[0], "f00000");
        assert_eq!(first[MAX_HITS - 1], format!("f{:05}", MAX_HITS - 1));
        assert_eq!(first, find(&root, "f"));
    }

    #[test]
    fn a_very_long_query_is_refused() {
        assert!(Query::parse(&"a".repeat(MAX_QUERY)).is_ok());
        assert!(err(&"a".repeat(MAX_QUERY + 1)).contains("512"));
    }

    #[test]
    fn a_flat_folder_stops_when_the_search_is_cancelled() {
        let mut root = Dir::default();
        for i in 0..CANCEL_EVERY * 3 {
            root.push(format!("f{i}").as_bytes(), Kind::File, 1, 1, 0);
        }
        let q = Query::parse("f").unwrap();
        let stop = AtomicBool::new(true);
        let walk = Walk {
            query: &q,
            apparent: false,
            cancel: &stop,
            base: 3,
        };
        // The walk looks at the flag inside the folder, after 4,096 entries.
        let found = walk.dir(&root, &mut b"/r/".to_vec(), false);
        assert!(found.count < CANCEL_EVERY as u64);
    }

    #[test]
    fn xcode_sees_the_path_above_the_search_folder() {
        let root = build(&[("DerivedData/x", 5)]);
        let q = Query::parse("@xcode").unwrap();
        let stop = AtomicBool::new(false);
        let hits = |base: &[u8]| run(&root, base, &q, false, &stop).unwrap().hits.len();
        assert_eq!(hits(b"/Users/me/Library/Developer/Xcode/"), 1);
        assert_eq!(hits(b"/Users/me/Library/"), 0);
    }

    #[test]
    fn cache_finds_cachedir_tags_and_library_caches() {
        let root = build(&[
            ("a/c1/CACHEDIR.TAG", 1),
            ("a/c1/x", 50),
            ("Library/Caches/x", 40),
            ("Other/Caches/x", 30),
            ("a/CACHEDIR.TAG/", 0),
        ]);
        assert_eq!(find(&root, "@cache"), ["a/c1", "Library/Caches"]);
    }

    #[test]
    fn dev_is_the_union_of_the_others() {
        let root = build(&[
            ("a/Cargo.toml", 1),
            ("a/package.json", 1),
            ("a/target/x", 10),
            ("a/node_modules/x", 20),
            ("b/target/x", 30),
            ("b/__pycache__/x", 5),
        ]);
        assert_eq!(
            find(&root, "@dev"),
            ["a/node_modules", "a/target", "b/__pycache__"]
        );
    }

    #[test]
    fn a_match_hides_what_lies_inside_it() {
        let root = build(&[
            ("a/package.json", 1),
            ("a/node_modules/x", 5),
            ("a/node_modules/node_modules/y", 7),
            ("a/node_modules/dep/node_modules/z", 9),
        ]);
        let q = Query::parse("@node").unwrap();
        let out = run(&root, b"/r/", &q, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.matches, 1);
        assert_eq!(out.total, 21, "the size of the outer folder only");
    }

    #[test]
    fn results_are_sorted_by_size_then_name() {
        let root = build(&[("b/x", 10), ("a/x", 10), ("c/x", 30), ("d/x", 20)]);
        assert_eq!(find(&root, "x"), ["c/x", "d/x", "a/x", "b/x"]);
    }

    #[test]
    fn a_search_keeps_the_largest_matches_and_counts_all() {
        let mut root = Dir::default();
        for i in 0..MAX_HITS as u64 + 500 {
            root.push(format!("f{i}").as_bytes(), Kind::File, i + 1, i + 1, 0);
        }
        let q = Query::parse("f").unwrap();
        let out = run(&root, b"/r/", &q, false, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.hits.len(), MAX_HITS);
        assert_eq!(out.matches, MAX_HITS as u64 + 500);
        assert_eq!(out.hits[0].size, MAX_HITS as u64 + 500);
        assert_eq!(out.hits[MAX_HITS - 1].size, 501);
        let all: u64 = (1..=MAX_HITS as u64 + 500).sum();
        assert_eq!(out.total, all);
    }

    #[test]
    fn a_cancelled_search_returns_nothing() {
        let root = build(&[("a/x", 1)]);
        let q = Query::parse("x").unwrap();
        assert!(run(&root, b"/r/", &q, false, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn a_last_word_with_at_lists_and_completes_ready_made_searches() {
        assert_eq!(presets_for("@").unwrap().len(), PRESETS.len());
        let names =
            |t: &str| -> Vec<&str> { presets_for(t).unwrap().iter().map(|p| p.name).collect() };
        assert_eq!(names("x @ru"), ["rust"]);
        assert_eq!(names("@d"), ["dev"]);
        assert!(presets_for("@rust").is_none());
        assert!(presets_for("@zz").is_none());
        assert!(presets_for("@ru ").is_none());
        assert!(presets_for("ru").is_none());
        assert_eq!(complete("x @ru").as_deref(), Some("x @rust "));
        assert_eq!(complete("@").as_deref(), None, "no common start");
        assert_eq!(complete("@zz"), None);
    }

    #[test]
    fn the_names_of_the_ready_made_searches_are_unique_and_lower_case() {
        for (i, p) in PRESETS.iter().enumerate() {
            assert_eq!(p.name, p.name.to_lowercase());
            assert!(PRESETS[i + 1..].iter().all(|q| q.name != p.name));
        }
    }

    /// A tree of about `n` entries: folders with folders and files, a few of
    /// which are `node_modules`, `target` and `*.log`.
    fn big_tree(n: u64) -> Dir {
        fn fill(dir: &mut Dir, depth: u32, seed: &mut u64, left: &mut u64) {
            let fanout = if depth == 0 { 40 } else { 14 };
            for i in 0..fanout {
                if *left == 0 {
                    return;
                }
                *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let size = (*seed >> 40) % 3_000_000;
                if depth < 4 && i % 3 == 0 {
                    let name = match *seed >> 60 {
                        0 => "node_modules".to_string(),
                        1 => "target".to_string(),
                        _ => format!("dir{i}"),
                    };
                    let mut sub = Dir::default();
                    if name == "target" {
                        sub.push(b"CACHEDIR.TAG", Kind::File, 1, 1, 0);
                    }
                    fill(&mut sub, depth + 1, seed, left);
                    dir.push(name.as_bytes(), Kind::Dir, 0, 0, 0);
                    let at = dir.entries.len() - 1;
                    dir.attach(at, sub, 0);
                } else {
                    let ext = if i % 5 == 0 { "log" } else { "dat" };
                    dir.push(
                        format!("file{i}.{ext}").as_bytes(),
                        Kind::File,
                        size,
                        size,
                        0,
                    );
                    *left -= 1;
                }
            }
        }
        let mut root = Dir::default();
        let mut seed = 7;
        let mut left = n;
        while left > 0 {
            let before = left;
            let mut sub = Dir::default();
            fill(&mut sub, 1, &mut seed, &mut left);
            let name = format!("top{}", root.entries.len());
            root.push(name.as_bytes(), Kind::Dir, 0, 0, 0);
            let at = root.entries.len() - 1;
            root.attach(at, sub, 0);
            assert!(left < before);
        }
        root
    }

    #[test]
    fn a_small_generated_tree_gives_the_same_result_every_time() {
        let root = big_tree(50_000);
        let first = find(&root, "*.log size>1M");
        assert_ne!(first.len(), 0);
        assert_eq!(first, find(&root, "*.log size>1M"));
    }

    /// Run with `cargo test --release -- --ignored --nocapture a_million`.
    #[test]
    #[ignore = "measures the time of a search over millions of entries"]
    fn a_search_over_millions_of_entries_is_fast() {
        let root = big_tree(3_000_000);
        let items = root.totals().items;
        println!("entries: {items}");
        let stop = AtomicBool::new(false);
        for query in ["node_modules", "@dev", "*.log size>1M", "zzzz"] {
            let q = Query::parse(query).unwrap();
            let mut times = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                let start = Instant::now();
                last = run(&root, b"/r/", &q, false, &stop);
                times.push(start.elapsed());
            }
            times.sort();
            let out = last.unwrap();
            println!(
                "{query:>16}: median {:?}, best {:?}, {} matches",
                times[2], times[0], out.matches
            );
        }
    }
}
