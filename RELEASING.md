# Releasing minimenta

This guide is for maintainers. It explains how a release works, what to set up once, and what to do for each release.

## What a release makes

A release makes three things:

1. A GitHub release with archives for five targets (macOS on Apple silicon and Intel, Linux on x86-64 and ARM64, Windows on x86-64), checksums, and two installer scripts. Each archive holds both commands, `minimenta` and `mm`.
2. A Homebrew formula in the tap [OriginalMHV/homebrew-tap](https://github.com/OriginalMHV/homebrew-tap). The formula installs both commands.
3. A crate on [crates.io](https://crates.io/crates/minimenta).

## How it works

You push a tag such as `v0.1.0`. The tag starts `.github/workflows/release.yml`. [cargo-dist](https://axodotdev.github.io/cargo-dist/) (version 0.33.0) generates that file from `dist-workspace.toml`. The workflow runs these jobs:

1. `plan` reads the tag and the configuration, and decides what to build.
2. `build-local-artifacts` builds the archives. It runs once for each of the five targets, each on a runner of that platform. It uses the `dist` profile in `Cargo.toml`, which keeps fat LTO.
3. `build-global-artifacts` makes the checksums, the installer scripts, and the Homebrew formula.
4. `host` creates the GitHub release and uploads all files. The release notes come from the matching section of `CHANGELOG.md`.
5. `publish-homebrew-formula` commits the formula to the tap. It needs the `HOMEBREW_TAP_TOKEN` secret.
6. `custom-publish-crates` runs `.github/workflows/publish-crates.yml`. It publishes the crate to crates.io with trusted publishing. It skips a version that crates.io already has.
7. `announce` ends the run.

Jobs 5 and 6 start together after `host`. They do not depend on each other, so one can fail while the other succeeds. `announce` waits for both.

The workflow in job 6 has two jobs:

- `verify` checks that `Cargo.toml` has the version of the tag, checks whether crates.io has that version, and builds the package with `cargo publish --dry-run --locked`. The build scripts of the dependencies run in this job. The job has no permission to ask GitHub for an OIDC token.
- `publish` gets a short-lived crates.io token and runs `cargo publish --locked --no-verify`. It runs only when crates.io does not have the version yet. It uses the environment `release`, so it waits for your approval. It runs no dependency code. Its actions are pinned to a commit.

A tag with a suffix, such as `v0.2.0-rc.1`, makes a GitHub prerelease. The two publish jobs do not run for a prerelease.

The same workflow runs on every pull request, but only the `plan` job runs. It checks that `release.yml` matches `dist-workspace.toml`. The `Package` workflow runs `cargo publish --dry-run --locked` on every pull request, so a packaging problem shows up before the release and not after it.

## First release

The first release needs extra steps, because crates.io needs a crate to exist before it accepts trusted publishing. Do the steps in this order.

1. Merge the pull request that sets up the releases. Trusted publishing in step 8 needs `release.yml` on the default branch.
2. Merge the pull request that updates the install instructions in the README. The README text that you publish in step 7 is the text that crates.io shows for 0.1.0, and a later change does not update it.
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
5. Turn on private vulnerability reporting. `SECURITY.md`, `CODE_OF_CONDUCT.md` and the issue forms send people to the private report form of GitHub. The form does not work while the setting is off.
   1. Open the repository settings of minimenta. In the section "Security and quality" of the sidebar, click "Advanced Security".
   2. To the right of "Private vulnerability reporting", click "Enable".
   3. Open <https://github.com/OriginalMHV/minimenta/security/advisories/new> in a private window and check that the form opens.
6. Protect the crates.io publish job. The tag filter of the workflow does not stop a person with write access from pushing a tag on any commit. The environment `release` makes you approve each publish.
   1. Open the repository settings of minimenta. Go to "Environments" and create the environment `release`. The `publish` job skips itself for 0.1.0, so GitHub does not create the environment before 0.1.1. Without protection rules, the first run of the job would publish with no approval.
   2. Add yourself under "Required reviewers".
   3. Under "Deployment branches and tags", choose "Selected branches and tags". Add a rule of type "Tag" with the name pattern `v*`.
   4. Recommended: create a tag ruleset for the pattern `v*`. Go to "Rules", then "Rulesets", then "New tag ruleset". Restrict creations, updates and deletions. Add the role "Repository admin" to the bypass list.
7. Publish 0.1.0 to crates.io by hand. Use the commit that you merged in step 3.
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
8. Set up trusted publishing on crates.io:
   1. Open <https://crates.io/crates/minimenta/settings>. You must be an owner of the crate.
   2. Under "Trusted Publishing", add a GitHub publisher.
   3. Enter these values:

      | Field | Value |
      |---|---|
      | Repository owner | `OriginalMHV` |
      | Repository name | `minimenta` |
      | Workflow filename | `release.yml` |
      | Environment | `release` |

   The workflow filename is `release.yml`, not `publish-crates.yml`. crates.io checks the workflow file that starts the run. `release.yml` starts the run and calls `publish-crates.yml`.

   Do not leave the environment field empty. crates.io then accepts a publish from any job of `release.yml`, also one that has no approval. With the environment `release` from step 6, only an approved run of the `publish` job can publish.
9. Create the tag on the commit that you published, and push it:

   ```sh
   git switch main
   git pull --ff-only
   git tag -s v0.1.0 -m "v0.1.0"
   git push origin v0.1.0
   ```

   The tag name must be `v` followed by the version in `Cargo.toml`. If the name differs, cargo-dist stops the run.
10. Watch the run. The `verify` job of `custom-publish-crates` finds 0.1.0 on crates.io, and the `publish` job skips itself, so no approval is needed. Then verify the release (see "Verify a release").

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

8. Watch the run in the Actions tab, or run `gh run watch`. The run publishes to GitHub, Homebrew, and crates.io. The `publish` job waits until you approve the deployment to the environment `release`. Open the run page, click "Review deployments", and approve.
9. Verify the release.

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

   Both commands must print the new version.
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

- **A build job fails.** No release exists yet, because `host` runs after all builds. Fix the problem on `main`. Delete the tag with `git push origin :refs/tags/vX.Y.Z` and `git tag -d vX.Y.Z`. Create the tag again on the new commit.
- **The Homebrew job fails.** The usual cause is a missing or expired `HOMEBREW_TAP_TOKEN`. The job runs after `host`, so the GitHub release is already public with all files. The crates.io job does not depend on the Homebrew job, so it still runs. The run is red and `announce` is skipped. `brew install` does not work until the tap has the formula. Fix the secret. Then run `gh run rerun RUN_ID --failed`. This runs the failed job only and completes the tap update.
- **One publish job fails and the other succeeds.** The Homebrew job and the crates.io job are independent. Fix the cause and run `gh run rerun RUN_ID --failed`. The job that passed stays as it is.
- **The crates.io `verify` job fails.** The GitHub release and the Homebrew formula already exist, and a published tag stays on its commit. A re-run cannot fix the problem, so fix it on `main` and release the next patch version. The `Package` workflow on pull requests should catch this before.
- **The crates.io `publish` job fails with an authentication error.** Check the trusted publisher on crates.io. The owner, repository, workflow filename (`release.yml`), and environment (`release`) must match exactly. If the job ran before you published 0.1.0 by hand, publish by hand first, then re-run the failed job.
- **A published version has a defect.** A published version cannot be replaced. Run `cargo yank --version X.Y.Z` if the version is harmful, and release a new patch version.

## Maintenance

- **Change the dist configuration.** Edit `dist-workspace.toml`, then run `dist generate`. Commit the new `release.yml`. Never edit `release.yml` by hand. The `plan` job fails when the file differs from the configuration.
- **Update cargo-dist.** Install the new version of `dist`, change `cargo-dist-version` in `dist-workspace.toml`, and run `dist init`. Review the diff of `release.yml`.
- **Test all targets on a pull request.** Set `pr-run-mode = "upload"` in `dist-workspace.toml` and run `dist generate`. Every build job then runs on the pull request. Set it back to `"plan"` before you merge.
- **Update the actions of the publish workflow.** `publish-crates.yml` pins each action to a commit, with the version in a comment. Dependabot opens pull requests for these pins. Before you merge one, check that the commit belongs to the release that the comment names. Dependabot skips `release.yml`, because dist generates it.
- **Raise the minimum Rust version.** Change `rust-version` in `Cargo.toml`. The MSRV workflow reads that value. Add a note under "Changed" in the changelog.
