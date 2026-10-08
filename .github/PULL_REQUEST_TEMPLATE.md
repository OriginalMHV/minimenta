## What does this PR do?

<!-- Describe the change and why you make it. -->

## Related Issue

<!-- Link the issue that this PR addresses, for example: Closes #123 -->

## Checklist

- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo clippy --all-targets --no-default-features --target x86_64-unknown-linux-gnu -- -D warnings` passes
- [ ] `cargo clippy --all-targets --no-default-features --target x86_64-pc-windows-msvc -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] I added or updated tests for the change
- [ ] I updated the README or `CHANGELOG.md` if users see the change
- [ ] For a speed claim: I followed the benchmark rules in `CONTRIBUTING.md` and named the machine
