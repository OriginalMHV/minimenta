use std::cmp::Ordering;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

pub mod flag {
    /// The directory could not be read completely.
    pub const ERROR: u8 = 1;
    /// Something below this directory could not be read.
    pub const SUB_ERROR: u8 = 2;
    /// Another hard link to the same inode was counted first.
    pub const HARDLINK: u8 = 4;
    /// A mount point that `--one-file-system` skipped.
    pub const OTHER_FS: u8 = 8;
    pub const SELECTED: u8 = 16;
    /// Set during an incremental update on entries whose totals must be recomputed.
    pub const DIRTY: u8 = 32;
    /// A file with more than one hard link. The cache lists its directory again
    /// on every load, so all links are counted once in the same pass.
    pub const MULTI_LINK: u8 = 64;
    /// The flags worth keeping in the cache file.
    pub const PERSISTENT: u8 = ERROR | SUB_ERROR | HARDLINK | OTHER_FS | MULTI_LINK;
}

/// Names live in the parent's `Dir::names` buffer, so a scan allocates once
/// per directory instead of once per file.
pub struct Entry {
    pub(crate) name_start: u32,
    pub(crate) name_len: u32,
    pub disk: u64,
    pub apparent: u64,
    pub items: u64,
    pub kind: Kind,
    pub flags: u8,
    pub dir: Option<Box<Dir>>,
}

impl Entry {
    /// Unique within the parent directory and stable across sorting.
    pub fn id(&self) -> u32 {
        self.name_start
    }

