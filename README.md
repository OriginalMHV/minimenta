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
- **The Trash first.** `d` moves the selection to the Trash. On macOS, minimenta asks Finder to do it, so "Put Back" works. If Finder cannot be controlled, minimenta uses the file manager API instead. `D` deletes permanently. Both keys ask for confirmation first: `y` or Enter confirms a move to the Trash, and only `y` confirms a permanent delete. `u` puts the last move to the Trash back, and each further `u` undoes the move before it, until you quit.
- **The ncdu look and keys.** The same layout, size bars, file flags, sort keys and `-x` option. The keys that both tools have work the same, except `d`, which moves items to the Trash.
- **A fast first scan.** At least 16 threads and, on macOS, bulk directory reads with `getattrlistbulk(2)`. On a cold disk, minimenta scans about 5.5x faster than ncdu with its defaults on macOS and about 4.8x faster on Linux (the median of three runs). See [Speed](#speed).
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
2. The browser lists the items in the folder, largest first. Open a folder with Enter and go back with Left. The list keeps five rows visible above and below the cursor, and the bottom bar shows the row of the cursor, for example `34/412`.
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
| `--no-spread` | Read a cold disk in tree order, not in random order (Linux) |
| `--cache` | Use the cache together with `--summary`, which scans everything by default |
| `--summary` | Scan, print the totals, and exit |
| `-h`, `--help` | Print the help |
| `-V`, `--version` | Print the version |

## Speed

<p align="center">
  <img src="docs/assets/speed.svg" alt="First scan of a cold disk on macOS, Linux and Windows. Bars show the speed of minimenta, ncdu, gdu, dua-cli and dust relative to a baseline tool. Longer is faster. The first row of each platform is minimenta. Each bar is the median of three runs. The details below list every value." width="600">
</p>

The chart shows the median of three runs, and the details below hold every number. The runs differ with the runner CPU: minimenta was 4.4x to 10.3x faster than gdu on Windows. On Linux, minimenta and `ncdu -t 64` are even, although their bars differ. The chart takes the median of each tool across the three runs separately (see What the results show).

<details>
<summary>All numbers, the warm scans and the method</summary>

**Measured data.** The chart and the tables come from three runs of the Speed workflow on 2026-10-10: [38032806084](https://github.com/OriginalMHV/minimenta/actions/runs/38032806084), [38032807835](https://github.com/OriginalMHV/minimenta/actions/runs/38032807835) and [38032809740](https://github.com/OriginalMHV/minimenta/actions/runs/38032809740). All three measured main at `87550d8` (the merge of PR #42) on GitHub-hosted runners. The first ran on main. The other two ran on the temporary refs `speed/run-b` and `speed/run-c` at the same commit. Both refs are deleted. GitHub gave the runs different CPUs, which changes some results a lot (see Runs differ below). Every other number on this page names its own run.

**How to read the numbers.** Each tool runs in alternating pairs with minimenta. The ratio of a tool is the median over the pairs of the time of the tool divided by the time of minimenta. A ratio above 1 means that minimenta is faster. A ratio below 1 means that the tool is faster. A result is even when the middle half of the pair ratios includes 1.

The chart shows a speed relative to a baseline. The baseline is ncdu with its default settings. ncdu does not run on Windows, so the baseline there is gdu. The speed of a tool is the ratio of the baseline divided by the ratio of the tool. The speed of minimenta is the ratio of the baseline. The chart computes each speed in each run and shows the median of the three values. A tool that is slower than the baseline shows below 1.0x. All bars share one scale. Every number is rounded to the digits shown, and an exact tie rounds against minimenta.

<!-- speed-tables:begin -->

**Runs**

| Run | Date | Commit |
| --- | --- | --- |
| [38032806084](https://github.com/OriginalMHV/minimenta/actions/runs/38032806084) | 2026-10-10 | [`87550d8`](https://github.com/OriginalMHV/minimenta/commit/87550d86cafbd91b7ee89c8f6d7b083a4943295c) |
| [38032807835](https://github.com/OriginalMHV/minimenta/actions/runs/38032807835) | 2026-10-10 | [`87550d8`](https://github.com/OriginalMHV/minimenta/commit/87550d86cafbd91b7ee89c8f6d7b083a4943295c) |
| [38032809740](https://github.com/OriginalMHV/minimenta/actions/runs/38032809740) | 2026-10-10 | [`87550d8`](https://github.com/OriginalMHV/minimenta/commit/87550d86cafbd91b7ee89c8f6d7b083a4943295c) |

| Run | Runner | CPU | Cores |
| --- | --- | --- | ---: |
| 38032806084 | macOS | Apple M1 (Virtual) | 3 |
| 38032806084 | Linux | AMD EPYC 9V45 96-Core Processor | 4 |
| 38032806084 | Windows, administrator | AMD64 Family 25 Model 17 Stepping 1, AuthenticAMD | 4 |
| 38032807835 | macOS | Apple M1 (Virtual) | 3 |
| 38032807835 | Linux | AMD EPYC 7763 64-Core Processor | 4 |
| 38032807835 | Windows, administrator | AMD64 Family 25 Model 1 Stepping 1, AuthenticAMD | 4 |
| 38032809740 | macOS | Apple M1 (Virtual) | 3 |
| 38032809740 | Linux | AMD EPYC 7763 64-Core Processor | 4 |
| 38032809740 | Windows, administrator | AMD64 Family 25 Model 1 Stepping 1, AuthenticAMD | 4 |

Times are medians. Pairs shows all pairs, then the pairs in which minimenta was faster and the pairs in which the tool was faster. Speed is the value of the chart.

3 runs. Each value in the tables below is the median of the values of the runs. Pairs add up.

**First scan of a cold disk, macOS, `/opt/homebrew`**

200,140 items. The speed value is relative to ncdu (defaults). This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.2 | 5.94 s | 1.04 s | 5.508 | 4.80 to 5.89 | 45 (45 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.2 | 2.21 s | 1.08 s | 1.889 | 1.68 to 2.23 | 45 (45 / 0) | minimenta | 2.7x |
| gdu | 5.38.0 | 3.16 s | 1.11 s | 2.879 | 2.58 to 3.28 | 45 (45 / 0) | minimenta | 2.0x |
| dua-cli | 2.45.1 | 1.76 s | 1.04 s | 1.762 | 1.49 to 2.07 | 45 (42 / 3) | minimenta | 3.1x |
| dust | 1.2.6 | 3.07 s | 1.05 s | 2.938 | 2.50 to 3.31 | 45 (44 / 1) | minimenta | 1.9x |

The speed value of minimenta is 5.5x. It is the ratio of the baseline.

**First scan of a cold disk, macOS, `/System/Library`**

427,134 to 428,336 items. The speed value is relative to ncdu (defaults).

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.2 | 13.21 s | 2.92 s | 4.706 | 4.41 to 5.66 | 30 (30 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.2 | 5.34 s | 2.71 s | 2.090 | 1.97 to 2.48 | 30 (30 / 0) | minimenta | 2.3x |
| gdu | 5.38.0 | 7.33 s | 2.52 s | 2.684 | 2.44 to 2.81 | 30 (30 / 0) | minimenta | 1.7x |
| dua-cli | 2.45.1 | 4.98 s | 2.68 s | 1.815 | 1.64 to 2.14 | 30 (30 / 0) | minimenta | 2.5x |
| dust | 1.2.6 | 7.72 s | 2.76 s | 2.685 | 2.28 to 3.00 | 30 (30 / 0) | minimenta | 1.8x |

The speed value of minimenta is 4.7x. It is the ratio of the baseline.

**First scan of a cold disk, Linux, `/usr`**

737,881 items. The speed value is relative to ncdu (defaults). This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.1 | 34.34 s | 7.15 s | 4.803 | 4.67 to 4.91 | 21 (21 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.1 | 7.36 s | 7.44 s | 1.016 | 0.98 to 1.06 | 21 (13 / 8) | even | 4.5x |
| gdu | 5.38.0 | 9.84 s | 7.25 s | 1.289 | 1.22 to 1.34 | 21 (21 / 0) | minimenta | 3.9x |
| dua-cli | 2.45.1 | 11.35 s | 7.45 s | 1.559 | 1.50 to 1.60 | 21 (21 / 0) | minimenta | 3.1x |
| dust | 1.2.6 | 10.93 s | 7.27 s | 1.439 | 1.42 to 1.53 | 21 (21 / 0) | minimenta | 3.3x |

The speed value of minimenta is 4.8x. It is the ratio of the baseline.

**First scan of a cold disk, Windows, `C:\Program Files`**

304,060 items. The speed value is relative to gdu. This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| minimenta --no-mft | 0.1.0 | 12.78 s | 4.11 s | 3.224 | 3.05 to 3.52 | 19 (19 / 0) | default scan | 1.5x |
| gdu | 5.38.0 | 17.72 s | 4.02 s | 4.516 | 4.25 to 5.03 | 19 (19 / 0) | minimenta | 1.0x |
| dua-cli | 2.45.1 | 17.12 s | 4.09 s | 4.224 | 3.70 to 5.82 | 19 (19 / 0) | minimenta | 1.1x |
| dust | 1.2.6 | 30.52 s | 4.07 s | 7.506 | 7.24 to 8.45 | 19 (19 / 0) | minimenta | 0.6x |

The speed value of minimenta is 4.5x. It is the ratio of the baseline.

**Scan with a warm cache, macOS, synthetic tree**

51,110 items. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| ncdu (defaults) | 2.9.2 | 193.9 ms | 61.0 ms | 3.343 | 2.47 to 4.19 | 180 (180 / 0) | minimenta |
| ncdu -t 3 | 2.9.2 | 73.6 ms | 63.7 ms | 1.265 | 0.89 to 1.54 | 180 (133 / 47) | even |
| ncdu (defaults, binary export) (setting check) | 2.9.2 | 194.0 ms | 62.7 ms | 3.380 | 2.32 to 4.09 | 180 (180 / 0) | minimenta |
| ncdu -t 3 (JSON export) (setting check) | 2.9.2 | 84.1 ms | 61.3 ms | 1.477 | 1.10 to 1.86 | 180 (144 / 36) | minimenta |
| gdu | 5.38.0 | 111.1 ms | 59.3 ms | 2.033 | 1.56 to 2.48 | 180 (170 / 10) | minimenta |
| dua-cli | 2.45.1 | 71.4 ms | 59.0 ms | 1.254 | 0.97 to 1.54 | 180 (131 / 49) | even |
| dust | 1.2.6 | 116.8 ms | 55.4 ms | 1.960 | 1.60 to 2.23 | 180 (172 / 8) | minimenta |

**Scan with a warm cache, Linux, synthetic tree**

51,110 items. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| ncdu (defaults) | 2.9.1 | 118.1 ms | 44.5 ms | 2.670 | 2.56 to 2.74 | 180 (180 / 0) | minimenta |
| ncdu -t 4 | 2.9.1 | 42.9 ms | 45.0 ms | 0.972 | 0.94 to 1.15 | 180 (54 / 126) | even |
| ncdu (defaults, binary export) (setting check) | 2.9.1 | 123.9 ms | 45.4 ms | 2.745 | 2.65 to 2.83 | 180 (180 / 0) | minimenta |
| ncdu -t 4 (JSON export) (setting check) | 2.9.1 | 43.5 ms | 44.8 ms | 0.979 | 0.94 to 1.06 | 180 (57 / 123) | even |
| gdu | 5.38.0 | 78.4 ms | 44.7 ms | 1.772 | 1.70 to 1.89 | 180 (180 / 0) | minimenta |
| dua-cli | 2.45.1 | 151.8 ms | 45.0 ms | 3.392 | 3.31 to 3.48 | 180 (180 / 0) | minimenta |
| dust | 1.2.6 | 72.2 ms | 45.3 ms | 1.615 | 1.56 to 1.78 | 180 (180 / 0) | minimenta |

**Scan with a warm cache, Windows, synthetic tree**

51,110 items. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| minimenta --no-mft | 0.1.0 | 32.0 ms | 32.2 ms | 0.989 | 0.97 to 1.01 | 180 (63 / 117) | even |
| gdu | 5.38.0 | 84.9 ms | 32.5 ms | 2.623 | 2.57 to 2.66 | 180 (180 / 0) | minimenta |
| dua-cli | 2.45.1 | 89.1 ms | 32.7 ms | 2.719 | 2.61 to 2.81 | 180 (180 / 0) | minimenta |
| dust | 1.2.6 | 762.0 ms | 32.4 ms | 22.864 | 22.34 to 23.44 | 180 (180 / 0) | minimenta |

**Ratio of the cold first scans in each run**

| Platform | Tree | Tool | 38032806084 | 38032807835 | 38032809740 | Median |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| macOS | `/opt/homebrew` | ncdu (defaults) | 5.906 | 5.508 | 4.514 | 5.508 |
| macOS | `/opt/homebrew` | ncdu -t 64 | 1.796 | 2.045 | 1.889 | 1.889 |
| macOS | `/opt/homebrew` | gdu | 3.027 | 2.813 | 2.879 | 2.879 |
| macOS | `/opt/homebrew` | dua-cli | 1.479 | 1.762 | 1.822 | 1.762 |
| macOS | `/opt/homebrew` | dust | 3.034 | 2.938 | 2.474 | 2.938 |
| macOS | `/System/Library` | ncdu (defaults) | 4.335 | 4.854 | 4.706 | 4.706 |
| macOS | `/System/Library` | ncdu -t 64 | 2.000 | 2.118 | 2.090 | 2.090 |
| macOS | `/System/Library` | gdu | 2.684 | 2.832 | 2.515 | 2.684 |
| macOS | `/System/Library` | dua-cli | 1.716 | 1.815 | 1.958 | 1.815 |
| macOS | `/System/Library` | dust | 2.685 | 2.714 | 2.539 | 2.685 |
| Linux | `/usr` | ncdu (defaults) | 4.803 | 6.215 | 4.443 | 4.803 |
| Linux | `/usr` | ncdu -t 64 | 1.059 | 0.989 | 1.016 | 1.016 |
| Linux | `/usr` | gdu | 1.230 | 1.406 | 1.289 | 1.289 |
| Linux | `/usr` | dua-cli | 1.559 | 1.818 | 1.496 | 1.559 |
| Linux | `/usr` | dust | 1.437 | 1.716 | 1.439 | 1.439 |
| Windows | `C:\Program Files` | minimenta --no-mft | 4.542 | 3.224 | 2.959 | 3.224 |
| Windows | `C:\Program Files` | gdu | 10.257 | 4.359 | 4.516 | 4.516 |
| Windows | `C:\Program Files` | dua-cli | 6.887 | 4.224 | 4.078 | 4.224 |
| Windows | `C:\Program Files` | dust | 9.597 | 7.506 | 7.176 | 7.506 |

<!-- speed-tables:end -->

**What the results show**

- **Linux:** `ncdu -t 64` and minimenta are even on a cold disk. The direct ratios of the three runs are 1.06, 0.99 and 1.02, and the median is 1.02. minimenta was faster in 7 of 8 pairs in the first run, in 2 of 6 pairs in the second run, and in 4 of 7 pairs in the third run. The middle half of the first run is 1.02 to 1.13. It excludes 1.0. The middle half of the second and third run includes 1.0, and so does the middle half in the table (0.98 to 1.06). The chart shows 4.8x against 4.5x because both medians come from the first run, where the ratio was 1.06. It is not a lead for minimenta, because the median of the three direct ratios is 1.02 and its middle half includes 1.0. On the warm tree, `ncdu -t 4` and minimenta are even too (ratio 0.972).
- **macOS:** the data volume (`/opt/homebrew`) is the main result. On `/System/Library`, minimenta skips empty folders without opening them. That works only on the system volume, and the other tools open every folder, so this tree favors minimenta. Cold macOS runs are short. minimenta needs about 1 s for `/opt/homebrew` after `purge`. Read these runs as "after purge", not as raw disk speed. On the warm tree, `ncdu -t 3` and dua-cli are even with minimenta. The middle half is wide because the macOS runner is noisy.
- **Windows:** the runner is an administrator, so minimenta reads the NTFS master file table (MFT). The row `minimenta --no-mft` is a run without administrator rights. The chart shows it as "minimenta (no admin)". In its table row, Tool time is the time of the `--no-mft` scan, and minimenta time is the time of the administrator scan. These medians are 12.78 s and 4.11 s. The ratio is 3.224. It is not 12.78 divided by 4.11 (3.11), because the median of ratios is not the ratio of medians. The Windows cold times changed a lot with the runner CPU, see Runs differ below. On the warm tree, `minimenta --no-mft` and minimenta are even. The scan takes about 32 ms, which is less than the 50 ms that pass before the MFT reader can start. On this tree, dust needs 762 ms against 32 ms for minimenta. We did not look into why.
- **Microsoft Defender:** real-time monitoring is off on the Windows runner, and `C:\` and `D:\` are excluded. Endpoint security software inspects every directory open. On machines that run it, opening directories takes a large part of the scan time for every tool, and the difference between minimenta and ncdu becomes smaller.
- **dust and dua-cli:** dust prints a tree and is not interactive. dua-cli exits with code 1 on `/System/Library` in all three runs. minimenta reports 10 read errors there in all three runs, so unreadable folders are the likely cause.
- **The 2x goal:** minimenta aims for 2x over ncdu at its best. The cold runs test only one multi-thread setting of ncdu, `-t 64`. Against it, minimenta does not reach 2x in every run. On macOS, the ratio is 1.796, 2.045 and 1.889 on `/opt/homebrew`, and 2.000, 2.118 and 2.090 on `/System/Library`, in the order of the three runs. The medians are 1.889 and 2.090. On Linux, the two tools are even.

**Repeat scans on macOS.** A repeat scan reads the cache and lists again only the folders that FSEvents reports as changed. It does not scan the disk again, so the numbers do not compare with the first scans above. They are not part of the three runs.

- On a 10-core Mac with Microsoft Defender, a repeat scan of trees with 210,000 and 413,000 items takes 0.05 s instead of 2 to 4 s for a full scan. This was measured by hand and has no run ID.
- On the macOS runner, a repeat scan of `/opt/homebrew` (200,140 items) takes 23.1 ms (run 37853798602). The first scan of the same tree takes 1.04 s (the median of the three runs above, on other runners).
- On the macOS runner, a repeat scan of `/Applications/Xcode.app` (156,994 items) takes 21 ms. It took 1,043 ms before PR #21 (run 37853798602), because every repeat scan listed the folders with hard-linked files again. Now minimenta lists them again only when links may have changed.

A scan of `/` on macOS now counts the data volume once. Before, every file behind a firmlink such as `/Users` counted twice. The scan is shorter and the total is lower (240.3 GiB instead of 442.4 GiB, PR #31, run 37855727004), because it does less work. This is a correctness fix. It is not a faster scanner, and none of the numbers above include it.

**How the numbers were measured**

- **Script and workflow.** [`.github/workflows/speed.yml`](.github/workflows/speed.yml) runs [`bench/compare.py`](bench/compare.py) on GitHub-hosted runners: one for macOS, one for Windows and six for Linux. Each Linux job runs at least 12 pairs, and [`bench/merge_speed.py`](bench/merge_speed.py) pools the pairs of the six jobs into one result for each tool. The table of such a tree also shows the median of each job and an interval that holds the median of the job medians with a probability of at least 95 percent. The runs in the tables above are older and used one Linux job. The workflow runs when `bench/` changes and on request. [`bench/speed_chart.py`](bench/speed_chart.py) draws the chart and writes the tables from the `speed-merged` artifacts of the runs. gdu, dua-cli, dust and ncdu on Linux come from official release downloads with pinned SHA-256 values ([`bench/install-tools.sh`](bench/install-tools.sh)). ncdu on macOS comes from Homebrew, which builds it from the source release.
- **Pairs.** Each tool runs against minimenta in alternating pairs. The order inside a pair changes from round to round. The script runs as many pairs as the time budget allows. The Pairs column of each table shows how many ran.
- **Commands.** `minimenta --summary`, `ncdu -0 -o /dev/null` (JSON export, default settings), `ncdu -0 -t N -O /dev/null` (binary export), `gdu -n -p -c`, `dua` and `dust -d 0 -P -b -c`. All output goes to the null device. The setting checks run ncdu with the other export format. They are not part of the comparison.
- **Cold disk.** The script drops the operating system file cache before every run. Linux: `sync`, then `echo 3 > /proc/sys/vm/drop_caches`. macOS: `sync`, then `sudo purge`. Windows: it empties the working sets and purges the standby list.
- **The cloud host may still cache the disk.** A first read on a fresh runner was about 8 times slower than a cold run here. The first scan of `/usr` on a fresh Linux runner took minimenta 61.6 s (run 37899170123, no cache drop). Cold scans after a cache drop took a median of 7.9 s (run 37904223270). Both runs used the scanner from before PR #42. So the cold numbers describe a disk behind a host cache. We did not time the other tools on such a first read, so we do not know if the ratios hold there.
- **Warm cache.** The warm tree is the synthetic tree from [`bench/gen_tree.py`](bench/gen_tree.py): 1,000 folders with 50 empty files each (51,110 items). The operating system cache already holds it. `ncdu -t N` uses one thread for each core of the runner.
- **Allocator.** On the small warm tree, the system allocator is faster than mimalloc. In the Linux job of run 37848716077 (4 cores, 30 pairs each), a build with the system allocator needed 0.93 times as long as the mimalloc build with 16 scan threads. With 4 scan threads, it needed 0.83 times as long. On a warm `/usr` (737,881 items, 10 pairs), it needed 1.03 times as long. Large trees are the common case, so minimenta keeps mimalloc.
- **Totals check.** Before it times anything, the script compares the total size that each tool reports with the total of minimenta. In the three runs, no tool reported a total more than 5% lower. The lowest total was from gdu on `/usr` in run 38032809740. gdu printed 39.8 GB there, against 42.4 GB in the other two runs. The check counted it as the apparent size, 2.37% lower. We do not know why gdu printed less in that run. gdu and dust report the apparent size on Windows. On the Windows synthetic tree, dust adds 8.6 MB for the folders themselves.
- **Versions.** The Version column of each table shows them. ncdu is 2.9.2 on macOS (Homebrew) and 2.9.1 on Linux. No static build of ncdu 2.9.2 exists, and ncdu 2.9.2 only fixes a build problem. dua-cli on Linux is the musl build, which is the only x86-64 Linux release file.
- **CI benchmarks.** The benchmark jobs in `ci.yml` ([`bench/cold.sh`](bench/cold.sh), [`bench/throughput.sh`](bench/throughput.sh), [`bench/windows.sh`](bench/windows.sh)) stay as quick checks. The numbers on this page do not come from them.

**Runs differ.** The three runs measured the same commit, but GitHub gave them different runner CPUs, and the ratios change with them. macOS got the same Apple M1 (Virtual) runner type in all three runs. Linux got an AMD EPYC 9V45 in the first run and an AMD EPYC 7763 in the other two. Windows got an AMD64 Family 25 Model 17 CPU in the first run and an AMD64 Family 25 Model 1 CPU in the other two.

- **Windows:** on the cold scan of `C:\Program Files`, every tool needed more time on the Model 17 CPU of the first run. gdu needed 43.1 s there, and 17.6 s and 17.7 s on the Model 1 CPU. minimenta needed 4.9 s to 6.7 s there, and 4.0 s to 4.1 s on the Model 1 CPU. dua-cli, dust and `minimenta --no-mft` were slower on the Model 17 CPU too. The warm scans of the synthetic tree went the other way. Every tool was faster on the Model 17 CPU. minimenta needed 24.1 ms there, and 32.2 ms and 33.0 ms on the Model 1 CPU. dust needed 472 ms there, and 767 ms and 762 ms on the Model 1 CPU. So the cold ratio against gdu is 10.257, 4.359 and 4.516 in the order of the runs. The first run made 3 pairs and the other two made 8 pairs each, so the first run is the least certain. The runner image, the administrator rights and the workflow were the same in the three jobs. Two older runs also show that the CPU changes the minimenta times. Run 37901931836 got a Family 26 CPU, and minimenta needed 8.1 s to 8.5 s. Run 37904223270 got a Family 25 CPU, and minimenta needed 4.0 s to 4.2 s. gdu needed 18.7 s in run 37901931836 and 18.2 s in run 37904223270, so the CPU did not change the gdu time there. We do not know why the Windows times change with the CPU.
- **Linux:** ncdu with its defaults needed 30.4 s, 44.4 s and 34.3 s. minimenta needed 6.3 s, 7.1 s and 7.4 s. So the ratio against ncdu with its defaults is 4.803, 6.215 and 4.443. The speed of minimenta in the chart is the median, 4.8x, and the runs give 4.8x, 6.2x and 4.4x. The ratio against gdu moved less, between 1.230 and 1.406. The second and third run got the same CPU type, and the ratio against ncdu with its defaults still moved from 6.215 to 4.443.
- **macOS:** the ratio against ncdu with its defaults is 5.906, 5.508 and 4.514 on `/opt/homebrew`, and 4.335, 4.854 and 4.706 on `/System/Library`.

Read every ratio as one sample on the machine types that GitHub gave these runs.

</details>

### Why minimenta is faster

- **More threads.** A scan mostly waits on the disk and the kernel, so minimenta uses at least 16 threads. ncdu uses 1 thread unless you pass `-t`.
- **Bulk reads on macOS.** One `getattrlistbulk(2)` call returns the names, types and sizes of many entries at once. ncdu calls `fstatat` for every file. Linux has no such call, so both tools need one `stat` per file there. On a cold Linux disk, reading the directory blocks takes almost all the time, and the order of those reads decides the speed. In the Speed runs on Linux, `ncdu -t 64` and minimenta are even.
- **Random order on a cold Linux disk.** Threads that follow the tree read neighbouring folders at the same time, and the disk then serves fewer requests per second. When a scan waits for the disk, minimenta reads the folders in random order. A scan that keeps the CPUs busy, such as a scan of a cached tree, follows the tree. On GitHub runners with a virtual disk, a cold scan of `/usr` took 0.92x (SCSI disk) and 0.89x (NVMe disk) of the time of the scan in tree order. These are A/B runs. The harness times the scanners in alternating pairs in one job. In the same runs, the scan took 0.95x (SCSI, middle half 0.912 to 0.995) and 0.92x (NVMe, middle half 0.881 to 1.011) of the time of `ncdu -t 64`. By the rule of the tables above, that is a small lead on SCSI and even on NVMe. The Speed runs show the two even (ratio 1.016, the median of 3 runs. The 21 pairs together give 1.019). A later test ran the Speed method and the A/B method in the same 12 jobs on a cold `/usr` (131 pairs each, [run 38041108777](https://github.com/OriginalMHV/minimenta/actions/runs/38041108777) and [run 38043852719](https://github.com/OriginalMHV/minimenta/actions/runs/38043852719)). The test was not the real Speed workflow. It did not cover the totals phase, the single runs or the gdu, dua and dust pairs, and its A/B method times adjacent pairs. The Speed method gave 1.056 (middle half 1.007 to 1.110). By the rule of the tables above, that is a small lead. The A/B method gave 1.057 (0.9998 to 1.103). By the same rule, that is even. None of the three differences that we switched (the wait for a quiet disk, the spawn and timing code, and the ncdu default pair before the pair) changed the ratio by 0.03 or more in at least 9 of 12 jobs. The median ratio differed by CPU model in this test. It was 1.04 to 1.05 on EPYC 7763 (9 jobs), 1.07 on Xeon 6973P-C (2 jobs) and 1.13 on Xeon 8573C (1 job). The CPU model, the VM series and the disk type go together here. All 9 EPYC 7763 jobs ran on `Standard_D4ads_v5` VMs with a SCSI disk. The 3 Intel jobs ran on v6 or v7 VMs with an NVMe disk. The test cannot separate the CPU model from the VM series, the disk type and noise. We did not test the CPU model or the disk type. One EPYC 7763 job had `ncdu -t 64` ahead with the Speed method (0.950). A draw of 3 jobs from the test gives a ratio of 1.019 or lower in 8 to 9 of 100 draws. Two EPYC 7763 jobs and one job from any of the 12 give this in 14 of 100 draws. Two EPYC 7763 jobs and one Intel job give it in about 4 of 100 draws. Three EPYC 7763 jobs give it in 15 to 18 of 100 draws. The Speed runs had two EPYC 7763 jobs and one EPYC 9V45 job, and the test has no EPYC 9V45 job. The three Speed runs are a small sample. We did not prove that this explains the earlier gap (see [docs/experiments.md](docs/experiments.md)). We did not measure spinning disks, network drives or local NVMe disks. If a cold scan is slower on such a disk, `--no-spread` reads the folders in tree order.
- **Huge folders on Linux.** One folder with hundreds of thousands of files used to keep one thread busy while the other threads waited. minimenta now splits the stat calls for the later batches of such a folder across threads. On a folder with 200,000 files, minimenta became 2.31x faster when warm and 2.60x faster when cold (run 37854877327, 20 warm pairs and 6 cold pairs). A scan of `/usr` does not change.
- **The master file table on Windows.** As administrator on NTFS, minimenta reads the master file table of the volume in large parallel blocks, as WizTree does, while it lists directories. On a cold disk, this replaces thousands of small reads. The reader starts after 50 ms when the listing is clearly slow (below 40,000 items per second), or later when it is only moderately slow. The first result to finish wins, so a warm scan does not wait for the table. A slow scan (5 s or more) by an administrator whose rights UAC limits ends with a hint to run minimenta as administrator.
- **The cache on macOS.** minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta` and asks FSEvents which directories changed since. It lists only those again. The browser says when it shows a cached scan, and `r` scans everything again. The progress screen says when minimenta checks the cache. The check stops when FSEvents does not answer within half the time of the last full scan, or when too many folders changed. minimenta then scans all folders again, and the screen says so. During that scan, the progress screen estimates the time left. It uses the item count of the cached scan and the speed of the scan so far. A rescan with `r` shows the same estimate on every platform. A first scan has no earlier count, so it shows no estimate.

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
