# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-11-01

### Added

- Search with `/`. It reads the scan in memory and runs while you type. Words can be a name, a pattern like `*.log`, a path like `cache/`, `size>1G`, `type:dir` and `-word`. Ready-made searches find build folders that grow large: `@node`, `@rust`, `@gradle`, `@python`, `@xcode`, `@cache` and `@dev`. Each result shows its path below the open folder. You can select results and delete them with `d` or `D`.
- macOS: when minimenta cannot use the cache, the progress screen gives the reason on a second line under the message. Examples are a last full scan older than 7 days, a replay that did not finish in its time limit, and more than 20,000 changes. For too many changes, the line shows the number of changes, the number of different folders among them, and the limit.
- The footer shows the free space of the disk that holds the open folder, for example `182.3 GiB free  1/2`. It stays while a message or the delete progress shows. On a narrow line it drops first, so the percent and the time left stay. minimenta reads it at start, after a delete or a rescan, when the open folder changes, and every 2 s while deletes run. After `D`, the message adds the space it freed, for example `Deleted 1 item. Freed 12.4 GiB.` After `d` it does not, because the Trash keeps the files until you empty it.

### Changed

- A delete (`d` or `D`) runs in the background. The rows leave the list at once, so you can keep browsing and queue more deletes. `q` waits for the running deletes, and `q` again quits at once. `Esc` stays as it was. `r` and `u` wait until the deletes end. A quick repeat of the same delete key is ignored when the question is off.
- `D` deletes folders with 8 threads. It is about 2.5x faster on Linux, 1.5x on Windows and gains little on macOS. It never follows links or junctions, and on macOS and Linux it does not enter a folder on another device.
- While a delete runs, the footer shows its progress. For `D` it shows the items removed, the percent and the time left, for example `Deleting big: 21% done, about 40 s left (21,340 of 100,000 items). 2 more queued (45,000 items)`. For `d` it shows how long the move to the Trash has run. The time left shows after about a second. When the count stops for 3 s, the footer says `waiting for the disk`. The text changes while you wait, with no key press.

## [0.1.0] - 2026-10-10

The Changed and Fixed lists describe changes since earlier builds from the main branch. They matter to you only if you built minimenta from source before 0.1.0.

### Added

