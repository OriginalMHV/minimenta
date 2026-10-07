<p align="center">
  <img src="docs/assets/minimenta-hero.svg" alt="minimenta. scan, select, shed. Minuere impedimenta. Find what fills your disk, and let it go." width="100%">
</p>

<p align="center">
  <a href="https://github.com/OriginalMHV/minimenta/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/OriginalMHV/minimenta/ci.yml?branch=main&style=flat-square&labelColor=59636E&color=1A7F5A&label=CI" alt="CI status"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-2024%20edition-1A7F5A?style=flat-square&labelColor=59636E" alt="Rust 2024 edition"></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-1A7F5A?style=flat-square&labelColor=59636E" alt="Runs on macOS and Linux">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-1A7F5A?style=flat-square&labelColor=59636E" alt="MIT or Apache-2.0 license"></a>
</p>

minimenta is an interactive disk usage analyzer for the terminal, written in Rust. It looks and feels like ncdu, scans faster, and lets you select many items and move them to the Trash in one step.

## Why minimenta

- **A start prompt.** Run `minimenta` without a folder. The prompt shows the current folder. Enter scans it, and Tab completes folder names.
- **Multi-select.** Space selects one item. Shift+Up/Down (or `K`/`J`) selects a range. Ctrl+A selects all items in the folder, and Esc clears the selection.
- **The Trash first.** `d` moves the selection to the Trash. On macOS, minimenta asks Finder to do it, so "Put Back" works. If Finder cannot be controlled, minimenta uses the file manager API instead. `D` deletes permanently. Both keys ask for confirmation first.
- **The ncdu look and keys.** The same layout, size bars, file flags, sort keys and `-x` option. The keys that both tools have work the same, except `d`, which moves items to the Trash.
- **Bulk directory reads.** On macOS, one `getattrlistbulk(2)` call returns the names, types and sizes of many entries at once. A parallel scanner reads directories on all cores.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Terminal recording: the start prompt scans the home folder, Downloads opens, J (the same as Shift+Down) selects three large files, D and y delete them, and the Downloads total drops from 4.0 GiB to 782 MiB" width="100%">
</p>

## Install

```sh
cargo install --git https://github.com/OriginalMHV/minimenta
```

minimenta runs on macOS and Linux. Building it needs a recent stable Rust toolchain.

## Usage

```sh
minimenta                    # ask which folder to scan
minimenta ~/code             # scan ~/code at once
minimenta --summary ~/code   # print the totals without the interface
```

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
| `s` / `n` / `C` | Sort by size / name / items (press again to reverse) |
| `a` | Show apparent size or disk usage |
| `r` | Rescan the current folder |
| `?` | Help |
| `q` | Quit |

## Options

| Option | Effect |
| --- | --- |
| `-x`, `--one-file-system` | Do not cross file system boundaries |
| `-t N`, `--threads N` | Use N scan threads (default: the number of CPU cores) |
| `--summary` | Scan, print the totals, and exit |
| `-h`, `--help` | Print the help |
| `-V`, `--version` | Print the version |

## Speed

The goal is a scan that is 2x faster than ncdu at its best, which is `ncdu -t <cores>`. This work is in progress, and minimenta does not reach the goal yet.

<p align="center">
  <img src="docs/assets/speed.svg" alt="Scan speed compared with ncdu -t 3: ncdu 1.00x, minimenta 1.26x, goal 2.00x. Median of 3 rounds from bench/throughput.sh in the CI benchmark job on a GitHub macOS runner." width="600">
</p>

[`bench/throughput.sh`](bench/throughput.sh) scans a fixed synthetic tree of about 51,000 items with both tools. It runs 3 rounds of 30 runs and reports the median speedup. The CI benchmark job runs it on a GitHub macOS runner with 3 cores. The graphic shows the run for commit `8e62eb7`: the rounds measured 1.25x, 1.26x and 1.50x, and with one thread each minimenta was 1.66x faster. Earlier runs measured from 1.01x to 1.69x, because the speed of shared runners varies. To measure on your own machine, install ncdu, hyperfine and jq, then run `bench/throughput.sh`.

On macOS, minimenta reads each directory with one `getattrlistbulk(2)` call, which returns the names, types and sizes of all entries at once. ncdu calls `fstatat` for every file.

Endpoint security software (for example Microsoft Defender) inspects every directory open. On such machines, opening directories dominates the scan time for every tool, and the difference between minimenta and ncdu becomes smaller.

## minimenta compared with other tools

<p align="center">
  <img src="docs/assets/comparison.svg" alt="Comparison of minimenta, ncdu, gdu, dua-cli and dust. Interactive browser: all except dust. Parallel scan by default: all, ncdu partial. Select several items at once: minimenta, gdu and dua-cli. Move items to the Trash: minimenta, gdu and dua-cli, ncdu partial. Export the scan to a file: all except minimenta." width="100%">
</p>

Compared on 2026-10-07 with each project's README, manual or source. Partial means that the tool does part of the row:

- ncdu scans in parallel only with `-t`. The default is one thread.
- ncdu moves items to the Trash only through `--delete-command`, for example `ncdu --delete-command 'gio trash --'`.

<details>
<summary>Comparison as text</summary>

| Feature | minimenta | ncdu | gdu | dua-cli | dust |
| --- | --- | --- | --- | --- | --- |
| Interactive browser | Yes | Yes | Yes | Yes (`dua i`) | No |
| Parallel scan by default | Yes | Partial (`-t`) | Yes | Yes | Yes |
| Select several items at once | Yes | No | Yes (Space) | Yes (Space) | No |
| Move items to the Trash | Yes | Partial (`--delete-command`) | Yes (`D`) | Yes (Ctrl+T) | No |
| Export the scan to a file | No | Yes (`-o`, `-O`) | Yes (`-o`) | Yes (`--export`) | Yes (`-j`) |

Sources: the [ncdu manual](https://dev.yorhel.nl/ncdu/man) and [home page](https://dev.yorhel.nl/ncdu), the [gdu README](https://github.com/dundee/gdu) and its [help screen source](https://github.com/dundee/gdu/blob/master/tui/show.go), the [dua-cli README](https://github.com/Byron/dua-cli), [key bindings](https://github.com/Byron/dua-cli/blob/main/src/config.rs) and [options](https://github.com/Byron/dua-cli/blob/main/src/options.rs), and the [dust README](https://github.com/bootandy/dust).

</details>

dua-cli and gdu do more than minimenta, for example saved scans and more platforms. dust prints a tree and has no interactive mode. Choose minimenta when you want the ncdu look and keys together with multi-select and the Trash.

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
