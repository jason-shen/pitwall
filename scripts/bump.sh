#!/bin/sh
# Ships a new version: bumps Cargo.toml, Cargo.lock and the website, commits,
# tags and pushes. The pushed tag starts .github/workflows/release.yml, which
# builds, releases and updates the cask, then redeploys the website.
#
#   scripts/bump.sh [patch|minor|major]   0.1.0 -> 0.1.1 / 0.2.0 / 1.0.0
#   scripts/bump.sh 1.2.0-beta.1          an exact version
#   scripts/bump.sh 0.1.0                 the current one, if it was never tagged
#
# DRY_RUN=1 checks and prints the new version without changing anything.
# SKIP_TESTS=1 skips the local test run (CI tests the tag anyway).
set -eu
cd "$(dirname "$0")/.."

die() { echo "bump: $*" >&2; exit 1; }

arg=${1:-patch}
remote=${REMOTE:-origin}
branch=${BRANCH:-main}

current=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$current" ] || die "no version in Cargo.toml"

case $arg in
  patch|minor|major)
    # A prerelease bumps from its base: 1.2.0-beta.1 patch -> 1.2.1.
    base=${current%%-*}
    IFS=. read -r x y z <<EOF
$base
EOF
    case $arg in
      patch) z=$((z + 1)) ;;
      minor) y=$((y + 1)); z=0 ;;
      major) x=$((x + 1)); y=0; z=0 ;;
    esac
    next="$x.$y.$z"
    ;;
  *) next=${arg#v} ;;
esac
echo "$next" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' \
  || die "$next isn't a version like 1.2.3 or 1.2.3-beta.1"
tag="v$next"

# Refuse anything that would ship more, or other, than what's on $branch.
here=$(git rev-parse --abbrev-ref HEAD)
[ "$here" = "$branch" ] || die "on $here; releases go out from $branch"
[ -z "$(git status --porcelain)" ] || die "uncommitted changes; commit or stash them first:
$(git status --short)"
git fetch --quiet "$remote" "$branch" --tags
git merge-base --is-ancestor "$remote/$branch" HEAD \
  || die "$branch is behind $remote/$branch; pull first"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  die "$tag already exists"
fi

if [ "$next" = "$current" ]; then
  echo "tagging $current as it is"
else
  echo "$current -> $next"
fi
[ "${DRY_RUN:-}" = 1 ] && exit 0

if [ "${SKIP_TESTS:-}" != 1 ]; then
  cargo test --workspace --locked --quiet
fi

if [ "$next" != "$current" ]; then
  # Only the [workspace.package] line starts a line with `version =`.
  sed -i.bak "s/^version = \"$current\"/version = \"$next\"/" Cargo.toml
  rm -f Cargo.toml.bak
  # Rewrites just the workspace crates' entries in the lock file.
  cargo update --workspace --quiet
  for page in website/index.html website/zh/index.html; do
    sed -i.bak "s#releases/tag/v[^\"]*\">v[^<]*</a>#releases/tag/$tag\">$tag</a>#g" "$page"
    rm -f "$page.bak"
  done
  git add Cargo.toml Cargo.lock website/index.html website/zh/index.html
  git commit --quiet -m "Release $tag"
fi
git tag -a "$tag" -m "Pitwall $next"
# Atomic, so the tag never lands without the commit that it names.
git push --atomic "$remote" "$branch" "$tag"

echo "Pushed $tag. The release builds at:"
url=$(git remote get-url "$remote" | sed -e 's#^git@github.com:#https://github.com/#' -e 's#\.git$##')
echo "  $url/actions"
