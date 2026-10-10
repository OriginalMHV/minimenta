# Contributing to minimenta

Thank you for your interest. This guide explains how to build minimenta, check a change, and send it.

All contributors must follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Setup

```sh
git clone https://github.com/OriginalMHV/minimenta.git
cd minimenta
cargo build
```

You need Rust 1.88 or newer and a C compiler. The C compiler builds the mimalloc allocator. To build without mimalloc, add `--no-default-features` to each cargo command.

Run the program from the checkout:

```sh
cargo run -- ~/code
cargo run -- --summary ~/code
```

## Workflow

1. Fork the repository and create a branch from `main`.
2. Make your change.
3. Run the checks in the next section.
4. Open a pull request against `main`.

## Before You Submit

Run these five commands:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features --target x86_64-unknown-linux-gnu -- -D warnings
cargo clippy --all-targets --no-default-features --target x86_64-pc-windows-msvc -- -D warnings
cargo test
```

The two commands with `--target` check the Linux and Windows code on any machine. Without them, clippy skips the code for the other systems. If a target is missing, run `rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc`. The option `--no-default-features` leaves out mimalloc, so these checks need no C cross compiler.

CI runs `cargo fmt`, clippy and the tests on macOS, Linux and Windows. It does not run the two commands with `--target`, so run them yourself. CI also runs these jobs:

- **MSRV** builds with the Rust version in `rust-version` in `Cargo.toml`. Do not use a newer language or library feature unless you raise `rust-version` in the same pull request.
- **Package** runs `cargo publish --dry-run --locked` on Linux. It also builds the documentation with `--no-default-features`, as docs.rs does.
- **cargo-deny** checks advisories, licenses, and sources. `deny.toml` lists the allowed licenses. Run `cargo deny check` locally if you add or update a dependency.
- **CodeQL** scans the Rust code and the workflow files.

Clippy runs with `clippy::pedantic`. Fix each warning. If an `allow` is the right choice, write a short comment that says why.

## Commit Convention

Use [Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` for a new feature
- `fix:` for a bug fix
- `perf:` for a change that makes minimenta faster
- `refactor:` for a change that neither fixes a bug nor adds a feature
- `docs:` for documentation only
- `test:` for tests only
- `chore:` for maintenance, dependencies, and CI

## Pull Requests

- Make one change per pull request.
- Use a title that follows the commit convention.
- Link the related issue in the description.
- Explain why you make the change, not only what it does.
- Keep the change small.

## Benchmarks

Before you try a performance idea, read [docs/experiments.md](docs/experiments.md). It lists what we tried, what worked, and what we dropped, with the evidence. Add a row to it for every experiment you run, kept or dropped, in the same pull request or in a docs pull request right after.

minimenta aims to be fast, so a speed claim needs a fair measurement. Follow these rules for each number you put in a pull request, an issue, or the README:

1. **Run the two commands in alternating pairs.** Use [`bench/interleave.py`](bench/interleave.py). It swaps the order in every pair and reports the median of the time ratios. A machine that slows down for a while then slows both commands.
2. **Report the median and the spread.** Give the median ratio, the middle half of the ratios, and the number of pairs.
3. **Compare with the other tool at its best.** For a warm scan, run `ncdu -t <cores>`. For a cold scan, also state the result against `ncdu` with its default settings, and say which one you used.
4. **Drop the file cache for a cold scan.** [`bench/cold.sh`](bench/cold.sh) does this before every run. A cold scan and a warm scan are different measurements. Never mix them in one number.
5. **Show cache gains on their own.** A repeat scan that uses the macOS cache is not comparable with a first scan of ncdu. Give it its own row.
6. **Name the machine.** State the operating system, the number of cores, the tree, and the item count. Endpoint security software such as Microsoft Defender slows every directory open and makes the difference smaller.
7. **Keep the run short.** Use a small tree that does not change. [`bench/throughput.sh`](bench/throughput.sh) scans a synthetic tree of about 51,000 items in 60 alternating pairs. Time one pair first. Do not start an open-ended run on a large real tree.
8. **Report results that go against minimenta.** The README already says where ncdu is faster. Do not remove a result because it looks bad.

The CI benchmark jobs run on GitHub runners without endpoint security. Use them to judge a change that affects speed.

## Project Layout

| Path | What lives there |
|---|---|
| `src/lib.rs`, `src/main.rs`, `src/bin/mm.rs` | The command line, and the two binaries `minimenta` and `mm` |
| `src/scan/` | The scanners for macOS, Linux, Windows, and the NTFS master file table |
| `src/ui/` | The start prompt, the browser, the progress view, and the Trash |
| `src/cache.rs`, `src/fsevents.rs` | The scan cache and FSEvents (macOS) |
| `src/tree.rs` | The tree of directories and entries |
| `bench/` | The benchmark scripts |
| `docs/` | The images for the README, the demo recording, and the [log of performance experiments](docs/experiments.md) |

## Releases

Maintainers release with one command from a clean `main`:

```sh
scripts/release.sh 0.1.0
```

The script opens a release PR that dates the CHANGELOG (and bumps the version after 0.1.0), waits for CI, and merges it. It then pushes a signed tag, which makes cargo-dist build the GitHub release and update the Homebrew tap. Last, it publishes the crate to crates.io. It asks before each step that cannot be undone. Run it again with the same version to continue after a failure. [RELEASING.md](RELEASING.md) has the details and the setup. [cargo-dist](https://axodotdev.github.io/cargo-dist/) generates `.github/workflows/release.yml`. Do not edit that file by hand. The release plan check fails when the file differs from the dist configuration. Dependabot skips the file, so update its actions through cargo-dist.

## License

minimenta is dual licensed as MIT OR Apache-2.0. Unless you state otherwise, any contribution that you intentionally submit for inclusion in minimenta, as defined in the Apache-2.0 license, is dual licensed as MIT OR Apache-2.0, without any additional terms or conditions.

## Questions

Open an issue. All questions are welcome.
