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
- **The Trash first.** `d` moves the selection to the Trash. On macOS, minimenta asks Finder to do it, so "Put Back" works. If Finder cannot be controlled, minimenta uses the file manager API instead. `D` deletes permanently. Both keys ask for confirmation first: `y` or Enter confirms a move to the Trash, and only `y` confirms a permanent delete. Shift+A confirms and stops the question of that key until you quit. The help screen (`?`) shows a question that is off and turns it back on. `u` puts the last move to the Trash back, and each further `u` undoes the move before it, until you quit.
- **The ncdu look and keys.** The same layout, size bars, file flags, sort keys and `-x` option. The keys that both tools have work the same, except `d`, which moves items to the Trash.
- **A fast first scan.** At least 16 threads and, on macOS, bulk directory reads with `getattrlistbulk(2)`. On a cold disk, minimenta scans about 5.1x faster than ncdu with its defaults on macOS and about 4.9x faster on Linux (the median of three runs). Against `ncdu -t 64` on Linux, the lead is small. See [Speed](#speed).
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

The chart shows the median of three runs, and the details below hold every number. The runs differ: minimenta was 3.6x to 6.3x faster than gdu on Windows. In the run with the lowest value, 38065977369, gdu reported a total 17.28% lower than the minimenta total (see Totals check). On Linux, the bars show 4.9x for minimenta and 4.8x for `ncdu -t 64`. The table of job medians decides that comparison. By its rule, minimenta is faster than `ncdu -t 64` on a cold Linux `/usr` by a small margin, but the pooled pairs call the two even. The chart takes the median of each tool across the three runs separately (see What the results show).

<details>
<summary>All numbers, the warm scans and the method</summary>

**Measured data.** The chart and the tables come from three runs of the Speed workflow on 2026-10-10: [38065977369](https://github.com/OriginalMHV/minimenta/actions/runs/38065977369), [38070763091](https://github.com/OriginalMHV/minimenta/actions/runs/38070763091) and [38073481635](https://github.com/OriginalMHV/minimenta/actions/runs/38073481635). All three measured commit `a700ff0` (the merge of PR #47, which made the Linux comparison six pooled jobs) on GitHub-hosted runners. Each run started after the previous run ended. The first two ran on main. The third ran on the temporary ref `speed/run3-a700ff0` at the same commit, because main had moved to `d2c2fd9` (PR #48, a change to the README only) during the second run. That ref is deleted. GitHub gave the runs different Linux CPUs (see Runs differ below). Every other number on this page names its own run.

**How to read the numbers.** Each tool runs in alternating pairs with minimenta. The ratio of a tool is the median over the pairs of the time of the tool divided by the time of minimenta. A ratio above 1 means that minimenta is faster. A ratio below 1 means that the tool is faster. A result is even when the middle half of the pair ratios includes 1. A tree that pools several jobs also has a table of the job medians. Its result tells if minimenta is faster on a typical runner. The Faster column follows the middle half of the single pair ratios. That range shows the spread of single pairs, and it does not get narrower with more pairs. When the difference is small, the two can disagree.

The chart shows a speed relative to a baseline. The baseline is ncdu with its default settings. ncdu does not run on Windows, so the baseline there is gdu. The speed of a tool is the ratio of the baseline divided by the ratio of the tool. The speed of minimenta is the ratio of the baseline. The chart computes each speed in each run and shows the median of the three values. A tool that is slower than the baseline shows below 1.0x. All bars share one scale. Every number is rounded to the digits shown, and an exact tie rounds against minimenta.

<!-- speed-tables:begin -->

**Runs**

| Run | Date | Commit |
| --- | --- | --- |
| [38065977369](https://github.com/OriginalMHV/minimenta/actions/runs/38065977369) | 2026-10-10 | [`a700ff0`](https://github.com/OriginalMHV/minimenta/commit/a700ff0dd402a7e5db58e880b4665e0366c564b3) |
| [38070763091](https://github.com/OriginalMHV/minimenta/actions/runs/38070763091) | 2026-10-10 | [`a700ff0`](https://github.com/OriginalMHV/minimenta/commit/a700ff0dd402a7e5db58e880b4665e0366c564b3) |
| [38073481635](https://github.com/OriginalMHV/minimenta/actions/runs/38073481635) | 2026-10-10 | [`a700ff0`](https://github.com/OriginalMHV/minimenta/commit/a700ff0dd402a7e5db58e880b4665e0366c564b3) |

| Run | Runner | CPU | Cores |
| --- | --- | --- | ---: |
| 38065977369 | macOS | Apple M1 (Virtual) | 3 |
| 38065977369 | Linux, 3 jobs | AMD EPYC 7763 64-Core Processor | 4 |
| 38065977369 | Linux, 1 job | INTEL(R) XEON(R) PLATINUM 8573C | 4 |
| 38065977369 | Linux, 2 jobs | AMD EPYC 9V45 96-Core Processor | 4 |
| 38065977369 | Windows, administrator | AMD64 Family 25 Model 1 Stepping 1, AuthenticAMD | 4 |
| 38070763091 | macOS | Apple M1 (Virtual) | 3 |
| 38070763091 | Linux, 3 jobs | AMD EPYC 7763 64-Core Processor | 4 |
| 38070763091 | Linux, 3 jobs | AMD EPYC 9V74 80-Core Processor | 4 |
| 38070763091 | Windows, administrator | AMD64 Family 25 Model 1 Stepping 1, AuthenticAMD | 4 |
| 38073481635 | macOS | Apple M1 (Virtual) | 3 |
| 38073481635 | Linux, 4 jobs | AMD EPYC 7763 64-Core Processor | 4 |
| 38073481635 | Linux, 1 job | AMD EPYC 9V74 80-Core Processor | 4 |
| 38073481635 | Linux, 1 job | INTEL(R) XEON(R) PLATINUM 8573C | 4 |
| 38073481635 | Windows, administrator | AMD64 Family 25 Model 1 Stepping 1, AuthenticAMD | 4 |

Times are medians. Pairs shows all pairs, then the pairs in which minimenta was faster and the pairs in which the tool was faster. Speed is the value of the chart.

3 runs. Each value in the tables below is the median of the values of the runs. Pairs add up.

**First scan of a cold disk, macOS, `/opt/homebrew`**

200,140 items. The speed value is relative to ncdu (defaults). This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.2 | 5.07 s | 0.92 s | 5.083 | 4.28 to 6.25 | 45 (45 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.2 | 1.98 s | 1.05 s | 1.903 | 1.49 to 2.20 | 45 (44 / 1) | minimenta | 2.9x |
| gdu | 5.38.0 | 2.82 s | 0.95 s | 3.118 | 2.42 to 3.34 | 45 (45 / 0) | minimenta | 1.8x |
| dua-cli | 2.45.1 | 1.58 s | 0.96 s | 1.498 | 1.32 to 1.86 | 45 (44 / 1) | minimenta | 3.3x |
| dust | 1.2.6 | 2.96 s | 0.97 s | 2.822 | 2.47 to 3.16 | 45 (45 / 0) | minimenta | 1.7x |

The speed value of minimenta is 5.1x. It is the ratio of the baseline.

**First scan of a cold disk, macOS, `/System/Library`**

427,121 to 427,466 items. The speed value is relative to ncdu (defaults).

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.2 | 10.00 s | 2.00 s | 4.956 | 4.48 to 5.46 | 37 (37 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.2 | 4.28 s | 1.99 s | 2.093 | 1.67 to 2.26 | 37 (37 / 0) | minimenta | 2.4x |
| gdu | 5.38.0 | 5.19 s | 2.24 s | 2.715 | 2.19 to 3.14 | 37 (37 / 0) | minimenta | 1.8x |
| dua-cli | 2.45.1 | 4.09 s | 2.29 s | 1.845 | 1.42 to 2.05 | 37 (37 / 0) | minimenta | 2.6x |
| dust | 1.2.6 | 5.63 s | 2.14 s | 2.728 | 2.35 to 2.99 | 37 (37 / 0) | minimenta | 1.8x |

The speed value of minimenta is 5.0x. It is the ratio of the baseline.

**First scan of a cold disk, Linux, `/usr`**

737,881 items. Each run pools the pairs of 6 jobs on different runners. The speed value is relative to ncdu (defaults). This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| ncdu (defaults) | 2.9.1 | 36.71 s | 7.27 s | 4.949 | 4.23 to 5.77 | 217 (217 / 0) | minimenta | 1.0x |
| ncdu -t 64 | 2.9.1 | 7.68 s | 7.30 s | 1.046 | 1.00 to 1.11 | 217 (147 / 70) | even | 4.8x |
| gdu | 5.38.0 | 9.90 s | 7.37 s | 1.340 | 1.28 to 1.41 | 217 (216 / 1) | minimenta | 3.7x |
| dua-cli | 2.45.1 | 12.29 s | 7.38 s | 1.654 | 1.56 to 1.91 | 217 (216 / 1) | minimenta | 3.0x |
| dust | 1.2.6 | 11.39 s | 7.48 s | 1.566 | 1.41 to 1.71 | 217 (217 / 0) | minimenta | 3.3x |

The speed value of minimenta is 4.9x. It is the ratio of the baseline.

Each job ran on its own runner. Faster jobs shows the jobs in which minimenta was faster in the median, then the jobs in which the tool was. The interval holds the true median of the job medians with a probability of at least 95 percent. The true median is the median of a very large number of jobs. In general, it gets narrower with more jobs. The middle half does not. Run medians shows the median of the job medians of each run. The result names a tool only when the interval and every run median are on its side of 1.

| Tool | Jobs | Faster jobs | Median of the job medians | Interval (95 percent) | Run medians | Result |
| --- | ---: | --- | ---: | --- | --- | --- |
| ncdu (defaults) | 18 | 18 / 0 | 5.008 | 4.520 to 6.029 | 4.818 / 5.008 / 5.572 | minimenta |
| ncdu -t 64 | 18 | 14 / 4 | 1.036 | 1.002 to 1.066 | 1.039 / 1.012 / 1.037 | minimenta |
| gdu | 18 | 18 / 0 | 1.339 | 1.302 to 1.372 | 1.343 / 1.332 / 1.355 | minimenta |
| dua-cli | 18 | 18 / 0 | 1.661 | 1.569 to 1.874 | 1.646 / 1.748 / 1.748 | minimenta |
| dust | 18 | 18 / 0 | 1.562 | 1.405 to 1.686 | 1.462 / 1.612 / 1.597 | minimenta |

**First scan of a cold disk, Windows, `C:\Program Files`**

304,060 items. The speed value is relative to gdu. This tree is in the chart.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster | Speed |
| --- | --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| minimenta --no-mft | 0.1.0 | 13.91 s | 4.06 s | 3.380 | 3.31 to 3.92 | 22 (22 / 0) | default scan | 1.3x |
| gdu | 5.38.0 | 17.80 s | 4.04 s | 4.435 | 4.37 to 4.59 | 22 (22 / 0) | minimenta | 1.0x |
| dua-cli | 2.45.1 | 17.92 s | 4.01 s | 4.527 | 4.36 to 4.65 | 22 (22 / 0) | minimenta | 1.0x |
| dust | 1.2.6 | 31.48 s | 4.01 s | 7.975 | 7.86 to 8.07 | 22 (22 / 0) | minimenta | 0.6x |

The speed value of minimenta is 4.4x. It is the ratio of the baseline.

**Scan with a warm cache, macOS, synthetic tree**

51,110 items. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| ncdu (defaults) | 2.9.2 | 203.6 ms | 58.4 ms | 3.781 | 2.85 to 4.35 | 180 (180 / 0) | minimenta |
| ncdu -t 3 | 2.9.2 | 92.2 ms | 61.2 ms | 1.460 | 1.25 to 1.74 | 180 (147 / 33) | minimenta |
| ncdu (defaults, binary export) (setting check) | 2.9.2 | 211.1 ms | 58.9 ms | 3.854 | 2.96 to 4.60 | 180 (180 / 0) | minimenta |
| ncdu -t 3 (JSON export) (setting check) | 2.9.2 | 91.2 ms | 61.8 ms | 1.499 | 1.17 to 1.76 | 180 (149 / 31) | minimenta |
| gdu | 5.38.0 | 119.2 ms | 59.2 ms | 2.204 | 1.65 to 2.48 | 180 (168 / 12) | minimenta |
| dua-cli | 2.45.1 | 77.7 ms | 58.6 ms | 1.331 | 0.99 to 1.68 | 180 (137 / 43) | even |
| dust | 1.2.6 | 110.0 ms | 57.8 ms | 1.959 | 1.60 to 2.20 | 180 (177 / 3) | minimenta |

**Scan with a warm cache, Linux, synthetic tree**

51,110 items. Each run pools the pairs of 6 jobs on different runners. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| ncdu (defaults) | 2.9.1 | 115.1 ms | 43.9 ms | 2.616 | 2.49 to 2.69 | 1080 (1080 / 0) | minimenta |
| ncdu -t 4 | 2.9.1 | 42.2 ms | 43.8 ms | 0.953 | 0.91 to 1.02 | 1080 (326 / 754) | even |
| ncdu (defaults, binary export) (setting check) | 2.9.1 | 118.8 ms | 44.2 ms | 2.721 | 2.60 to 2.81 | 1080 (1080 / 0) | minimenta |
| ncdu -t 4 (JSON export) (setting check) | 2.9.1 | 42.5 ms | 43.5 ms | 0.953 | 0.91 to 1.07 | 1080 (355 / 725) | even |
| gdu | 5.38.0 | 75.8 ms | 43.8 ms | 1.710 | 1.63 to 1.80 | 1080 (1078 / 2) | minimenta |
| dua-cli | 2.45.1 | 151.5 ms | 44.0 ms | 3.472 | 3.30 to 3.65 | 1080 (1080 / 0) | minimenta |
| dust | 1.2.6 | 71.5 ms | 43.9 ms | 1.609 | 1.53 to 1.71 | 1080 (1080 / 0) | minimenta |

Each job ran on its own runner. Faster jobs shows the jobs in which minimenta was faster in the median, then the jobs in which the tool was. The interval holds the true median of the job medians with a probability of at least 95 percent. The true median is the median of a very large number of jobs. In general, it gets narrower with more jobs. The middle half does not. Run medians shows the median of the job medians of each run. The result names a tool only when the interval and every run median are on its side of 1.

| Tool | Jobs | Faster jobs | Median of the job medians | Interval (95 percent) | Run medians | Result |
| --- | ---: | --- | ---: | --- | --- | --- |
| ncdu (defaults) | 18 | 18 / 0 | 2.637 | 2.617 to 2.661 | 2.636 / 2.664 / 2.629 | minimenta |
| ncdu -t 4 | 18 | 0 / 18 | 0.953 | 0.936 to 0.971 | 0.941 / 0.952 / 0.956 | ncdu -t 4 |
| gdu | 18 | 18 / 0 | 1.730 | 1.683 to 1.753 | 1.714 / 1.709 / 1.755 | minimenta |
| dua-cli | 18 | 18 / 0 | 3.381 | 3.335 to 3.631 | 3.319 / 3.473 / 3.391 | minimenta |
| dust | 18 | 18 / 0 | 1.614 | 1.549 to 1.636 | 1.591 / 1.608 / 1.623 | minimenta |

**Scan with a warm cache, Windows, synthetic tree**

51,110 items. From bench/gen_tree.py.

| Tool | Version | Tool time | minimenta time | Ratio | Middle half | Pairs | Faster |
| --- | --- | ---: | ---: | ---: | --- | --- | --- |
| minimenta --no-mft | 0.1.0 | 33.8 ms | 34.0 ms | 0.992 | 0.98 to 1.01 | 180 (71 / 109) | even |
| gdu | 5.38.0 | 87.2 ms | 33.8 ms | 2.583 | 2.53 to 2.72 | 180 (180 / 0) | minimenta |
| dua-cli | 2.45.1 | 91.6 ms | 34.2 ms | 2.712 | 2.67 to 2.78 | 180 (180 / 0) | minimenta |
| dust | 1.2.6 | 784.1 ms | 34.0 ms | 23.161 | 22.67 to 23.75 | 180 (180 / 0) | minimenta |

**Ratio of the cold first scans in each run**

| Platform | Tree | Tool | 38065977369 | 38070763091 | 38073481635 | Median |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| macOS | `/opt/homebrew` | ncdu (defaults) | 5.083 | 5.592 | 4.605 | 5.083 |
| macOS | `/opt/homebrew` | ncdu -t 64 | 1.732 | 1.903 | 1.911 | 1.903 |
| macOS | `/opt/homebrew` | gdu | 3.119 | 3.118 | 2.481 | 3.118 |
| macOS | `/opt/homebrew` | dua-cli | 1.498 | 1.671 | 1.431 | 1.498 |
| macOS | `/opt/homebrew` | dust | 2.934 | 2.822 | 2.730 | 2.822 |
| macOS | `/System/Library` | ncdu (defaults) | 5.007 | 4.956 | 4.711 | 4.956 |
| macOS | `/System/Library` | ncdu -t 64 | 2.095 | 1.947 | 2.093 | 2.093 |
| macOS | `/System/Library` | gdu | 2.715 | 2.745 | 2.608 | 2.715 |
| macOS | `/System/Library` | dua-cli | 1.654 | 1.891 | 1.845 | 1.845 |
| macOS | `/System/Library` | dust | 2.794 | 2.728 | 2.715 | 2.728 |
| Linux | `/usr` | ncdu (defaults) | 4.822 | 4.949 | 5.331 | 4.949 |
| Linux | `/usr` | ncdu -t 64 | 1.056 | 1.021 | 1.046 | 1.046 |
| Linux | `/usr` | gdu | 1.340 | 1.338 | 1.341 | 1.340 |
| Linux | `/usr` | dua-cli | 1.648 | 1.654 | 1.721 | 1.654 |
| Linux | `/usr` | dust | 1.446 | 1.566 | 1.567 | 1.566 |
| Windows | `C:\Program Files` | minimenta --no-mft | 2.675 | 3.380 | 4.809 | 3.380 |
| Windows | `C:\Program Files` | gdu | 3.610 | 4.435 | 6.344 | 4.435 |
| Windows | `C:\Program Files` | dua-cli | 3.504 | 4.527 | 6.176 | 4.527 |
| Windows | `C:\Program Files` | dust | 6.505 | 7.975 | 10.800 | 7.975 |

<!-- speed-tables:end -->

**What the results show**

- **Linux:** the table of job medians decides the comparison with `ncdu -t 64`. It holds 18 jobs, 6 in each run. The median of the 18 job medians is 1.036, and the interval is 1.002 to 1.066. The run medians are 1.039, 1.012 and 1.037. minimenta was faster in 14 jobs and `ncdu -t 64` in 4. The job medians range from 0.957 to 1.131. By the rule of the table, minimenta is faster than `ncdu -t 64` on a cold `/usr`. The margin is small, and the lower end of the interval is 1.002. We wrote this rule in [docs/experiments.md](docs/experiments.md) before we read any six-job data. The pooled pair table says even (ratio 1.046, middle half 1.00 to 1.11, 217 pairs). The ratio is the median of the ratios of the three runs, 1.056, 1.021 and 1.046. minimenta was faster in 147 pairs and `ncdu -t 64` in 70. The two verdicts answer different questions. The middle half shows the spread of single pairs, and the table of job medians tests the median. The chart shows 4.9x against 4.8x, because both medians come from the second run, where the ratio against `ncdu -t 64` was 1.021. On the warm tree, `ncdu -t 4` is faster than minimenta by the table of job medians (median 0.953, interval 0.936 to 0.971, and `ncdu -t 4` was faster in all 18 jobs). The pooled pair table says even there (ratio 0.953, middle half 0.91 to 1.02).
- **macOS:** the data volume (`/opt/homebrew`) is the main result. On `/System/Library`, minimenta skips empty folders without opening them. That works only on the system volume, and the other tools open every folder, so this tree favors minimenta. Cold macOS runs are short. In the pairs with ncdu defaults, minimenta needed 1.14 s, 0.92 s and 0.65 s for `/opt/homebrew` after `purge`, in the order of the runs. Read these runs as "after purge", not as raw disk speed. On the warm tree, minimenta is faster than `ncdu -t 3` (ratio 1.460, middle half 1.25 to 1.74), and dua-cli is even with minimenta (ratio 1.331, middle half 0.99 to 1.68). The middle half of dua-cli is wide. We did not test why.
- **Windows:** the runner is an administrator, so minimenta reads the NTFS master file table (MFT). The row `minimenta --no-mft` is a run without administrator rights. The chart shows it as "minimenta (no admin)". In its table row, Tool time is the time of the `--no-mft` scan, and minimenta time is the time of the administrator scan. These medians are 13.91 s and 4.06 s. The ratio is 3.380. It is not 13.91 divided by 4.06 (3.43), because the median of ratios is not the ratio of medians. The Windows cold times changed a lot between the runs (see Runs differ below). In run 38065977369, gdu reported a total that was 17.28% lower than the minimenta total on this tree (see Totals check below). On the warm tree, `minimenta --no-mft` and minimenta are even (ratio 0.992). The scan takes about 34 ms, which is less than the 50 ms that pass before the MFT reader can start. On this tree, dust needs 784 ms against 34 ms for minimenta. We did not look into why.
- **Microsoft Defender:** real-time monitoring is off on the Windows runner, and `C:\` and `D:\` are excluded. Endpoint security software inspects every directory open. On machines that run it, opening directories takes a large part of the scan time for every tool, and the difference between minimenta and ncdu becomes smaller.
- **dust and dua-cli:** dust prints a tree and is not interactive. dua-cli exits with code 1 on `/System/Library` in all three runs. minimenta reports 10 read errors there in all three runs. Unreadable folders may be the cause. We did not test it.
- **The 2x goal:** minimenta aims for 2x over ncdu at its best. The cold runs test only one multi-thread setting of ncdu, `-t 64`. Against it, minimenta does not reach 2x in every run. On macOS, the ratio is 1.732, 1.903 and 1.911 on `/opt/homebrew`, and 2.095, 1.947 and 2.093 on `/System/Library`, in the order of the three runs. The medians are 1.903 and 2.093. On `/opt/homebrew`, no run reaches 2x. On Linux, the median of the 18 job medians is 1.036 (interval 1.002 to 1.066), far below 2x.

**Repeat scans on macOS.** A repeat scan reads the cache and lists again only the folders that FSEvents reports as changed. It does not scan the disk again, so the numbers do not compare with the first scans above. They are not part of the three runs.

- On a 10-core Mac with Microsoft Defender, a repeat scan of trees with 210,000 and 413,000 items takes 0.05 s instead of 2 to 4 s for a full scan. This was measured by hand and has no run ID.
- On the macOS runner, a repeat scan of `/opt/homebrew` (200,140 items) takes 23.1 ms (run 37853798602). The first scan of the same tree takes 0.92 s (the median of the three runs above in the pairs with ncdu defaults, on other runners). Those three runs gave 1.14 s, 0.92 s and 0.65 s.
- On the macOS runner, a repeat scan of `/Applications/Xcode.app` (156,994 items) takes 21 ms. It took 1,043 ms before PR #21 (run 37853798602), because every repeat scan listed the folders with hard-linked files again. Now minimenta lists them again only when links may have changed.

A scan of `/` on macOS now counts the data volume once. Before, every file behind a firmlink such as `/Users` counted twice. The scan is shorter and the total is lower (240.3 GiB instead of 442.4 GiB, PR #31, run 37855727004), because it does less work. This is a correctness fix. It is not a faster scanner, and none of the numbers above include it.

**How the numbers were measured**

- **Script and workflow.** [`.github/workflows/speed.yml`](.github/workflows/speed.yml) runs [`bench/compare.py`](bench/compare.py) on GitHub-hosted runners: one for macOS, one for Windows and six for Linux. Each Linux job runs at least 12 pairs, and [`bench/merge_speed.py`](bench/merge_speed.py) pools the pairs of the six jobs into one result for each tool. A second table under such a tree shows the job medians and a 95 percent interval for their true median. The workflow runs when `bench/` changes and on request. [`bench/speed_chart.py`](bench/speed_chart.py) draws the chart and writes the tables from the `speed-merged` artifacts of the runs. gdu, dua-cli, dust and ncdu on Linux come from official release downloads with pinned SHA-256 values ([`bench/install-tools.sh`](bench/install-tools.sh)). ncdu on macOS comes from Homebrew, which builds it from the source release.
- **Pairs.** Each tool runs against minimenta in alternating pairs. The order inside a pair changes from round to round. The script runs as many pairs as the time budget allows. The Pairs column of each table shows how many ran.
- **Commands.** `minimenta --summary`, `ncdu -0 -o /dev/null` (JSON export, default settings), `ncdu -0 -t N -O /dev/null` (binary export), `gdu -n -p -c`, `dua` and `dust -d 0 -P -b -c`. All output goes to the null device. The setting checks run ncdu with the other export format. They are not part of the comparison.
- **Cold disk.** The script drops the operating system file cache before every run. Linux: `sync`, then `echo 3 > /proc/sys/vm/drop_caches`. macOS: `sync`, then `sudo purge`. Windows: it empties the working sets and purges the standby list.
- **The cloud host may still cache the disk.** A first read on a fresh runner was about 8 times slower than a cold run here. The first scan of `/usr` on a fresh Linux runner took minimenta 61.6 s (run 37899170123, no cache drop). Cold scans after a cache drop took a median of 7.9 s (run 37904223270). Both runs used the scanner from before PR #42. So the cold numbers describe a disk behind a host cache. We did not time the other tools on such a first read, so we do not know if the ratios hold there.
- **Warm cache.** The warm tree is the synthetic tree from [`bench/gen_tree.py`](bench/gen_tree.py): 1,000 folders with 50 empty files each (51,110 items). The operating system cache already holds it. `ncdu -t N` uses one thread for each core of the runner.
- **Allocator.** On the small warm tree, the system allocator is faster than mimalloc. In the Linux job of run 37848716077 (4 cores, 30 pairs each), a build with the system allocator needed 0.93 times as long as the mimalloc build with 16 scan threads. With 4 scan threads, it needed 0.83 times as long. On a warm `/usr` (737,881 items, 10 pairs), it needed 1.03 times as long. Large trees are the common case, so minimenta keeps mimalloc.
- **Totals check.** Before it times anything, the script compares the total size that each tool reports with the total of minimenta. In the three runs, one tool reported a total more than 5% lower: gdu on `C:\Program Files` in run 38065977369. gdu reported 43.2 GB, and minimenta reported 52.2 GB of disk use, so gdu was 17.28% lower and the script flagged it. In run 38070763091 and run 38073481635, gdu reported 62.8 GB and 64.9 GB there. The check counted both as the apparent size (2.99% lower and 0.17% higher). We do not know why gdu reported less in run 38065977369, and we did not check if it skipped files. The gdu pairs of that run stay in the tables. On Linux, the lowest total was from gdu on `/usr`: 39.4 GB in job 2 of run 38070763091, which the check counted as the apparent size (3.42% lower). gdu reported 42.4 GB in 11 of the 18 Linux jobs. ncdu, dua-cli and dust were at most 0.53% lower in every tree and run. The check counted the dust totals as the apparent size on Windows in all three runs. On the Windows synthetic tree, dust adds 8.6 MB for the folders themselves.
- **Versions.** The Version column of each table shows them. ncdu is 2.9.2 on macOS (Homebrew) and 2.9.1 on Linux. No static build of ncdu 2.9.2 exists, and ncdu 2.9.2 only fixes a build problem. dua-cli on Linux is the musl build, which is the only x86-64 Linux release file.
- **CI benchmarks.** The benchmark jobs in `ci.yml` ([`bench/cold.sh`](bench/cold.sh), [`bench/throughput.sh`](bench/throughput.sh), [`bench/windows.sh`](bench/windows.sh)) stay as quick checks. The numbers on this page do not come from them.

**Runs differ.** The three runs measured the same commit, but GitHub gave them different runners, and some ratios changed between the runs. macOS got the same Apple M1 (Virtual) runner type in all three runs. Windows got the same AMD64 Family 25 Model 1 CPU type in all three runs. Linux got different CPUs. Run 38065977369 had 3 AMD EPYC 7763, 1 Intel Xeon Platinum 8573C and 2 AMD EPYC 9V45 jobs. Run 38070763091 had 3 AMD EPYC 7763 and 3 AMD EPYC 9V74 jobs. Run 38073481635 had 4 AMD EPYC 7763, 1 AMD EPYC 9V74 and 1 Intel Xeon Platinum 8573C job.

- **Windows:** on the cold scan of `C:\Program Files`, the ratio against gdu is 3.610, 4.435 and 6.344 in the order of the runs. The CPU type was the same in all three runs. We do not know why the ratio changed. gdu needed 14.3 s, 17.8 s and 25.9 s. minimenta needed 4.0 s, 4.0 s and 4.2 s in the same pairs. dua-cli needed 13.6 s, 17.9 s and 25.5 s, dust needed 25.5 s, 31.5 s and 47.5 s, and `minimenta --no-mft` needed 10.4 s, 13.9 s and 20.1 s. The other tools needed much more time in each later run. The runs made 9, 8 and 5 pairs. The warm scans of the synthetic tree changed little. gdu needed 87.1 ms, 87.6 ms and 87.2 ms, and minimenta needed 32.9 ms, 33.8 ms and 34.1 ms in the same pairs. In run 38065977369, gdu reported a total that was 17.28% lower than the minimenta total (see Totals check above).
- **Linux:** ncdu with its defaults needed 32.6 s, 36.7 s and 41.1 s. minimenta needed 7.0 s, 7.3 s and 7.4 s in the same pairs. So the ratio against ncdu with its defaults is 4.822, 4.949 and 5.331. The speed of minimenta in the chart is the median, 4.9x, and the runs give 4.8x, 4.9x and 5.3x. The ratio against gdu moved less: 1.340, 1.338 and 1.341. The ratio against `ncdu -t 64` in the pooled pairs of each run is 1.056, 1.021 and 1.046. The job medians against `ncdu -t 64` range from 0.957 to 1.131. The medians per CPU model are a description and not a test: AMD EPYC 7763 1.027 (10 jobs), AMD EPYC 9V74 1.012 (4 jobs), Intel Xeon Platinum 8573C 1.077 (2 jobs) and AMD EPYC 9V45 1.110 (2 jobs). We did not test the CPU model as a factor.
- **macOS:** the ratio against ncdu with its defaults is 5.083, 5.592 and 4.605 on `/opt/homebrew`, and 5.007, 4.956 and 4.711 on `/System/Library`. On `/opt/homebrew`, ncdu with its defaults needed 5.4 s, 5.1 s and 3.0 s, and minimenta needed 1.14 s, 0.92 s and 0.65 s in the same pairs.

Read every ratio as one sample on the machine types that GitHub gave these runs.

</details>

<details>
<summary>Why minimenta is faster</summary>

- **More threads.** A scan mostly waits on the disk and the kernel, so minimenta uses at least 16 threads. ncdu uses 1 thread unless you pass `-t`.
- **Bulk reads on macOS.** One `getattrlistbulk(2)` call returns the names, types and sizes of many entries at once. ncdu calls `fstatat` for every file. Linux has no such call, so both tools need one `stat` per file there. On a cold Linux disk, reading the directory blocks takes almost all the time, and the order of those reads decides the speed. In the three Speed runs at `a700ff0`, minimenta is faster than `ncdu -t 64` on a cold Linux `/usr` by a small margin (see What the results show). We did not test the reason for this lead.
- **Random order on a cold Linux disk.** Threads that follow the tree read neighbouring folders at the same time, and the disk then serves fewer requests per second. When a scan waits for the disk, minimenta reads the folders in random order. A scan that keeps the CPUs busy, such as a scan of a cached tree, follows the tree. On GitHub runners with a virtual disk, a cold scan of `/usr` took 0.92x (SCSI disk) and 0.89x (NVMe disk) of the time of the scan in tree order. These are A/B runs. The harness times the scanners in alternating pairs in one job. In the same runs, the scan took 0.95x (SCSI, middle half 0.912 to 0.995) and 0.92x (NVMe, middle half 0.881 to 1.011) of the time of `ncdu -t 64`. By the rule of the tables above, that is a small lead on SCSI and even on NVMe. The first three Speed runs (main at `87550d8`, one Linux job each) showed the two even (ratio 1.016, the median of 3 runs. The 21 pairs together gave 1.019). A later test ran the Speed method and the A/B method in the same 12 jobs on a cold `/usr` (131 pairs each, [run 38041108777](https://github.com/OriginalMHV/minimenta/actions/runs/38041108777) and [run 38043852719](https://github.com/OriginalMHV/minimenta/actions/runs/38043852719)). The test was not the real Speed workflow. It did not cover the totals phase, the single runs or the gdu, dua and dust pairs, and its A/B method times adjacent pairs. The Speed method gave 1.056 (middle half 1.007 to 1.110). By the rule of the tables above, that is a small lead. The A/B method gave 1.057 (0.9998 to 1.103). By the same rule, that is even. None of the three differences that we switched (the wait for a quiet disk, the spawn and timing code, and the ncdu default pair before the pair) changed the ratio by 0.03 or more in at least 9 of 12 jobs. The median ratio differed by CPU model in this test. It was 1.04 to 1.05 on EPYC 7763 (9 jobs), 1.07 on Xeon 6973P-C (2 jobs) and 1.13 on Xeon 8573C (1 job). The CPU model, the VM series and the disk type go together here. All 9 EPYC 7763 jobs ran on `Standard_D4ads_v5` VMs with a SCSI disk. The 3 Intel jobs ran on v6 or v7 VMs with an NVMe disk. The test cannot separate the CPU model from the VM series, the disk type and noise. We did not test the CPU model or the disk type. One EPYC 7763 job had `ncdu -t 64` ahead with the Speed method (0.950). A draw of 3 jobs from the test gives a ratio of 1.019 or lower in 8 to 9 of 100 draws. Two EPYC 7763 jobs and one job from any of the 12 give this in 14 of 100 draws. Two EPYC 7763 jobs and one Intel job give it in about 4 of 100 draws. Three EPYC 7763 jobs give it in 15 to 18 of 100 draws. Those Speed runs had two EPYC 7763 jobs and one EPYC 9V45 job, and the test has no EPYC 9V45 job. Those three Speed runs are a small sample. The next test ran the Speed workflow three times with six Linux jobs each at `a700ff0`. The median of the 18 job medians was 1.036, and the interval was 1.002 to 1.066. So the ratio 1.056 of the test above is inside that interval, and the earlier result of 1.019 is inside it too. We did not prove why the earlier gap appeared (see [docs/experiments.md](docs/experiments.md)). We did not measure spinning disks, network drives or local NVMe disks. If a cold scan is slower on such a disk, `--no-spread` reads the folders in tree order.
- **Huge folders on Linux.** One folder with hundreds of thousands of files used to keep one thread busy while the other threads waited. minimenta now splits the stat calls for the later batches of such a folder across threads. On a folder with 200,000 files, minimenta became 2.31x faster when warm and 2.60x faster when cold (run 37854877327, 20 warm pairs and 6 cold pairs). A scan of `/usr` does not change.
- **The master file table on Windows.** As administrator on NTFS, minimenta reads the master file table of the volume in large parallel blocks, as WizTree does, while it lists directories. On a cold disk, this replaces thousands of small reads. The reader starts after 50 ms when the listing is clearly slow (below 40,000 items per second), or later when it is only moderately slow. The first result to finish wins, so a warm scan does not wait for the table. A slow scan (5 s or more) by an administrator whose rights UAC limits ends with a hint to run minimenta as administrator.
- **The cache on macOS.** minimenta keeps the last scan of each folder in `~/Library/Caches/minimenta` and asks FSEvents which directories changed since. It lists only those again. The browser says when it shows a cached scan, and `r` scans everything again. The progress screen says when minimenta checks the cache. The check stops when FSEvents does not answer within half the time of the last full scan, or when too many folders changed. minimenta then scans all folders again, and the screen says so. During that scan, the progress screen estimates the time left. It uses the item count of the cached scan and the speed of the scan so far. A rescan with `r` shows the same estimate on every platform. A first scan has no earlier count, so it shows no estimate.

</details>

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
