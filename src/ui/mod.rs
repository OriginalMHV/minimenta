mod browser;
mod progress;
mod prompt;
mod trash;
mod view;

use std::io;
use std::path::{Path, PathBuf};

use ratatui::DefaultTerminal;

use crate::scan::{self, Options};
use crate::tree::Tree;
use progress::Outcome;

pub fn run(dir: Option<PathBuf>, opts: Options) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_screens(&mut terminal, dir, opts);
    ratatui::restore();
    result
}

fn run_screens(
    terminal: &mut DefaultTerminal,
    dir: Option<PathBuf>,
    opts: Options,
) -> io::Result<()> {
    let mut prompt = prompt::Prompt::new()?;
    let mut next = dir;
    loop {
        let path = match next.take() {
            Some(path) => path,
            None => match prompt.run(terminal)? {
                Some(path) => path,
                None => return Ok(()),
            },
        };
        match progress::scan(terminal, &path, opts, 0)? {
            Outcome::Done(scan) => {
                let path = path.canonicalize().unwrap_or(path);
                // Rescans inside this tree must count its firmlinks once.
                scan::set_tree_root(&path);
                let tree = Tree {
                    path,
                    dir: Box::new(scan.dir),
                };
                return browser::run(terminal, tree, opts, &scan.source, scan.session);
            }
            Outcome::Cancelled => prompt.set_path(&path),
            Outcome::Quit => return Ok(()),
            Outcome::Failed(e) => {
                prompt.set_path(&path);
                prompt.set_error(format!("Cannot scan {}: {e}", display_path(&path)));
            }
        }
    }
}

/// Shows the home directory as `~` to keep paths short. On Windows it also
/// hides the `\\?\` prefix that `canonicalize` adds for long paths.
pub fn display_path(path: &Path) -> String {
    let shown = path
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|s| !s.starts_with("UNC\\"));
    let path = shown.map_or(path, Path::new);
    match std::env::home_dir() {
        Some(home) if path.starts_with(&home) && home != Path::new("/") => {
            let rest = path.strip_prefix(&home).unwrap_or(path);
            if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            }
        }
        _ => path.display().to_string(),
    }
}