- An interactive disk usage analyzer for the terminal. It runs on macOS, Linux and Windows. The command is `minimenta`, and `mm` is a short name for the same program.
- A start prompt. Run `minimenta` without a folder. The prompt shows the current folder. Enter scans it, Tab completes folder names, and Ctrl+U clears the line.
- The ncdu look and keys. The browser has the same layout, size bars, file flags and sort keys. Press `s`, `n` or `C` to sort by size, name or item count. Press `a` to switch between apparent size and disk usage. Press `r` to rescan the current folder. Press `?` for help.
- Multi-select. Space selects one item. Shift+Up and Shift+Down (or `K` and `J`) select a range. Ctrl+A selects all items in the folder. Esc clears the selection.
- Delete with a safety net. Press `d` to move the selection to the Trash. Press `D` to delete it for good. Both keys ask for confirmation first. In the question, Shift+A confirms and stops the question of that key until you quit. The help screen shows a question that is off, and `t` and `p` turn it back on. On macOS, minimenta asks Finder to move the items, so "Put Back" works.
- Undo. Press `u` to put the last move to the Trash back. Each further `u` undoes the move before it, until you quit.
- A fast first scan. minimenta uses at least 16 threads. On macOS, it reads directories in bulk with `getattrlistbulk(2)`. On NTFS volumes, it reads the master file table when you run it as administrator. On a cold disk, minimenta scans about 5.1x faster than ncdu with its default settings on macOS and about 4.9x faster on Linux. Against `ncdu -t 64` on Linux, the lead is small. The README shows the method and the cases where ncdu is as fast or faster.
- Fast repeat scans on macOS. minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta`. It lists again only the folders that FSEvents reports as changed. The browser says when it shows a cached scan.
- These options: `-x` (stay on one file system), `-t N` (number of scan threads), `--no-cache`, `--no-mft`, `--cache`, `--summary` (print the totals without the interface), `-h` and `-V`.
- Prebuilt binaries for macOS (Apple silicon and Intel), Linux (x86-64 and ARM64) and Windows (x86-64). Both commands are in each archive.
- Installation with Homebrew, `cargo install`, and installer scripts for macOS, Linux and Windows.
- A hint to run as administrator on Windows. After a slow scan (5 s or more) of an NTFS volume, minimenta suggests an elevated run. This happens when UAC limits your administrator account and minimenta could not read the master file table. A standard user and an elevated process do not see the hint. `--summary` prints the hint on stderr.
- An estimate of the time left on the progress screen. minimenta takes it from the item count of an earlier scan of the same folder and the speed of the scan so far. A rescan with `r` has that count on every platform. On macOS, the cache file stores the count. A full scan after a failed cache check shows an estimate when the cache is from the same folder and options. A first scan has no earlier count and shows no estimate. The estimate shows after 2 s of counting and 1% of the earlier count.

### Changed

- Linux: a folder with hundreds of thousands of files scans faster. minimenta splits the file stat calls for the later parts of such a folder across threads. Before, one thread did all of them. Esc stops the listing of such a folder, and the memory that the listing uses stays bounded. Scans of ordinary folders do not change.
- Linux: a cold scan reads the folders in random order. Before, the threads followed the tree and read neighbouring folders at the same time, and the disk served fewer requests per second. A scan that keeps the CPUs busy, such as a scan of a cached tree, does not change. On GitHub runners with a virtual disk, a cold scan of `/usr` took 0.92x (SCSI disk) and 0.89x (NVMe disk) of the time of the earlier scanner. It took 0.95x (SCSI) and 0.92x (NVMe) of the time of `ncdu -t 64`. By the rule of the README tables, that is a small lead on SCSI and even on NVMe. Those A/B runs timed both scanners in alternating pairs in one job. The first three Speed runs on main at `87550d8` (one Linux job each) showed the two even on a cold `/usr`. The ratio was 1.016, the median of the three runs (middle half of that run 0.98 to 1.06). The 21 pairs of the three runs together gave 1.019. A later test ran both timing methods in the same 12 jobs (131 pairs each). It was not the real Speed workflow. It gave 1.056 for the Speed method and 1.057 for the A/B method. The median ratio differed by runner CPU model and disk type together (3 Intel NVMe jobs against 9 EPYC 7763 SCSI jobs). We did not test either factor. Then three Speed runs at `a700ff0`, with six pooled Linux jobs each, showed a small lead over `ncdu -t 64` on a cold `/usr`. The median of the 18 job medians is 1.036, and the 95 percent interval is 1.002 to 1.066. The median of the job medians of each run is above 1 (1.039, 1.012 and 1.037). So the rule of the README tables names minimenta. minimenta was faster in 14 of the 18 jobs, and the job medians range from 0.957 to 1.131. The pooled pairs of the same runs are even by the rule of the pair table (ratio 1.046, middle half 1.00 to 1.11, 217 pairs). The ratio 1.056 of the test above is inside the interval. We did not prove why the earlier results differ. We did not measure spinning disks, network drives or local NVMe disks. Random order may cost extra seeks on a spinning disk, and `--no-spread` turns it off. The peak memory of a scan of `/usr` did not grow (251 MB against 252 MB at most). The peak number of open files grew by at most 1 (22 against 21).
- macOS: a scan skips empty folders on the system volume without opening them. This makes a scan of `/System/Library` faster. An empty system folder that you may not read is no longer reported as a read error, because minimenta never opens it. Scans of the data volume do not change.
- macOS: a repeat scan lists folders with hard-linked files again only when links may have changed. Before, it listed them every time, which made repeat scans of trees such as `Xcode.app` slow.
- macOS: a repeat scan decodes the cache while it reads the FSEvents history. This makes repeat scans of large trees faster.
- Windows: when you run minimenta as administrator on a clearly cold NTFS disk, the master file table reader starts after 50 ms. Before, it started after 250 ms. Warm scans and small scans do not change.
- Windows: minimenta loads the system libraries for the Recycle Bin only when it needs them. Start-up is faster, which helps small scans most.
- macOS: the cache file stores the item count of the saved tree. The file format changed (version 5), so the first scan after the update scans all folders.

### Fixed

- macOS: a volume that is mounted over a folder with entries is now listed completely. Before, the scan could stop after the first part of the listing and miss files.
- macOS: the progress screen says when minimenta checks the cache. Before, it showed 0 items and 0 B while minimenta read the cache and FSEvents replayed the changes. The replay can take up to half the time of the last full scan. For a home folder with a full scan of 35 s, that is up to 17 s. During the replay, the Time line shows the latest time at which the replay stops. That time includes the time to read the cache. If minimenta cannot use the cache, the screen says that it scans all folders again, and the counters start from 0. Before, the counters counted some folders twice when the check had listed them before it stopped.
- macOS: a scan that crosses firmlinks counts the data volume once. Before, a scan of `/` counted every file behind a firmlink such as `/Users` twice, and `-x` followed the firmlinks into the data volume. Now `/System/Volumes/Data/Users` shows `>` and its contents show at `/Users`. With `-x`, a firmlink counts as a mount point. A rescan with `r` and the cache follow the same rule. A scan of `/` is shorter and shows a lower total, because it does less work. This is a correction and not a faster scanner.

[Unreleased]: https://github.com/OriginalMHV/minimenta/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/OriginalMHV/minimenta/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/OriginalMHV/minimenta/releases/tag/v0.1.0
