<p align="center">
  <img src="docs/assets/minimenta-hero.svg" alt="minimenta. Minuere impedimenta. Find what fills your disk, and let it go." width="100%">
</p>

<p align="center">
  <a href="https://github.com/OriginalMHV/minimenta/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/OriginalMHV/minimenta/ci.yml?branch=main&style=flat-square&labelColor=59636E&color=1A7F5A&label=CI" alt="CI status"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-2024%20edition-1A7F5A?style=flat-square&labelColor=59636E" alt="Rust 2024 edition"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-1A7F5A?style=flat-square&labelColor=59636E" alt="Runs on macOS, Linux and Windows">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-1A7F5A?style=flat-square&labelColor=59636E" alt="MIT or Apache-2.0 license"></a>
</p>

minimenta is an interactive disk usage analyzer for the terminal, written in Rust. It looks and feels like ncdu, scans faster, and lets you select many items and move them to the Trash in one step.

*Minuere impedimenta* is Latin for "reduce the baggage". *Impedimenta* was the baggage train of a Roman army: everything that slowed it down.

## Why minimenta

- **A start prompt.** Run `minimenta` without a folder. The prompt shows the current folder. Enter scans it, and Tab completes folder names.
- **Multi-select.** Space selects one item. Shift+Up/Down (or `K`/`J`) selects a range. Ctrl+A selects all items in the folder, and Esc clears the selection.
- **The Trash first.** `d` moves the selection to the Trash. On macOS, minimenta asks Finder to do it, so "Put Back" works. If Finder cannot be controlled, minimenta uses the file manager API instead. `D` deletes permanently. Both keys ask for confirmation first. `u` puts the last move to the Trash back, and each further `u` undoes the move before it, until you quit.
- **The ncdu look and keys.** The same layout, size bars, file flags, sort keys and `-x` option. The keys that both tools have work the same, except `d`, which moves items to the Trash.
- **A fast first scan.** At least 16 threads and, on macOS, bulk directory reads with `getattrlistbulk(2)`. On a cold disk, minimenta scans about 4.4x faster than ncdu with its defaults on macOS and Linux. See [Speed](#speed).
- **Fast repeat scans on macOS.** minimenta keeps the last scan and lists again only the folders that FSEvents reports as changed.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Terminal recording: the start prompt scans the home folder, Downloads opens, J (the same as Shift+Down) selects three large files, D and y delete them, and the Downloads total drops from 4.0 GiB to 782 MiB" width="100%">
</p>

## Install

```sh
cargo install --git https://github.com/OriginalMHV/minimenta
```

This installs two commands: `minimenta` and its short name `mm`. minimenta runs on macOS, Linux and Windows. Building it needs a recent stable Rust toolchain and a C compiler (for the mimalloc allocator). To build without mimalloc, add `--no-default-features`.

## Usage

```sh
mm                           # ask which folder to scan
mm ~/code                    # scan ~/code at once
mm --summary ~/code          # print the totals without the interface
```

`mm` and `minimenta` are the same program.

1. The prompt shows the current folder. Press Enter to scan it, or type another path. Tab completes folder names and Ctrl+U clears the line.
2. The browser lists the items in the folder, largest first. Open a folder with Enter and go back with Left.
3. Select the items you do not need, then press `d` to move them to the Trash or `D` to delete them permanently.

Press Esc during a scan to cancel it, or `q` to quit.

## Keys

| Keys | Action |
| --- | --- |
| Up, `k` / Down, `j` | Move the cursor |
| Shift+Up/Down, `K` / `J` | Extend the selection |
| Space | Select or deselect, then move down |
| Ctrl+A / Esc | Select all / clear the selection |
| Enter, Right, `l` | Open the folder |
| Left, `h`, Backspace | Go to the parent folder |
| `d` | Move the selection (or the item under the cursor) to the Trash |
| `D` | Delete permanently |
| `u` | Undo the last move to the Trash (press again for the move before it) |
| `s` / `n` / `C` | Sort by size / name / items (press again to reverse) |
| `a` | Show apparent size or disk usage |
| `r` | Rescan the current folder |
| `?` | Help |
| `q` | Quit |

## Options

| Option | Effect |
| --- | --- |
| `-x`, `--one-file-system` | Do not cross file system boundaries |
| `-t N`, `--threads N` | Use N scan threads (default: the number of CPU cores, at least 16) |
| `--no-cache` | Scan everything, and do not read or write the cache (macOS) |
| `--no-mft` | Do not read the NTFS master file table (Windows) |
| `--cache` | Use the cache together with `--summary`, which scans everything by default |
| `--summary` | Scan, print the totals, and exit |
| `-h`, `--help` | Print the help |
| `-V`, `--version` | Print the version |

## Speed

You usually open a disk analyzer because the disk is full. Many folders have not been read for a long time, so the first scan finds a cold disk. That scan is the one that matters most, so this section shows it first. Scans with a warm cache and repeat scans on macOS follow.

The chart, the two speed tables and the full results come from one run of the Speed workflow: [run 37904223270](https://github.com/OriginalMHV/minimenta/actions/runs/37904223270) on 2026-10-09 (commit `1dfc586`). The run used GitHub-hosted runners. Every other number on this page names its own run.

<p align="center">
  <img src="docs/assets/speed.svg" alt="First scan of a cold disk. Each value shows how many times faster minimenta is than that tool. A longer bar is a bigger lead. macOS, 3 cores, /opt/homebrew, 200,140 items: ncdu 4.4x, ncdu -t 64 1.67x, gdu 2.5x, dua-cli 1.70x, dust 2.8x. Linux, 4 cores, /usr, 737,881 items: ncdu 4.4x, ncdu -t 64 0.98x (even, ncdu slightly faster), gdu 1.21x, dua-cli 1.50x, dust 1.42x. Windows, 4 cores, C:\Program Files, 304,060 items, as administrator: ncdu and ncdu -t 64 do not run on Windows, gdu 4.4x, dua-cli 4.3x, dust 8.0x. Without administrator rights, minimenta takes 3.4x as long (--no-mft). Values close to 1.0x are even. Below 1.0x, the other tool is faster. Each value is the median of tool time divided by minimenta time. The operating system file cache is dropped before every run, and the cloud host cache may still be warm. dust prints a tree and is not interactive. macOS also ran on /System/Library (427,459 items). Measured on GitHub runners on 2026-10-09 in Speed workflow run 37904223270. Versions: minimenta 0.1.0, gdu 5.38.0, dua-cli 2.45.1, dust 1.2.6, ncdu 2.9.2 (macOS) and 2.9.1 (Linux)." width="600">
</p>

Each value is a ratio: the time of the other tool divided by the time of minimenta. The tool and minimenta run in alternating pairs, and the value is the median of the pairs. A value above 1.0x means that minimenta is faster. A value below 1.0x means that the other tool is faster. A result is *even* when the middle half of the pairs includes 1.0x. Values are rounded to the nearest 0.1x from 2.0x up and to the nearest 0.01x below 2.0x. The exact values are in the [full results](#full-results).

### First scan of a cold disk

| Tree | ncdu | ncdu -t 64 | gdu | dua-cli | dust |
| --- | ---: | ---: | ---: | ---: | ---: |
| macOS, `/opt/homebrew` (200,140 items, 15 pairs) | 4.4x | 1.67x | 2.5x | 1.70x | 2.8x |
| macOS, `/System/Library` (427,459 items, 11 pairs) | 4.5x | 1.96x | 2.6x | 1.90x | 2.5x |
| Linux, `/usr` (737,881 items, 7 pairs) | 4.4x | 0.98x, even | 1.21x | 1.50x | 1.42x |
| Windows, `C:\Program Files` (304,060 items, 8 pairs), as administrator | n/a | n/a | 4.4x | 4.3x | 8.0x |

- **ncdu** runs with its default settings (1 thread). **ncdu -t 64** is its fastest cold setting. ncdu does not run on Windows.
- **Linux:** `ncdu -t 64` and minimenta are even. The median is 0.98x, the middle half is 0.96x to 1.01x, and ncdu was faster in 5 of 7 pairs.
- **macOS:** the data volume (`/opt/homebrew`) is the main result. On `/System/Library`, minimenta skips empty folders without opening them. That works only on the system volume, and the other tools open every folder, so this tree favors minimenta.
- **Windows:** the runner is an administrator, so minimenta reads the NTFS master file table (MFT). Without administrator rights, minimenta lists directories instead. `minimenta --no-mft` shows that case. It took 13.5 s on `C:\Program Files`, which is 3.4x as long as the administrator scan (4.1 s). In the same run, gdu took 18.2 s and dua-cli took 17.6 s. Each tool ran in its own pairs, so these times are not a paired comparison.
- **Microsoft Defender** real-time monitoring is off on the runner, and `C:\` and `D:\` are excluded. Most Windows PCs run it, so expect smaller differences there.
- **macOS cold runs are short.** minimenta needs about 1.0 s for `/opt/homebrew` after `purge`. Read these runs as "after purge", not as raw disk speed.
- **dust** prints a tree and is not interactive. **dua-cli** exits with code 1 on `/System/Library`. minimenta reports 10 read errors there, so unreadable folders are the likely cause.

### Scan with a warm cache

The tree is the synthetic tree from [`bench/gen_tree.py`](bench/gen_tree.py): 51,110 items, 1,000 folders with 50 empty files each. The operating system cache already holds it. Each tool ran 60 pairs.

| Platform | ncdu | ncdu -t N | gdu | dua-cli | dust |
| --- | ---: | ---: | ---: | ---: | ---: |
| macOS (`-t 3`) | 3.2x | 1.27x | 2.0x | 1.15x, even | 1.96x |
| Linux (`-t 4`) | 2.5x | 0.96x, even | 1.73x | 3.3x | 1.63x |
| Windows, as administrator | n/a | n/a | 2.7x | 2.7x | 24.0x |

- **ncdu -t N** uses one thread for each core of the runner.
- **Linux:** `ncdu -t 4` and minimenta are even. The median is 0.96x, the middle half is 0.92x to 1.05x, and ncdu was faster in 41 of 60 pairs. In earlier experiments (runs 37848716077 to 37851433967), the system allocator was faster than mimalloc on this small tree, and mimalloc was faster on large trees with a warm cache. Large trees are the common case, so minimenta keeps mimalloc.
- **macOS:** dua-cli and minimenta are even. The middle half is 0.78x to 1.74x, because the macOS runner is noisy.
- **Windows:** `minimenta --no-mft` and minimenta are even (0.99x). This scan takes about 32 ms, which is less than the 50 ms that pass before the MFT reader can start. dust needed 767 ms against 32 ms for minimenta. We did not look into why.

### Repeat scans on macOS

A repeat scan reads the cache and lists again only the folders that FSEvents reports as changed. It does not scan the disk again, so the numbers do not compare with the first scans above. They are not part of run 37904223270.

- On a 10-core Mac with Microsoft Defender, a repeat scan of trees with 210,000 and 413,000 items takes 0.05 s instead of 2 to 4 s for a full scan. This was measured by hand and has no run ID.
- On the macOS runner, a repeat scan of `/opt/homebrew` (200,140 items) takes 23.1 ms (run 37853798602). The first scan of the same tree takes 1.04 s (run 37904223270, a different runner).
- On the macOS runner, a repeat scan of `/Applications/Xcode.app` (156,994 items) takes 21 ms. It took 1,043 ms before PR #21 (run 37853798602), because every repeat scan listed the folders with hard-linked files again. Now minimenta lists them again only when links may have changed.

A scan of `/` on macOS now counts the data volume once. Before, every file behind a firmlink such as `/Users` counted twice. The scan is shorter and the total is lower (240.3 GiB instead of 442.4 GiB, PR #31, run 37855727004), because it does less work. This is a correctness fix. It is not a faster scanner, and none of the numbers above include it.

### How the numbers were measured

- **Script and workflow.** [`.github/workflows/speed.yml`](.github/workflows/speed.yml) runs [`bench/compare.py`](bench/compare.py) on one GitHub-hosted runner for each platform. The workflow runs when `bench/` changes and on request. gdu, dua-cli, dust and ncdu on Linux come from official release downloads with pinned SHA-256 values ([`bench/install-tools.sh`](bench/install-tools.sh)). ncdu on macOS comes from Homebrew, which builds it from the source release.
- **Runners in run 37904223270.** Linux: 4 cores, AMD EPYC 7763. macOS: 3 cores, Apple M1 (virtual). Windows: 4 cores, AMD, administrator.
- **Pairs.** Each tool runs against minimenta in alternating pairs. The order inside a pair changes from round to round. The script runs as many pairs as the time budget allows: 7 to 15 pairs for each cold tree and 60 for each warm tree.
- **Commands.** `minimenta --summary`, `ncdu -0 -o /dev/null` (JSON export, default settings), `ncdu -0 -t N -O /dev/null` (binary export), `gdu -n -p -c`, `dua` and `dust -d 0 -P -b -c`. All output goes to the null device. The setting checks in the [full results](#full-results) run ncdu with the other export format. They are not part of the comparison.
- **Cold disk.** The script drops the operating system file cache before every run. Linux: `sync; echo 3 > /proc/sys/vm/drop_caches`. macOS: `sync; sudo purge`. Windows: it empties the working sets and purges the standby list.
- **The cloud host may still cache the disk.** A first read on a fresh runner was about 8 times slower than a cold run here. The first scan of `/usr` on a fresh Linux runner took minimenta 61.6 s (run 37899170123, no cache drop). Cold scans after a cache drop took a median of 7.9 s (run 37904223270). So the cold numbers describe a disk behind a host cache. We did not time the other tools on such a first read, so we do not know if the ratios hold there.
- **Totals check.** Before it times anything, the script compares the total size that each tool reports with the total of minimenta. No tool reported a total more than 5% lower, so no tool seems to skip work. gdu and dust report the apparent size on Windows. On the Windows synthetic tree, dust adds 8.6 MB for the folders themselves.
- **Versions.** minimenta 0.1.0, gdu 5.38.0, dua-cli 2.45.1 and dust 1.2.6. ncdu is 2.9.2 on macOS (Homebrew) and 2.9.1 on Linux. No static build of ncdu 2.9.2 exists, and ncdu 2.9.2 only fixes a build problem. dua-cli on Linux is the musl build, which is the only x86-64 Linux release file.
- **CI benchmarks.** The benchmark jobs in `ci.yml` ([`bench/cold.sh`](bench/cold.sh), [`bench/throughput.sh`](bench/throughput.sh), [`bench/windows.sh`](bench/windows.sh)) stay as quick checks. The numbers on this page do not come from them.

#### Runs differ

The runner hardware changes between runs, and the ratios change with it. Run 37901931836 got other CPUs on Linux (Intel Xeon Platinum 8370C) and on Windows (another AMD generation). The macOS runner was the same type, and its ratios stayed close.

| Cold first scan, ratio | Run 37901931836 | Run 37904223270 |
| --- | ---: | ---: |
| Linux, ncdu | 3.3x | 4.4x |
| Linux, ncdu -t 64 | 0.91x, even | 0.98x, even |
| Linux, gdu | 1.14x | 1.21x |
| Linux, dua-cli | 1.25x | 1.50x |
| Linux, dust | 1.16x | 1.42x |
| Windows, gdu | 2.2x | 4.4x |
| Windows, dua-cli | 2.2x | 4.3x |
| Windows, dust | 3.8x | 8.0x |

On Windows, the MFT scan of minimenta took about 8.1 s to 8.5 s in the first run and 4.0 s to 4.2 s in the second. The medians of gdu, dua-cli and dust changed by less than 3%. Read every ratio as one sample on one machine type.

#### Full results

<details>
<summary>Every row of run 37904223270</summary>

The ratio is the time of the tool divided by the time of minimenta. For `minimenta --no-mft` on Windows, the ratio is the time of the scan without the MFT divided by the time of the administrator scan. Times are medians of the pairs.

| Platform | Tree | Mode | Tool | Tool median | minimenta median | Ratio | Middle half | Pairs, minimenta faster / tool faster | Result |
| --- | --- | --- | --- | ---: | ---: | ---: | --- | --- | --- |
| macOS | `/opt/homebrew` | cold | ncdu (defaults) | 4.60 s | 1.04 s | 4.425 | 3.93 to 5.32 | 15 (15 / 0) | minimenta faster |
| macOS | `/opt/homebrew` | cold | ncdu -t 64 | 2.02 s | 1.09 s | 1.674 | 1.39 to 2.07 | 15 (14 / 1) | minimenta faster |
| macOS | `/opt/homebrew` | cold | gdu | 3.01 s | 1.03 s | 2.545 | 2.19 to 3.31 | 15 (15 / 0) | minimenta faster |
| macOS | `/opt/homebrew` | cold | dua-cli | 1.64 s | 968.9 ms | 1.697 | 1.26 to 2.09 | 15 (14 / 1) | minimenta faster |
| macOS | `/opt/homebrew` | cold | dust | 3.07 s | 1.08 s | 2.771 | 2.34 to 4.06 | 15 (15 / 0) | minimenta faster |
| macOS | `/System/Library` | cold | ncdu (defaults) | 9.20 s | 2.11 s | 4.531 | 4.25 to 4.83 | 11 (11 / 0) | minimenta faster |
| macOS | `/System/Library` | cold | ncdu -t 64 | 3.85 s | 2.07 s | 1.962 | 1.67 to 2.06 | 11 (11 / 0) | minimenta faster |
| macOS | `/System/Library` | cold | gdu | 5.73 s | 2.15 s | 2.587 | 2.20 to 2.90 | 11 (11 / 0) | minimenta faster |
| macOS | `/System/Library` | cold | dua-cli | 4.34 s | 2.31 s | 1.898 | 1.61 to 2.03 | 11 (11 / 0) | minimenta faster |
| macOS | `/System/Library` | cold | dust | 5.70 s | 2.10 s | 2.504 | 2.25 to 3.27 | 11 (11 / 0) | minimenta faster |
| macOS | synthetic tree | warm | ncdu (defaults) | 214.8 ms | 69.1 ms | 3.217 | 2.24 to 3.96 | 60 (60 / 0) | minimenta faster |
| macOS | synthetic tree | warm | ncdu -t 3 | 89.0 ms | 69.9 ms | 1.274 | 1.14 to 1.60 | 60 (49 / 11) | minimenta faster |
| macOS | synthetic tree | warm | ncdu (defaults, binary export) (setting check) | 230.6 ms | 64.4 ms | 3.265 | 2.38 to 4.29 | 60 (60 / 0) | minimenta faster |
| macOS | synthetic tree | warm | ncdu -t 3 (JSON export) (setting check) | 94.6 ms | 72.9 ms | 1.396 | 1.01 to 1.71 | 60 (46 / 14) | minimenta faster |
| macOS | synthetic tree | warm | gdu | 131.0 ms | 72.6 ms | 2.023 | 1.38 to 2.40 | 60 (56 / 4) | minimenta faster |
| macOS | synthetic tree | warm | dua-cli | 80.6 ms | 72.7 ms | 1.154 | 0.78 to 1.74 | 60 (38 / 22) | even |
| macOS | synthetic tree | warm | dust | 127.3 ms | 65.6 ms | 1.964 | 1.62 to 2.29 | 60 (59 / 1) | minimenta faster |
| Linux | `/usr` | cold | ncdu (defaults) | 34.84 s | 7.89 s | 4.407 | 4.36 to 4.49 | 7 (7 / 0) | minimenta faster |
| Linux | `/usr` | cold | ncdu -t 64 | 7.65 s | 7.87 s | 0.979 | 0.96 to 1.01 | 7 (2 / 5) | even |
| Linux | `/usr` | cold | gdu | 9.46 s | 7.84 s | 1.206 | 1.19 to 1.24 | 7 (7 / 0) | minimenta faster |
| Linux | `/usr` | cold | dua-cli | 11.78 s | 7.94 s | 1.496 | 1.48 to 1.54 | 7 (7 / 0) | minimenta faster |
| Linux | `/usr` | cold | dust | 11.12 s | 7.86 s | 1.424 | 1.37 to 1.50 | 7 (7 / 0) | minimenta faster |
| Linux | synthetic tree | warm | ncdu (defaults) | 119.6 ms | 48.0 ms | 2.509 | 2.41 to 2.61 | 60 (60 / 0) | minimenta faster |
| Linux | synthetic tree | warm | ncdu -t 4 | 44.1 ms | 46.1 ms | 0.958 | 0.92 to 1.05 | 60 (19 / 41) | even |
| Linux | synthetic tree | warm | ncdu (defaults, binary export) (setting check) | 126.5 ms | 47.2 ms | 2.694 | 2.54 to 2.77 | 60 (60 / 0) | minimenta faster |
| Linux | synthetic tree | warm | ncdu -t 4 (JSON export) (setting check) | 44.0 ms | 46.8 ms | 0.968 | 0.93 to 1.06 | 60 (22 / 38) | even |
| Linux | synthetic tree | warm | gdu | 80.3 ms | 46.7 ms | 1.726 | 1.67 to 1.83 | 60 (60 / 0) | minimenta faster |
| Linux | synthetic tree | warm | dua-cli | 154.2 ms | 46.7 ms | 3.332 | 3.18 to 3.47 | 60 (60 / 0) | minimenta faster |
| Linux | synthetic tree | warm | dust | 75.8 ms | 46.7 ms | 1.634 | 1.59 to 1.77 | 60 (60 / 0) | minimenta faster |
| Windows | `C:\Program Files` | cold | minimenta --no-mft | 13.54 s | 4.05 s | 3.351 | 3.19 to 4.32 | 8 (8 / 0) | default scan faster |
| Windows | `C:\Program Files` | cold | gdu | 18.16 s | 4.17 s | 4.427 | 4.34 to 4.92 | 8 (8 / 0) | minimenta faster |
| Windows | `C:\Program Files` | cold | dua-cli | 17.61 s | 4.02 s | 4.292 | 4.20 to 4.71 | 8 (8 / 0) | minimenta faster |
| Windows | `C:\Program Files` | cold | dust | 31.42 s | 3.97 s | 7.980 | 7.87 to 8.24 | 8 (8 / 0) | minimenta faster |
| Windows | synthetic tree | warm | minimenta --no-mft | 31.7 ms | 31.8 ms | 0.993 | 0.96 to 1.02 | 60 (24 / 36) | even |
| Windows | synthetic tree | warm | gdu | 83.2 ms | 31.4 ms | 2.667 | 2.59 to 2.72 | 60 (60 / 0) | minimenta faster |
| Windows | synthetic tree | warm | dua-cli | 85.4 ms | 31.3 ms | 2.719 | 2.65 to 2.76 | 60 (60 / 0) | minimenta faster |
| Windows | synthetic tree | warm | dust | 766.7 ms | 31.9 ms | 23.980 | 22.75 to 24.46 | 60 (60 / 0) | minimenta faster |
</details>

### Why minimenta is faster

- **More threads.** A scan mostly waits on the disk and the kernel, so minimenta uses at least 16 threads. ncdu uses 1 thread unless you pass `-t`.
- **Bulk reads on macOS.** One `getattrlistbulk(2)` call returns the names, types and sizes of many entries at once. ncdu calls `fstatat` for every file. Linux has no such call, so both tools need one `stat` per file there. On a cold Linux disk, reading the directory blocks takes almost all the time, and the order of those reads decides the speed. In the Linux runs, `ncdu -t 64` is as fast as minimenta.
- **Huge folders on Linux.** One folder with hundreds of thousands of files used to keep one thread busy while the other threads waited. minimenta now splits the stat calls for the later batches of such a folder across threads. On a folder with 200,000 files, minimenta became 2.31x faster when warm and 2.60x faster when cold (run 37854877327, 20 warm pairs and 6 cold pairs). A scan of `/usr` does not change.
- **The master file table on Windows.** As administrator on NTFS, minimenta reads the master file table of the volume in large parallel blocks, as WizTree does, while it lists directories. On a cold disk, this replaces thousands of small reads. The reader starts after 50 ms when the listing is clearly slow (below 40,000 items per second), or later when it is only moderately slow. The first result to finish wins, so a warm scan does not wait for the table. A slow scan (5 s or more) by an administrator whose rights UAC limits ends with a hint to run minimenta as administrator.
- **The cache on macOS.** minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta` and asks FSEvents which directories changed since. It lists only those again. The browser says when it shows a cached scan, and `r` scans everything again.

The goal of 2x over ncdu at its best is not reached on a cold disk. The best result against `ncdu -t 64` is 1.96x on `/System/Library`, where minimenta skips empty folders. On the data volume it is 1.67x, and on Linux the two tools are even.

Endpoint security software (for example Microsoft Defender) inspects every directory open. On such machines, opening directories takes a large part of the scan time for every tool, and the difference between minimenta and ncdu becomes smaller.

## minimenta compared with other tools

<p align="center">
  <img src="docs/assets/comparison.svg" alt="Comparison of minimenta, ncdu, gdu, dua-cli and dust. Start prompt with the current folder: only minimenta. Range select with Shift+arrows: only minimenta. Select several items at once: minimenta, gdu and dua-cli. macOS Trash with Put Back: minimenta and dua-cli, partial in ncdu and gdu. Bulk reads on macOS with getattrlistbulk: minimenta and dua-cli." width="100%">
</p>

Compared on 2026-10-07 with each project's README, manual, help screen or source. Partial means that the tool does part of the row:

- ncdu and gdu reach the macOS Trash only through a custom command: `ncdu --delete-command` or `gdu --trash-command`. The built-in Trash of gdu does not support macOS.
- dua-cli moves marked items to the Trash with the `trash` crate, which asks Finder by default, so "Put Back" works there too.

<details>
<summary>Comparison as text</summary>

| Feature | minimenta | ncdu | gdu | dua-cli | dust |
| --- | --- | --- | --- | --- | --- |
| Start prompt with the current folder | Yes | No (scans the current folder) | No (scans the current folder) | No (scans the current folder) | No |
| Range select with Shift+arrows | Yes | No | No | No | No |
| Select several items at once | Yes | No | Yes (Space) | Yes (Space) | No |
| macOS Trash with Put Back | Yes | Partial (`--delete-command`) | Partial (`--trash-command`) | Yes (Ctrl+T) | No |
| Bulk reads on macOS (`getattrlistbulk`) | Yes | No (`fstatat` per file) | No | Yes | No |

Sources: the [ncdu manual](https://dev.yorhel.nl/ncdu/man) and [scanner source](https://code.blicky.net/yorhel/ncdu/src/branch/zig/src/scan.zig), the [gdu README](https://github.com/dundee/gdu), [help screen](https://github.com/dundee/gdu/blob/master/tui/show.go) and [macOS Trash code](https://github.com/dundee/gdu/blob/master/pkg/remove/trash_darwin.go), the [dua-cli README](https://github.com/Byron/dua-cli), [key bindings](https://github.com/Byron/dua-cli/blob/main/src/config.rs), [options](https://github.com/Byron/dua-cli/blob/main/src/options.rs) and [Trash code](https://github.com/Byron/dua-cli/blob/main/src/interactive/app/deletion.rs), the [trash crate](https://github.com/Byron/trash-rs/blob/master/src/macos/mod.rs), and the [dust README](https://github.com/bootandy/dust). The bulk read cells come from a GitHub code search for `getattrlistbulk` in each repository.

</details>

dua-cli and gdu have features that minimenta does not have, for example search and more platforms, and gdu can export a scan to a file. dust prints a tree and has no interactive mode. Choose minimenta when you want the ncdu look and keys together with a start prompt, range selection and the Trash.

## Development

Before you open a pull request, run the same checks as CI:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`docs/demo/render.sh` records the demo GIF from [`docs/demo/demo.tape`](docs/demo/demo.tape).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
