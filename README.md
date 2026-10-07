# minimenta

*Minuere impedimenta.* Reduce the baggage.

An interactive disk usage analyzer for the terminal, written in Rust. It looks and feels like ncdu, scans faster, and lets you select and delete many items at once.

## Install

```sh
cargo install --git https://github.com/OriginalMHV/minimenta
```

## Use

```sh
minimenta          # asks which directory to scan; Enter scans the current one
minimenta ~/code   # scans ~/code at once
```

| Keys | Action |
| --- | --- |
| Up, k / Down, j | Move the cursor |
| Shift+Up/Down, K / J | Extend the selection |
| Space | Select or deselect, then move down |
| Ctrl+A / Esc | Select all / clear the selection |
| Enter, Right, l | Open the directory |
| Left, h, Backspace | Go to the parent directory |
| d | Move the selection (or the item under the cursor) to the Trash |
| D | Delete permanently |
| s / n / C | Sort by size / name / items (press again to reverse) |
| a | Show apparent size or disk usage |
| r | Rescan the current directory |
| ? | Help |
| q | Quit |

Options: `-x` stays on one file system, `-t N` sets the number of scan threads, and `--summary` prints the totals without the interface.

## Speed

On macOS, minimenta reads each directory with one `getattrlistbulk(2)` call, which returns the names, types, and sizes of all entries at once. ncdu calls `fstatat` for every file. Directories are scanned in parallel on all cores.

`bench/throughput.sh` compares the scan throughput with ncdu on a fixed synthetic tree. The CI benchmark job runs it on a GitHub macOS runner.

Endpoint security software (for example Microsoft Defender) inspects every directory open. On such machines, opening directories dominates the scan time for every tool, and the difference between minimenta and ncdu becomes smaller.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
