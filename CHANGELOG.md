# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

minimenta has no release yet. The Unreleased section lists what version 0.1.0 contains. The Changed and Fixed lists describe changes since earlier builds from the main branch. They matter to you only if you built minimenta from source before 0.1.0.

## [Unreleased]

### Added

- An interactive disk usage analyzer for the terminal. It runs on macOS, Linux and Windows. The command is `minimenta`, and `mm` is a short name for the same program.
- A start prompt. Run `minimenta` without a folder. The prompt shows the current folder. Enter scans it, Tab completes folder names, and Ctrl+U clears the line.
- The ncdu look and keys. The browser has the same layout, size bars, file flags and sort keys. Press `s`, `n` or `C` to sort by size, name or item count. Press `a` to switch between apparent size and disk usage. Press `r` to rescan the current folder. Press `?` for help.
- Multi-select. Space selects one item. Shift+Up and Shift+Down (or `K` and `J`) select a range. Ctrl+A selects all items in the folder. Esc clears the selection.
- Delete with a safety net. Press `d` to move the selection to the Trash. Press `D` to delete it for good. Both keys ask for confirmation first. On macOS, minimenta asks Finder to move the items, so "Put Back" works.
- Undo. Press `u` to put the last move to the Trash back. Each further `u` undoes the move before it, until you quit.
- A fast first scan. minimenta uses at least 16 threads. On macOS, it reads directories in bulk with `getattrlistbulk(2)`. On NTFS volumes, it reads the master file table when you run it as administrator. On a cold disk, minimenta scans about 5.5x faster than ncdu with its default settings on macOS and about 4.8x faster on Linux. The README shows the method and the cases where ncdu is as fast or faster.
- Fast repeat scans on macOS. minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta`. It lists again only the folders that FSEvents reports as changed. The browser says when it shows a cached scan.
- These options: `-x` (stay on one file system), `-t N` (number of scan threads), `--no-cache`, `--no-mft`, `--cache`, `--summary` (print the totals without the interface), `-h` and `-V`.
- Prebuilt binaries for macOS (Apple silicon and Intel), Linux (x86-64 and ARM64) and Windows (x86-64). Both commands are in each archive.
- Installation with Homebrew, `cargo install`, and installer scripts for macOS, Linux and Windows.
- A hint to run as administrator on Windows. After a slow scan (5 s or more) of an NTFS volume, minimenta suggests an elevated run. This happens when UAC limits your administrator account and minimenta could not read the master file table. A standard user and an elevated process do not see the hint. `--summary` prints the hint on stderr.

### Changed

- Linux: a folder with hundreds of thousands of files scans faster. minimenta splits the file stat calls for the later parts of such a folder across threads. Before, one thread did all of them. Esc stops the listing of such a folder, and the memory that the listing uses stays bounded. Scans of ordinary folders do not change.
- Linux: a cold scan reads the folders in random order. Before, the threads followed the tree and read neighbouring folders at the same time, and the disk served fewer requests per second. A scan that keeps the CPUs busy, such as a scan of a cached tree, does not change. On GitHub runners with a virtual disk, a cold scan of `/usr` took 0.92x (SCSI disk) and 0.89x (NVMe disk) of the time of the earlier scanner. It took 0.95x and 0.92x of the time of `ncdu -t 64`. Those A/B runs timed both scanners in alternating pairs in one job. They read `/usr` once with the earlier scanner before the timing, and they waited for a quiet disk before every scan. The Speed workflow also reads the tree before it times, in its totals check and its single runs, but it does not wait for a quiet disk. The Speed runs on main at `87550d8` do not show this lead over `ncdu -t 64`. They show the two even on a cold `/usr` (ratio 1.016, middle half 0.98 to 1.06). The two harnesses differ, and we do not know why the results differ. We did not measure spinning disks, network drives or local NVMe disks. Random order may cost extra seeks on a spinning disk, and `--no-spread` turns it off. The peak memory of a scan of `/usr` did not grow (251 MB against 252 MB at most). The peak number of open files grew by at most 1 (22 against 21).
- macOS: a scan skips empty folders on the system volume without opening them. This makes a scan of `/System/Library` faster. An empty system folder that you may not read is no longer reported as a read error, because minimenta never opens it. Scans of the data volume do not change.
- macOS: a repeat scan lists folders with hard-linked files again only when links may have changed. Before, it listed them every time, which made repeat scans of trees such as `Xcode.app` slow.
- macOS: a repeat scan decodes the cache while it reads the FSEvents history. This makes repeat scans of large trees faster.
- Windows: when you run minimenta as administrator on a clearly cold NTFS disk, the master file table reader starts after 50 ms. Before, it started after 250 ms. Warm scans and small scans do not change.
- Windows: minimenta loads the system libraries for the Recycle Bin only when it needs them. Start-up is faster, which helps small scans most.

### Fixed

- macOS: a volume that is mounted over a folder with entries is now listed completely. Before, the scan could stop after the first part of the listing and miss files.
- macOS: a scan that crosses firmlinks counts the data volume once. Before, a scan of `/` counted every file behind a firmlink such as `/Users` twice, and `-x` followed the firmlinks into the data volume. Now `/System/Volumes/Data/Users` shows `>` and its contents show at `/Users`. With `-x`, a firmlink counts as a mount point. A rescan with `r` and the cache follow the same rule. A scan of `/` is shorter and shows a lower total, because it does less work. This is a correction and not a faster scanner.
