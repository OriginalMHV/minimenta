#!/usr/bin/env bash
# Releases minimenta the same way as Ward: a release PR that dates the
# CHANGELOG, a signed tag (cargo-dist builds the GitHub release and updates
# the Homebrew tap), then crates.io from this machine. RELEASING.md explains
# the steps and what to set up once.
#
# Usage: scripts/release.sh <version> [--yes]
#   Run from a clean main. Run it again with the same version to resume.
#   --yes skips the confirmation before each step that cannot be undone.
set -euo pipefail

REPO="OriginalMHV/minimenta"
REPO_URL="https://github.com/$REPO"
CRATE="minimenta"
TAP="OriginalMHV/homebrew-tap"
BREW_NAME="OriginalMHV/tap/minimenta"

version="${1:-}"
assume_yes="${2:-}"

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
warn() { printf '  \033[33mwarning:\033[0m %s\n' "$*"; }
step() {
  local label=$1
  shift
  local started=$SECONDS log
  log="$(mktemp)"
  printf '  %-16s ' "$label"
  if "$@" >"$log" 2>&1; then
    printf 'ok (%ss)\n' "$((SECONDS - started))"
    rm -f "$log"
  else
    printf 'FAILED\n'
    cat "$log" >&2
    rm -f "$log"
    fail "$label failed"
  fi
}
confirm() {
  [[ "$assume_yes" == "--yes" ]] && return 0
  local answer
  read -r -p "$1 [y/N] " answer
  [[ "$answer" == "y" || "$answer" == "Y" ]] || fail "stopped by user"
}

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "usage: scripts/release.sh <version> [--yes]  (for example 0.1.0)"
[[ -z "$assume_yes" || "$assume_yes" == "--yes" ]] || fail "unknown option: $assume_yes"
tag="v$version"
branch="release/$tag"

for tool in git gh cargo jq curl perl; do
  command -v "$tool" >/dev/null || fail "$tool is not installed"
done
gh auth status >/dev/null 2>&1 || fail "gh is not logged in. Run: gh auth login"
cd "$(git rev-parse --show-toplevel)"

# Reads the package version from Cargo.toml text on stdin.
manifest_version() { awk -F'"' '/^version = "/ { print $2; exit }'; }
crate_published() {
  curl -fsS -A "$CRATE-release-script ($REPO_URL)" "https://crates.io/api/v1/crates/$CRATE/$version" >/dev/null 2>&1
}
# Undoes a release branch that was not pushed, so that a new run starts clean.
abandon_branch() {
  git checkout --quiet HEAD -- Cargo.toml Cargo.lock CHANGELOG.md 2>/dev/null || true
  git switch --quiet main 2>/dev/null || true
  git branch --quiet -D "$branch" 2>/dev/null || true
}

bold "Checking the starting point"
git fetch --quiet origin main || fail "could not fetch origin/main"
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || fail "the working tree has uncommitted changes"
remote_tags="$(git ls-remote --tags --refs origin 'refs/tags/v*')" || fail "could not list the tags on GitHub"
first=0
[[ -n "$(awk -v t="refs/tags/$tag" 'NF && $2 != t' <<<"$remote_tags")" ]] || first=1
if ((first)); then
  echo "This is the first release."
fi

problems=()
secrets="$(gh secret list --repo "$REPO" --json name --jq '.[].name')" || fail "could not list the repository secrets"
grep -qx HOMEBREW_TAP_TOKEN <<<"$secrets" \
  || problems+=("The repository secret HOMEBREW_TAP_TOKEN is missing. The Homebrew job needs it.")
if ((first)); then
  main_readme="$(git show origin/main:README.md)"
  grep -qF "brew install $BREW_NAME" <<<"$main_readme" \
    || problems+=("README.md on main has no install instructions for the release. crates.io shows the README of the first version for good, so merge them first.")
