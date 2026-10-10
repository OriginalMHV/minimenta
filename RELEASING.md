# Releasing minimenta

This guide is for maintainers. It explains how a release works, what to set up once, and what to do for each release. A release works the same way as in [Ward](https://github.com/OriginalMHV/Ward).

## What a release makes

A release makes three things:

1. A GitHub release with archives for five targets (macOS on Apple silicon and Intel, Linux on x86-64 and ARM64, Windows on x86-64), checksums, and two installer scripts. Each archive holds both commands, `minimenta` and `mm`.
2. A Homebrew formula in the tap [OriginalMHV/homebrew-tap](https://github.com/OriginalMHV/homebrew-tap). The formula installs both commands.
3. A crate on [crates.io](https://crates.io/crates/minimenta).

## How it works

`scripts/release.sh` does the work. It opens a release pull request, pushes a signed tag such as `v0.1.0`, and publishes the crate to crates.io from your machine.

The tag starts `.github/workflows/release.yml`. [cargo-dist](https://axodotdev.github.io/cargo-dist/) (version 0.33.0) generates that file from `dist-workspace.toml`. The workflow runs these jobs:

1. `plan` reads the tag and the configuration, and decides what to build.
2. `build-local-artifacts` builds the archives. It runs once for each of the five targets, each on a runner of that platform. It uses the `dist` profile in `Cargo.toml`, which keeps fat LTO.
3. `build-global-artifacts` makes the checksums, the installer scripts, and the Homebrew formula.
4. `host` creates the GitHub release and uploads all files. The release notes come from the matching section of `CHANGELOG.md`.
5. `publish-homebrew-formula` commits the formula to the tap. It needs the `HOMEBREW_TAP_TOKEN` secret.
6. `announce` ends the run.

The workflow does not publish to crates.io. The script runs `cargo publish --locked` after the workflow succeeds.

The same workflow runs on every pull request, but only the `plan` job runs. It checks that `release.yml` matches `dist-workspace.toml`. The `Package` workflow runs `cargo publish --dry-run --locked` on every pull request, so a packaging problem shows up before the release and not after it.

## Set up once

1. Create the `HOMEBREW_TAP_TOKEN` secret:
   1. Create a fine-grained personal access token on GitHub. Give it access to the repository `OriginalMHV/homebrew-tap` only, with the permission "Contents: Read and write". If you made a token with this access for Ward and still have it, you can use it again.
   2. Open the repository settings of minimenta. Go to "Secrets and variables", then "Actions". Add a repository secret named `HOMEBREW_TAP_TOKEN` with the token as its value.
2. Give Cargo a crates.io token:
   1. Log in to [crates.io](https://crates.io) with GitHub. Verify your email address in the account settings.
   2. Create an API token with the scopes `publish-new`, `publish-update` and `yank`. Limit it to the crate pattern `minimenta`. The first release creates the crate, so it needs `publish-new`. `yank` lets you withdraw a bad version (see "When a step fails"), and `cargo yank --undo` reverses it. A token that is limited to `ward-cli` cannot publish minimenta.
   3. Run `cargo login` and paste the token. Cargo keeps it in `~/.cargo/credentials.toml`.
3. Check that private vulnerability reporting is on. `SECURITY.md`, `CODE_OF_CONDUCT.md` and the issue forms send people to the private report form of GitHub. Run `gh api repos/OriginalMHV/minimenta/private-vulnerability-reporting --jq .enabled`. It must print `true`. If it prints `false`, open the repository settings, go to "Advanced Security", and enable "Private vulnerability reporting".
4. Recommended: create a tag ruleset for the pattern `v*`. Go to "Rules", then "Rulesets", then "New tag ruleset". Restrict creations, updates and deletions. Add the role "Repository admin" to the bypass list. Then only you can start a release.

The script checks item 1 and stops when the secret is missing. It warns when it finds no token for item 2.

## Before a release

crates.io shows the README of each version as it was at the publish, and a later change does not update it. Merge open pull requests that change the README first.

Before the first release, merge the pull request that adds the install instructions for the release to the README. The script stops when the README on `main` has no Homebrew install line.

## Release

1. Check out `main`. Make sure that you have no uncommitted changes and no commits that are not on GitHub. Untracked files do not matter, unless they are in the package: `src`, `Cargo.lock`, `CHANGELOG.md`, `README.md` and the license files. `cargo publish` refuses such a file, also an ignored one such as `src/.DS_Store`. The script stops before the tag when it finds one.
2. Choose the version. While the version is below 1.0.0, a breaking change raises the minor number and other changes raise the patch number. The first release is 0.1.0, the version that `Cargo.toml` already has.
3. Run the script:

   ```sh
   scripts/release.sh 0.1.0
   ```

The script does these steps. It asks before each step that cannot be undone. Add `--yes` to skip the questions.

1. It checks the setup.
2. It creates the branch `release/vX.Y.Z` and updates `CHANGELOG.md`:
   - The heading `## [Unreleased]` stays, empty. The changes get the heading `## [X.Y.Z] - YYYY-MM-DD`, with the date of today.
   - The links at the end of the file point to the new version.
   - For the first release, the script removes the note "minimenta has no release yet", moves the rest of that note under the new heading, and adds the links.
   - For a later release, the script also sets `version` in `Cargo.toml` and updates `Cargo.lock`.
3. It runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check` (when cargo-deny is installed) and `cargo publish --dry-run --locked`. The first run on a machine compiles everything and takes several minutes.
4. It commits, pushes, and opens the pull request `chore: release vX.Y.Z`. It waits for CI, asks, and squash-merges the pull request.
5. It checks that the package has no untracked or ignored files. Then it asks, creates the signed tag `vX.Y.Z` on the head of `main`, and pushes it.
6. It waits for the release workflow (about 10 minutes) and checks the GitHub release and the Homebrew formula.
7. It asks, then runs `cargo publish --locked`.

If a step fails, fix the cause and run the script again with the same version. It skips the steps that are done.

## Release by hand

Use these steps when you cannot use the script.

1. Create a branch `release/vX.Y.Z` from an up-to-date `main`.
2. Update `CHANGELOG.md` and, for a later release, the version in `Cargo.toml` and `Cargo.lock` (`cargo update --workspace`), as the script does in its step 2.
3. Run the checks of step 3 of the script.
4. Open a pull request with the title `chore: release vX.Y.Z`. Wait for CI. Squash-merge it.
5. Create the signed tag on the head of `main`, and push it:

   ```sh
   git switch main
   git pull --ff-only
   git tag -s vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

   The tag name must be `v` followed by the version in `Cargo.toml`. If the name differs, cargo-dist stops the run.
6. Wait until the release workflow succeeds. Run `gh run watch`, or look in the Actions tab.
7. Publish the crate from the tagged commit:

   ```sh
   cargo publish --locked
   ```

## Verify a release

1. Open the GitHub release. It must list these 17 files. The notes must match the changelog section.
   - Five archives, each with a `.sha256` file. The archive for Windows (`minimenta-x86_64-pc-windows-msvc`) is a `.zip` file. The other four archives are `.tar.xz` files.
   - The installers `minimenta-installer.sh` and `minimenta-installer.ps1`.
   - The Homebrew formula `minimenta.rb`.
   - The checksum list `sha256.sum`.
   - The source archive `source.tar.gz` and its `source.tar.gz.sha256`.
   - The build manifest `dist-manifest.json`.
2. Check Homebrew. The tap must have a new commit named `minimenta X.Y.Z`. Then run:

   ```sh
   brew update
   brew install OriginalMHV/tap/minimenta
   minimenta --version
   mm --version
   ```

   Both commands must print the new version. If you installed minimenta with `cargo install` before, the command that runs first in your `PATH` wins. Run `which -a minimenta` to see both.
3. Check Cargo. Run this on a machine with a C compiler:

   ```sh
   cargo install --locked minimenta --version X.Y.Z
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

- **CI fails on the release pull request.** Push a fix to the branch `release/vX.Y.Z`. Or fix `main`, close the pull request, and delete the branch on GitHub and on your machine (`git push origin --delete release/vX.Y.Z` and `git branch -D release/vX.Y.Z`). Then run the script again. It prepares the release again with the date of that day.
- **A `plan` or build job fails.** No release exists yet, because `host` runs after all builds. Fix the problem on `main`. Delete the tag with `git push origin :refs/tags/vX.Y.Z` and `git tag -d vX.Y.Z`. Run the script again. It creates the tag on the new head of `main` and watches only the new run of the workflow.
- **The `host` job fails.** Open the release page of `vX.Y.Z`. If a release exists with missing files, delete it with `gh release delete vX.Y.Z`. Then delete the tag as in the previous item, and run the script again. A rerun of the failed job does not work, because the release already exists.
- **The Homebrew job fails.** The usual cause is a missing or expired `HOMEBREW_TAP_TOKEN`. The job runs after `host`, so the GitHub release is already public with all files. `brew install` does not work until the tap has the formula. Fix the secret. Then run `gh run rerun RUN_ID --failed`. This runs the failed job only and completes the tap update. Then run the script again for the crates.io step.
- **`cargo publish` fails.** The GitHub release and the Homebrew formula already exist. For a token problem, create a new token (see "Set up once") and run `cargo login`. Then run the script again. It continues with `cargo publish`. If the package itself is wrong, fix it on `main` and release the next patch version.
- **A published version has a defect.** A published version cannot be replaced. Run `cargo yank --version X.Y.Z` if the version is harmful, and release a new patch version.

## Maintenance

- **Change the dist configuration.** Edit `dist-workspace.toml`, then run `dist generate` with dist 0.33.0. Commit the new `release.yml`. Never edit `release.yml` by hand. The `plan` job fails when the file differs from the configuration.
- **Update cargo-dist.** Install the new version of `dist`, change `cargo-dist-version` in `dist-workspace.toml`, and run `dist init`. Review the diff of `release.yml`.
- **Test all targets on a pull request.** Set `pr-run-mode = "upload"` in `dist-workspace.toml` and run `dist generate`. Every build job then runs on the pull request. Set it back to `"plan"` before you merge.
- **Raise the minimum Rust version.** Change `rust-version` in `Cargo.toml`. The MSRV workflow reads that value. Add a note under "Changed" in the changelog.
