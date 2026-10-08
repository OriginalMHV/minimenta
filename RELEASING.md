# Releasing minimenta

This guide is for maintainers. It explains how a release works, what to set up once, and what to do for each release.

## What a release makes

A release makes three things:

1. A GitHub release with archives for five targets (macOS on Apple silicon and Intel, Linux on x86-64 and ARM64, Windows on x86-64), checksums, and two installer scripts. Each archive holds both commands, `minimenta` and `mm`.
2. A Homebrew formula in the tap [OriginalMHV/homebrew-tap](https://github.com/OriginalMHV/homebrew-tap). The formula installs both commands.
3. A crate on [crates.io](https://crates.io/crates/minimenta).

## How it works

You push a tag such as `v0.1.0`. The tag starts `.github/workflows/release.yml`. [cargo-dist](https://axodotdev.github.io/cargo-dist/) (version 0.33.0) generates that file from `dist-workspace.toml`. The workflow runs these jobs in order:

1. `plan` reads the tag and the configuration, and decides what to build.
2. `build-local-artifacts` builds the archives. It runs once for each of the five targets, each on a runner of that platform. It uses the `dist` profile in `Cargo.toml`, which keeps fat LTO.
3. `build-global-artifacts` makes the checksums, the installer scripts, and the Homebrew formula.
4. `host` creates the GitHub release and uploads all files. The release notes come from the matching section of `CHANGELOG.md`.
5. `publish-homebrew-formula` commits the formula to the tap. It needs the `HOMEBREW_TAP_TOKEN` secret.
6. `custom-publish-crates` runs `.github/workflows/publish-crates.yml`. It publishes the crate to crates.io with trusted publishing. It skips a version that crates.io already has.
7. `announce` ends the run.

A tag with a suffix, such as `v0.2.0-rc.1`, makes a GitHub prerelease. The two publish jobs do not run for a prerelease.

The same workflow runs on every pull request, but only the `plan` job runs. It checks that `release.yml` matches `dist-workspace.toml`.

## First release

The first release needs extra steps, because crates.io needs a crate to exist before it accepts trusted publishing. Do the steps in this order.

1. Merge the pull request that sets up the releases. Trusted publishing in step 6 needs `release.yml` on the default branch.
2. Merge the pull request that updates the install instructions in the README. The README text that you publish in step 5 is the text that crates.io shows for 0.1.0, and a later change does not update it.
3. Prepare the changelog:
   1. Create a branch `release/v0.1.0` from an up-to-date `main`.
   2. In `CHANGELOG.md`, delete the sentence "minimenta has no release yet. The Unreleased section lists what version 0.1.0 contains."
   3. Change the heading `## [Unreleased]` to `## [0.1.0] - YYYY-MM-DD`. Use the date of the release. Add a new empty `## [Unreleased]` section above it.
   4. Add these links at the end of the file:

      ```text
      [Unreleased]: https://github.com/OriginalMHV/minimenta/compare/v0.1.0...HEAD
      [0.1.0]: https://github.com/OriginalMHV/minimenta/releases/tag/v0.1.0
      ```

   5. Open a pull request with the title `chore: release v0.1.0`. Wait for CI. Squash-merge it.
4. Create the `HOMEBREW_TAP_TOKEN` secret:
   1. Create a fine-grained personal access token on GitHub. Give it access to the repository `OriginalMHV/homebrew-tap` only, with the permission "Contents: Read and write". If the token of the Ward repository already has this access, you can use the same token.
   2. Open the repository settings of minimenta. Go to "Secrets and variables", then "Actions". Add a repository secret named `HOMEBREW_TAP_TOKEN` with the token as its value.
5. Publish 0.1.0 to crates.io by hand. Use the commit that you merged in step 3.
   1. Log in to crates.io with GitHub. Verify your email address in the account settings.
   2. Create an API token with the scope `publish-new` and the crate pattern `minimenta`.
   3. Check out `main` and make sure the working tree is clean.
   4. Run the dry run and look at the package size and file list:

      ```sh
      cargo publish --dry-run --locked
      ```

   5. Publish. The `read` command keeps the token out of your shell history:

      ```sh
      read -rs CARGO_REGISTRY_TOKEN && export CARGO_REGISTRY_TOKEN
      cargo publish --locked
      unset CARGO_REGISTRY_TOKEN
      ```

   6. Revoke the API token on crates.io. You do not need it again.
6. Set up trusted publishing on crates.io:
   1. Open <https://crates.io/crates/minimenta/settings>. You must be an owner of the crate.
   2. Under "Trusted Publishing", add a GitHub publisher.
   3. Enter these values:

      | Field | Value |
      |---|---|
      | Repository owner | `OriginalMHV` |
      | Repository name | `minimenta` |
      | Workflow filename | `release.yml` |
      | Environment | Leave empty, or enter `release` (see step 7) |

   The workflow filename is `release.yml`, not `publish-crates.yml`. crates.io checks the workflow file that starts the run. `release.yml` starts the run and calls `publish-crates.yml`.
7. Optional: add a manual approval before each crates.io publish.
   1. Open the repository settings of minimenta. Go to "Environments" and open the environment `release`. GitHub creates it when the publish job runs for the first time. You can also create it now.
   2. Add yourself under "Required reviewers".
   3. On crates.io, set the environment of the trusted publisher to `release`.
8. Create the tag on the commit that you published, and push it:

   ```sh
   git switch main
   git pull --ff-only
   git tag -s v0.1.0 -m "v0.1.0"
   git push origin v0.1.0
   ```

   The tag name must be `v` followed by the version in `Cargo.toml`. If the name differs, cargo-dist stops the run.
9. Watch the run. The `custom-publish-crates` job finds 0.1.0 on crates.io and publishes nothing. Then verify the release (see "Verify a release").

## Later releases

1. Start from an up-to-date `main` with a clean working tree. Create a branch `release/vX.Y.Z`.
2. Choose the version. While the version is below 1.0.0, a breaking change raises the minor number and other changes raise the patch number.
3. Set `version` in `Cargo.toml`. Run `cargo update --workspace` to update `Cargo.lock`.
4. Update `CHANGELOG.md`:
   1. Add a new empty `## [Unreleased]` section above the changes.
   2. Change the heading of the changes to `## [X.Y.Z] - YYYY-MM-DD`.
   3. Update the two links at the end of the file. Point `[Unreleased]` to `compare/vX.Y.Z...HEAD`. Add `[X.Y.Z]` with the link `compare/vA.B.C...vX.Y.Z`, where A.B.C is the previous version.
5. Run the checks:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   cargo deny check
   cargo publish --dry-run --locked
   ```

6. Open a pull request with the title `chore: release vX.Y.Z`. Wait for CI. Squash-merge it.
7. Create the signed tag on the merge commit, and push it:

   ```sh
   git switch main
   git pull --ff-only
   git tag -s vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

8. Watch the run in the Actions tab, or run `gh run watch`. The run publishes to GitHub, Homebrew, and crates.io.
9. Verify the release.

## Verify a release

1. Open the GitHub release. It must list five archives with `.sha256` files, `minimenta-installer.sh`, `minimenta-installer.ps1`, `minimenta.rb`, `sha256.sum`, and `source.tar.gz`. The notes must match the changelog section.
2. Check Homebrew. The tap must have a new commit named `minimenta X.Y.Z`. Then run:

   ```sh
   brew update
   brew install OriginalMHV/tap/minimenta
   minimenta --version
   mm --version
   ```

   Both commands must print the new version.
3. Check Cargo. Run this on a machine with a C compiler:

   ```sh
   cargo install minimenta --version X.Y.Z
   minimenta --version
   mm --version
   ```

4. Check the installer scripts:

   ```sh
   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/OriginalMHV/minimenta/releases/latest/download/minimenta-installer.sh | sh
   ```

   On Windows, run this in PowerShell:

   ```powershell
   powershell -ExecutionPolicy Bypass -c "irm https://github.com/OriginalMHV/minimenta/releases/latest/download/minimenta-installer.ps1 | iex"
   ```

5. Open <https://crates.io/crates/minimenta>. The README must show its images. Open <https://docs.rs/minimenta> and check that the build passes.

## When a step fails

- **A build job fails.** No release exists yet, because `host` runs after all builds. Fix the problem on `main`. Delete the tag with `git push origin :refs/tags/vX.Y.Z` and `git tag -d vX.Y.Z`. Create the tag again on the new commit.
- **The Homebrew job fails.** The usual cause is a missing or expired `HOMEBREW_TAP_TOKEN`. Fix the secret. Then run `gh run rerun RUN_ID --failed`.
- **The crates.io job fails with an authentication error.** Check the trusted publisher on crates.io. The owner, repository, and workflow filename (`release.yml`) must match exactly. If the job ran before you published 0.1.0 by hand, publish by hand first, then re-run the failed job.
- **A published version has a defect.** A published version cannot be replaced. Run `cargo yank --version X.Y.Z` if the version is harmful, and release a new patch version.

## Maintenance

- **Change the dist configuration.** Edit `dist-workspace.toml`, then run `dist generate`. Commit the new `release.yml`. Never edit `release.yml` by hand. The `plan` job fails when the file differs from the configuration.
- **Update cargo-dist.** Install the new version of `dist`, change `cargo-dist-version` in `dist-workspace.toml`, and run `dist init`. Review the diff of `release.yml`.
- **Test all targets on a pull request.** Set `pr-run-mode = "upload"` in `dist-workspace.toml` and run `dist generate`. Every build job then runs on the pull request. Set it back to `"plan"` before you merge.
- **Raise the minimum Rust version.** Change `rust-version` in `Cargo.toml`. The MSRV workflow reads that value. Add a note under "Changed" in the changelog.