fi
if ((${#problems[@]})); then
  for problem in "${problems[@]}"; do
    printf '  - %s\n' "$problem" >&2
  done
  fail "fix the setup above (RELEASING.md, \"Set up once\"), then run the script again"
fi
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
if [[ -z "${CARGO_REGISTRY_TOKEN:-}" && ! -f "$cargo_home/credentials.toml" && ! -f "$cargo_home/credentials" ]]; then
  warn "found no crates.io token. The last step needs one: run cargo login before it."
fi

# Step 1: release PR.
main_changelog="$(git show origin/main:CHANGELOG.md)"
if grep -qF "## [$version] - " <<<"$main_changelog"; then
  echo "The CHANGELOG on main has a section for $version, so the release PR is merged."
else
  if ! git ls-remote --exit-code --heads origin "$branch" >/dev/null 2>&1; then
    [[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] || fail "check out main first"
    git merge --quiet --ff-only origin/main || fail "main has diverged from origin/main"
    if git rev-parse --quiet --verify "refs/heads/$branch" >/dev/null; then
      fail "the local branch $branch exists. Delete it with: git branch -D $branch"
    fi
    previous="$(manifest_version <Cargo.toml)"
    if ((first)); then
      [[ "$previous" == "$version" ]] || fail "the first release must use the version in Cargo.toml ($previous)"
    else
      [[ "$previous" != "$version" && "$(printf '%s\n%s\n' "$previous" "$version" | sort -V | tail -1)" == "$version" ]] \
        || fail "$version is not higher than the version in Cargo.toml ($previous)"
    fi
    perl -0ne 'exit(/^## \[Unreleased\]\n+### /m ? 0 : 1)' CHANGELOG.md \
      || fail "the Unreleased section of CHANGELOG.md is empty"

    bold "Preparing $tag"
    git switch --quiet -c "$branch"
    trap abandon_branch EXIT
    today="$(date +%Y-%m-%d)"
    export VERSION="$version" PREVIOUS="$previous" TODAY="$today" REPO_URL
    if ((first)); then
      # The note above [Unreleased] says that nothing is released yet. Its
      # last sentences explain the Changed and Fixed lists of the first
      # version, so they move under the new heading.
      perl -0pi -e '
        s/^minimenta has no release yet\. The Unreleased section lists what version \S+ contains\. ([^\n]*)\n\n## \[Unreleased\]\n/## [Unreleased]\n\n## [$ENV{VERSION}] - $ENV{TODAY}\n\n$1\n/m
          or die "CHANGELOG.md does not have the note of the first release\n";
        s/\n*\z/\n\n[Unreleased]: $ENV{REPO_URL}\/compare\/v$ENV{VERSION}...HEAD\n[$ENV{VERSION}]: $ENV{REPO_URL}\/releases\/tag\/v$ENV{VERSION}\n/;
      ' CHANGELOG.md
    else
      perl -0pi -e 's/^version = "[^"]*"/version = "$ENV{VERSION}"/m' Cargo.toml
      [[ "$(manifest_version <Cargo.toml)" == "$version" ]] || fail "could not update the version in Cargo.toml"
      perl -0pi -e '
        s/^## \[Unreleased\]\n/## [Unreleased]\n\n## [$ENV{VERSION}] - $ENV{TODAY}\n/m
          or die "CHANGELOG.md has no [Unreleased] heading\n";
        s{^\[Unreleased\]: \S+}{[Unreleased]: $ENV{REPO_URL}/compare/v$ENV{VERSION}...HEAD\n[$ENV{VERSION}]: $ENV{REPO_URL}/compare/v$ENV{PREVIOUS}...v$ENV{VERSION}}m
          or die "CHANGELOG.md has no [Unreleased] link\n";
      ' CHANGELOG.md
    fi
    grep -qF "## [$version] - $today" CHANGELOG.md || fail "could not date the CHANGELOG"

    bold "Running the checks (the first run on a machine compiles everything and takes several minutes)"
    if ((!first)); then
      step "lock file" cargo update --workspace
    fi
    step "formatting" cargo fmt --check
    step "clippy" cargo clippy --all-targets -- -D warnings
    step "tests" cargo test
    if cargo deny --version >/dev/null 2>&1; then
      step "cargo deny" cargo deny check
    else
      printf '  %-16s skipped (cargo-deny is not installed, CI runs it)\n' "cargo deny"
    fi
    step "package dry run" cargo publish --dry-run --locked --allow-dirty

    git add Cargo.toml Cargo.lock CHANGELOG.md
    printf 'chore: release %s\n' "$tag" | git commit --quiet -S -F -
    git push --quiet -u origin "$branch"
    trap - EXIT
    git switch --quiet main
  fi

  pr="$(gh pr list --repo "$REPO" --head "$branch" --state open --json number --jq '.[0].number // empty')"
  if [[ -z "$pr" ]]; then
    body="$(mktemp)"
    # The backticks are Markdown.
    # shellcheck disable=SC2016
    printf 'Dates the CHANGELOG for %s. After the merge, `scripts/release.sh %s` tags the release and publishes it.\n' "$version" "$version" >"$body"
    gh pr create --repo "$REPO" --base main --head "$branch" --title "chore: release $tag" --body-file "$body" >/dev/null
    rm -f "$body"
    pr="$(gh pr list --repo "$REPO" --head "$branch" --state open --json number --jq '.[0].number // empty')"
    [[ -n "$pr" ]] || fail "could not open the PR for $branch. Check $REPO_URL/pulls"
  fi
  bold "Waiting for CI on PR #$pr"
  # GitHub registers the checks a little after the PR opens.
  for _ in $(seq 1 30); do
    [[ -n "$(gh pr checks "$pr" --repo "$REPO" 2>/dev/null)" ]] && break
    sleep 10
  done
  gh pr checks "$pr" --repo "$REPO" --watch --interval 20 || fail "CI failed on PR #$pr. Fix it, then run the script again."
  confirm "Merge PR #$pr into main?"
  gh pr merge "$pr" --repo "$REPO" --squash --delete-branch \
    --subject "chore: release $tag (#$pr)" --body ""
  git fetch --quiet origin main
fi

git switch --quiet main
git merge --quiet --ff-only origin/main || fail "main has diverged from origin/main"
[[ "$(manifest_version <Cargo.toml)" == "$version" ]] || fail "Cargo.toml on main does not have version $version"
release_commit="$(git log -1 --format=%H -S "## [$version] - " HEAD -- CHANGELOG.md)"
head_commit="$(git rev-parse HEAD)"

# Step 2: signed tag. cargo-dist builds the GitHub release and the formula.
if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
  echo "Tag $tag already exists on GitHub."
  git fetch --quiet origin "refs/tags/$tag:refs/tags/$tag" \
    || fail "the local tag $tag differs from the tag on GitHub. Delete it with: git tag -d $tag"
else
  # Like Ward, the tag goes on the head of main. After a failed build, that
  # head holds the fix.
  if [[ -n "$release_commit" && "$release_commit" != "$head_commit" ]]; then
    echo "  main has $(git rev-list --count "$release_commit..HEAD") commits after the release PR. The tag includes them."
  fi
  confirm "Create and push the signed tag $tag on $(git log -1 --format='%h (%s)' HEAD)? This starts the public release."
  if git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
    [[ "$(git rev-parse "$tag^{commit}")" == "$head_commit" ]] \
      || fail "the local tag $tag points to another commit. Delete it with: git tag -d $tag"
  else
    git tag -s "$tag" -m "$tag"
  fi
  git push --quiet origin "refs/tags/$tag"
fi
tag_commit="$(git rev-parse "$tag^{commit}")"

bold "Waiting for the release workflow (about 10 minutes)"
run_id=""
for _ in $(seq 1 30); do
  run_id="$(gh run list --repo "$REPO" --workflow release.yml --branch "$tag" --event push --limit 1 \
    --json databaseId --jq '.[0].databaseId // empty')"
  [[ -n "$run_id" ]] && break
  sleep 10
done
[[ -n "$run_id" ]] || fail "the release workflow did not start. Check $REPO_URL/actions"
echo "  $REPO_URL/actions/runs/$run_id"
gh run watch "$run_id" --repo "$REPO" --interval 30 --exit-status >/dev/null \
  || fail "the release workflow failed: $REPO_URL/actions/runs/$run_id. RELEASING.md, \"When a step fails\", says what to do. Then run the script again."

bold "Checking the release"
assets="$(gh release view "$tag" --repo "$REPO" --json assets --jq '.assets | length')"
if [[ "$assets" == 17 ]]; then
  echo "  The GitHub release has 17 files."
else
  warn "the GitHub release has $assets files. RELEASING.md lists 17."
fi
formula="$(gh api -H "Accept: application/vnd.github.raw" "repos/$TAP/contents/Formula/$CRATE.rb" 2>/dev/null || true)"
if grep -qF "version \"$version\"" <<<"$formula"; then
  echo "  The Homebrew formula has version $version."
else
  warn "the Homebrew formula in $TAP does not have version $version."
fi

# Step 3: crates.io.
if crate_published; then
  echo "$CRATE $version is already on crates.io."
else
  [[ "$(git rev-parse HEAD)" == "$tag_commit" ]] \
    || fail "main has commits after $tag. Publish the tagged commit: git switch --detach $tag && cargo publish --locked && git switch main"
  if ((first)); then
    echo "  The first publish creates the crate on crates.io. The token needs the scope publish-new."
  fi
  confirm "Publish $CRATE $version to crates.io? A published version cannot be replaced."
  cargo publish --locked \
    || fail "cargo publish failed. Fix the cause (run cargo login for a token problem), then run the script again. It continues with this step."
fi

bold "Released $tag"
echo "GitHub:    $(gh release view "$tag" --repo "$REPO" --json url --jq .url)"
echo "crates.io: https://crates.io/crates/$CRATE/$version"
echo "Homebrew:  brew install $BREW_NAME"
echo "Now do the checks in RELEASING.md, \"Verify a release\"."
