# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

minimenta has no release yet. The Unreleased section lists what version 0.1.0 contains.

## [Unreleased]

### Added

- An interactive disk usage analyzer for the terminal. It runs on macOS, Linux and Windows. The command is `minimenta`, and `mm` is a short name for the same program.
- A start prompt. Run `minimenta` without a folder. The prompt shows the current folder. Enter scans it, Tab completes folder names, and Ctrl+U clears the line.
- The ncdu look and keys. The browser has the same layout, size bars, file flags and sort keys. Press `s`, `n` or `C` to sort by size, name or item count. Press `a` to switch between apparent size and disk usage. Press `r` to rescan the current folder. Press `?` for help.
- Multi-select. Space selects one item. Shift+Up and Shift+Down (or `K` and `J`) select a range. Ctrl+A selects all items in the folder. Esc clears the selection.
- Delete with a safety net. Press `d` to move the selection to the Trash. Press `D` to delete it for good. Both keys ask for confirmation first. On macOS, minimenta asks Finder to move the items, so "Put Back" works.
- Undo. Press `u` to put the last move to the Trash back. Each further `u` undoes the move before it, until you quit.
- A fast first scan. minimenta uses at least 16 threads. On macOS, it reads directories in bulk with `getattrlistbulk(2)`. On NTFS volumes, it reads the master file table when you run it as administrator. On a cold disk, minimenta scans about 4x to 5x faster than ncdu with its default settings. The README shows the method and the cases where ncdu is as fast or faster.
- Fast repeat scans on macOS. minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta`. It lists again only the folders that FSEvents reports as changed. The browser says when it shows a cached scan.
- These options: `-x` (stay on one file system), `-t N` (number of scan threads), `--no-cache`, `--no-mft`, `--cache`, `--summary` (print the totals without the interface), `-h` and `-V`.
- Prebuilt binaries for macOS (Apple silicon and Intel), Linux (x86-64 and ARM64) and Windows (x86-64). Both commands are in each archive.
- Installation with Homebrew, `cargo install`, and installer scripts for macOS, Linux and Windows.
