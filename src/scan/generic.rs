use std::ffi::CStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use super::{Ctx, as_path};
use crate::tree::{Dir, Kind, flag};

pub(super) fn read_dir(
    ctx: &Ctx,
    path: &CStr,
    dir: &mut Dir,
    subdirs: &mut Vec<usize>,
) -> io::Result<()> {
    for entry in fs::read_dir(as_path(path))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.as_bytes();
        let Ok(meta) = entry.metadata() else {
            dir.push(name, Kind::Other, 0, 0, flag::ERROR);
            continue;
        };
        let file_type = meta.file_type();
        if file_type.is_dir() {
            if ctx.one_fs && meta.dev() != ctx.root_dev {
                dir.push(name, Kind::Dir, 0, 0, flag::OTHER_FS);
            } else {
                subdirs.push(dir.entries.len());
                dir.push(name, Kind::Dir, 0, 0, 0);
            }
            continue;
        }
        let kind = if file_type.is_file() {
            Kind::File
        } else if file_type.is_symlink() {
            Kind::Symlink
        } else {
            Kind::Other
        };
        let shared = meta.nlink() > 1 && !ctx.first_link(meta.dev(), meta.ino());
        let flags = if shared { flag::HARDLINK } else { 0 };
        dir.push(name, kind, meta.blocks() * 512, meta.len(), flags);
    }
    Ok(())
}