    pub fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }

    pub fn counted_disk(&self) -> u64 {
        if self.has(flag::HARDLINK) {
            0
        } else {
            self.disk
        }
    }

    pub fn counted_apparent(&self) -> u64 {
        if self.has(flag::HARDLINK) {
            0
        } else {
            self.apparent
        }
    }

    pub fn size(&self, apparent: bool) -> u64 {
        if apparent { self.apparent } else { self.disk }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SortKey {
    #[default]
    Size,
    Name,
    Items,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Sort {
    pub key: SortKey,
    pub reverse: bool,
    pub apparent: bool,
}

#[derive(Default)]
pub struct Dir {
    pub names: Vec<u8>,
    pub entries: Vec<Entry>,
    /// The order the entries are in. `None` means the order is stale.
    pub sort: Option<Sort>,
    /// The blocks of the directory itself, in bytes. `du` counts them on
    /// Linux. The macOS scanner leaves them at 0.
    pub own_disk: u64,
    /// The inode of this directory, or 0 when unknown. An incremental update
    /// reuses a cached subtree only when the name and the inode both match.
    pub id: u64,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Totals {
    pub disk: u64,
    pub apparent: u64,
    pub items: u64,
}

impl Dir {
    pub fn name(&self, e: &Entry) -> &[u8] {
        let start = e.name_start as usize;
        &self.names[start..start + e.name_len as usize]
    }

    pub fn push(&mut self, name: &[u8], kind: Kind, disk: u64, apparent: u64, flags: u8) {
        let name_start = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.entries.push(Entry {
            name_start,
            name_len: name.len() as u32,
            disk,
            apparent,
            items: 1,
            kind,
            flags,
            dir: None,
        });
    }

    pub fn find(&self, name: &[u8]) -> Option<usize> {
        self.entries.iter().position(|e| self.name(e) == name)
    }

    /// The sizes of the entries plus the blocks of the directory itself.
    pub fn totals(&self) -> Totals {
        let own = Totals {
            disk: self.own_disk,
            ..Totals::default()
        };
        self.entries.iter().fold(own, |t, e| Totals {
            disk: t.disk + e.counted_disk(),
            apparent: t.apparent + e.counted_apparent(),
            items: t.items + e.items,
        })
    }

    pub fn has_error(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.has(flag::ERROR | flag::SUB_ERROR))
    }

    /// Stores a scanned subdirectory in entry `i` and sets the entry's sizes
    /// to the totals of `sub`, which include `sub.own_disk`.
    pub fn attach(&mut self, i: usize, sub: Dir, flags: u8) {
        let t = sub.totals();
        let e = &mut self.entries[i];
        e.disk = t.disk;
        e.apparent = t.apparent;
        e.items = 1 + t.items;
        e.flags |= flags;
        if sub.has_error() {
            e.flags |= flag::SUB_ERROR;
        }
        e.dir = Some(Box::new(sub));
    }

    pub fn sort(&mut self, sort: Sort) {
        let names = &self.names;
        let name = |e: &Entry| {
            let start = e.name_start as usize;
            &names[start..start + e.name_len as usize]
        };
        let by_name = |a: &Entry, b: &Entry| name(a).cmp(name(b));
        self.entries.sort_unstable_by(|a, b| {
            let primary = match sort.key {
                SortKey::Size => b.size(sort.apparent).cmp(&a.size(sort.apparent)),
                SortKey::Items => b.items.cmp(&a.items),
                SortKey::Name => Ordering::Equal,
            };
            let order = primary.then_with(|| by_name(a, b));
            if sort.reverse { order.reverse() } else { order }
        });
        self.sort = Some(sort);
    }
}

pub struct Tree {
    pub path: PathBuf,
    pub dir: Box<Dir>,
}

impl Tree {
    pub fn dir_at(&self, stack: &[usize]) -> &Dir {
        stack.iter().fold(&self.dir, |d, &i| {
            d.entries[i]
                .dir
                .as_ref()
                .expect("stack points to a directory")
        })
    }

    pub fn dir_at_mut(&mut self, stack: &[usize]) -> &mut Dir {
        let mut d = &mut self.dir;
        for &i in stack {
            d = d.entries[i]
                .dir
                .as_mut()
                .expect("stack points to a directory");
        }
        d
    }

    /// Recomputes the totals of every directory on `stack` after a change below
    /// it. The sizes of the ancestors change, so their order becomes stale.
    pub fn refresh_totals(&mut self, stack: &[usize]) {
        fn refresh(dir: &mut Dir, stack: &[usize]) {
            let Some((&i, rest)) = stack.split_first() else {
                return;
            };
            dir.sort = None;
            let e = &mut dir.entries[i];
            let sub = e.dir.as_mut().expect("stack points to a directory");
            refresh(sub, rest);
            let t = sub.totals();
            e.disk = t.disk;
            e.apparent = t.apparent;
            e.items = 1 + t.items;
            e.flags &= !flag::SUB_ERROR;
            if sub.has_error() {
                e.flags |= flag::SUB_ERROR;
            }
        }
        refresh(&mut self.dir, stack);
    }
}

const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes:>5}   B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1023.95 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:>5.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units_with_one_decimal() {
        assert_eq!(format_size(0), "    0   B");
        assert_eq!(format_size(1023), " 1023   B");
        assert_eq!(format_size(1024), "  1.0 KiB");
        assert_eq!(format_size(1536 * 1024), "  1.5 MiB");
        assert_eq!(format_size(1024 * 1024 - 1), "  1.0 MiB");
    }

    #[test]
    fn hard_links_are_shown_but_not_counted() {
        let mut dir = Dir::default();
        dir.push(b"a", Kind::File, 4096, 10, 0);
        dir.push(b"b", Kind::File, 4096, 10, flag::HARDLINK);
        assert_eq!(
            dir.totals(),
            Totals {
                disk: 4096,
                apparent: 10,
                items: 2
            }
        );
    }

    #[test]
    fn refreshing_an_unchanged_tree_keeps_directory_blocks() {
        let mut leaf = Dir {
            own_disk: 4096,
            ..Dir::default()
        };
        leaf.push(b"f", Kind::File, 8192, 5000, 0);
        let mut mid = Dir {
            own_disk: 4096,
            ..Dir::default()
        };
        mid.push(b"leaf", Kind::Dir, 0, 0, 0);
        mid.attach(0, leaf, 0);
        let mut root = Dir {
            own_disk: 4096,
            ..Dir::default()
        };
        root.push(b"mid", Kind::Dir, 0, 0, 0);
        root.attach(0, mid, 0);
        let mut tree = Tree {
            path: PathBuf::from("/"),
            dir: Box::new(root),
        };
        let before = tree.dir.totals();

        tree.refresh_totals(&[0, 0]);

        assert_eq!(before.disk, 3 * 4096 + 8192);
        assert_eq!(tree.dir.totals(), before);
        assert_eq!(tree.dir.entries[0].disk, 2 * 4096 + 8192);
    }

    #[test]
    fn refreshing_totals_marks_ancestor_order_as_stale() {
        let mut sub = Dir::default();
        sub.push(b"big", Kind::File, 100, 100, 0);
        let mut root = Dir::default();
        root.push(b"a", Kind::Dir, 0, 0, 0);
        root.attach(0, sub, 0);
        root.sort(Sort::default());
        let mut tree = Tree {
            path: PathBuf::from("/"),
            dir: Box::new(root),
        };

        tree.dir_at_mut(&[0]).entries.clear();
        tree.refresh_totals(&[0]);

        assert_eq!(tree.dir.sort, None);
        assert_eq!(tree.dir.entries[0].disk, 0);
        assert_eq!(tree.dir.entries[0].items, 1);
    }

    #[test]
    fn sorting_by_size_breaks_ties_by_name() {
        let mut dir = Dir::default();
        dir.push(b"b", Kind::File, 10, 1, 0);
        dir.push(b"a", Kind::File, 10, 1, 0);
        dir.push(b"c", Kind::File, 99, 1, 0);
        dir.sort(Sort::default());
        let names: Vec<_> = dir.entries.iter().map(|e| dir.name(e).to_vec()).collect();
        assert_eq!(names, [b"c".to_vec(), b"a".to_vec(), b"b".to_vec()]);
    }
}
