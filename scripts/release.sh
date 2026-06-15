#!/usr/bin/env bash
# Cut a Pawrly release: bump the workspace version, refresh Cargo.lock, commit,
# tag `v<version>`, and push — which triggers .github/workflows/release.yml to
# build prebuilt binaries and publish the GitHub Release that scripts/install.sh
# downloads from.
#
# Usage:
#   scripts/release.sh <version> [options]
#   scripts/release.sh 0.1.0
#   scripts/release.sh 0.1.0 --dry-run
#
# Options:
#   --dry-run       Show every action without mutating anything.
#   --allow-dirty   Don't require a clean working tree.
#   --allow-branch  Don't require being on `main`.
#   --no-push       Commit and tag locally but don't push.
#   --remote <name> Git remote to push to (default: origin).
#   --yes           Skip the final confirmation prompt before pushing.
#
# Version may be bare (0.1.0) or tag-style (v0.1.0); the git tag is always
# `v<version>`.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DRY_RUN=0
ALLOW_DIRTY=0
ALLOW_BRANCH=0
NO_PUSH=0
ASSUME_YES=0
REMOTE="origin"
RELEASE_BRANCH="main"
VERSION_ARG=""

# ----- output ----------------------------------------------------------------

if [ -t 2 ]; then
  BOLD="$(printf '\033[1m')"; RED="$(printf '\033[31m')"
  GREEN="$(printf '\033[32m')"; YELLOW="$(printf '\033[33m')"
  RESET="$(printf '\033[0m')"
else
  BOLD=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi
info() { printf '%s\n' "${BOLD}release${RESET} $*" >&2; }
warn() { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
die()  { printf '%s\n' "${RED}error:${RESET} $*" >&2; exit 1; }

# run <cmd...> — echoes in dry-run, executes otherwise.
run() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '  %s+ %s%s\n' "$YELLOW" "$*" "$RESET" >&2
  else
    "$@"
  fi
}

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# ----- arg parsing -----------------------------------------------------------

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)      usage 0 ;;
    --dry-run)      DRY_RUN=1 ;;
    --allow-dirty)  ALLOW_DIRTY=1 ;;
    --allow-branch) ALLOW_BRANCH=1 ;;
    --no-push)      NO_PUSH=1 ;;
    --yes|-y)       ASSUME_YES=1 ;;
    --remote)       shift; [ $# -gt 0 ] || die "--remote needs a value"; REMOTE="$1" ;;
    --remote=*)     REMOTE="${1#*=}" ;;
    -*)             die "unknown option: $1 (see --help)" ;;
    *)
      [ -z "$VERSION_ARG" ] || die "unexpected extra argument: $1"
      VERSION_ARG="$1" ;;
  esac
  shift
done

[ -n "$VERSION_ARG" ] || { warn "missing <version>"; usage 1; }

# Normalize: strip a leading v, validate semver-ish.
VERSION="${VERSION_ARG#v}"
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "version '$VERSION_ARG' is not X.Y.Z (optionally with -prerelease)" ;;
esac
TAG="v$VERSION"

# ----- tooling ---------------------------------------------------------------

for c in git cargo sed; do
  command -v "$c" >/dev/null 2>&1 || die "required command not found: $c"
done

# Portable in-place sed (BSD/macOS vs GNU).
sed_inplace() {
  _expr="$1"; _file="$2"
  if sed --version >/dev/null 2>&1; then
    sed -i -e "$_expr" "$_file"          # GNU
  else
    sed -i '' -e "$_expr" "$_file"       # BSD
  fi
}

# ----- read current version --------------------------------------------------

CURRENT="$(sed -n -E 's/^version = "([0-9][^"]*)".*/\1/p' Cargo.toml | head -n1)"
[ -n "$CURRENT" ] || die "could not read current version from Cargo.toml"

info "current: ${BOLD}$CURRENT${RESET}  ->  new: ${BOLD}$VERSION${RESET}  (tag $TAG)"
[ "$CURRENT" != "$VERSION" ] || die "version is already $VERSION"

# ----- preflight -------------------------------------------------------------

git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "not a git repository"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "$BRANCH" != "$RELEASE_BRANCH" ] && [ "$ALLOW_BRANCH" != "1" ]; then
  die "on branch '$BRANCH', expected '$RELEASE_BRANCH' (use --allow-branch to override)"
fi

if [ "$ALLOW_DIRTY" != "1" ] && [ -n "$(git status --porcelain)" ]; then
  die "working tree is not clean (use --allow-dirty to override)"
fi

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  die "tag $TAG already exists"
fi

# Warn if the remote already has the tag.
if git ls-remote --tags "$REMOTE" "refs/tags/$TAG" 2>/dev/null | grep -q "$TAG"; then
  die "tag $TAG already exists on remote '$REMOTE'"
fi

# ----- bump versions ---------------------------------------------------------

# Replace exact `version = "<current>"` occurrences in the manifests that pin
# internal crate versions. These are confined to the root manifest (workspace
# package version + internal dependency pins) and pawrly-core's self dev-dep.
FILES_TO_BUMP="Cargo.toml crates/pawrly-core/Cargo.toml"

info "bumping version in: $FILES_TO_BUMP"
for f in $FILES_TO_BUMP; do
  [ -f "$f" ] || die "expected manifest not found: $f"
  if [ "$DRY_RUN" = "1" ]; then
    _n="$(grep -c "version = \"$CURRENT\"" "$f" || true)"
    printf '  %s+ sed s/version = "%s"/version = "%s"/ in %s (%s occurrences)%s\n' \
      "$YELLOW" "$CURRENT" "$VERSION" "$f" "$_n" "$RESET" >&2
  else
    sed_inplace "s/version = \"$CURRENT\"/version = \"$VERSION\"/g" "$f"
  fi
done

# Refresh Cargo.lock for workspace members only (no registry churn).
info "updating Cargo.lock (workspace members)"
run cargo update --workspace --offline || run cargo update --workspace

# Sanity build so we never tag a broken version bump.
if [ "$DRY_RUN" != "1" ]; then
  info "verifying the workspace builds"
  cargo check --workspace --locked >/dev/null \
    || die "cargo check failed after version bump — aborting (no commit made)"
fi

# ----- commit + tag ----------------------------------------------------------

info "committing and tagging $TAG"
run git add Cargo.toml Cargo.lock crates/pawrly-core/Cargo.toml
run git commit -m "chore(release): $TAG"
run git tag -a "$TAG" -m "Pawrly $TAG"

# ----- push ------------------------------------------------------------------

if [ "$NO_PUSH" = "1" ]; then
  info "${GREEN}done${RESET} (local only). Push when ready:"
  printf '    git push %s %s && git push %s %s\n' "$REMOTE" "$BRANCH" "$REMOTE" "$TAG" >&2
  exit 0
fi

if [ "$ASSUME_YES" != "1" ] && [ "$DRY_RUN" != "1" ]; then
  printf '%s\n' "${BOLD}Push $BRANCH and tag $TAG to '$REMOTE'? This triggers the release build.${RESET} [y/N] " >&2
  read -r _reply </dev/tty || _reply=""
  case "$_reply" in
    y|Y|yes|YES) ;;
    *) info "push aborted; commit and tag remain local"; exit 0 ;;
  esac
fi

info "pushing branch and tag to $REMOTE"
run git push "$REMOTE" "$BRANCH"
run git push "$REMOTE" "$TAG"

info "${GREEN}released $TAG${RESET}"
info "watch the build:  https://github.com/CITGuru/pawrly/actions/workflows/release.yml"
