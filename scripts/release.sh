#!/bin/sh
# Publishes the GitHub release of the version in Cargo.toml: builds slutty.app,
# zips it, tags main as v<version> and uploads the zip with the notes in $1.
# Usage: scripts/release.sh <notes.md>
set -eu

notes=$(realpath "$1")
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tag=v$version
zip=target/release/slutty-$version-arm64.zip

fail() {
    echo "$1" >&2
    exit 1
}
[ "$(uname -m)" = arm64 ] || fail "build on Apple silicon: the zip is named arm64"
[ "$(git branch --show-current)" = main ] || fail "release from main"
[ -z "$(git status --porcelain)" ] || fail "the working copy is dirty"
git fetch --quiet origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not origin/main"

scripts/bundle-app.sh
rm -f "$zip"
# ditto keeps the bundle's signature and attributes.
ditto -c -k --keepParent target/release/slutty.app "$zip"

git tag -a "$tag" -m "slutty $version"
git push origin "$tag"
gh release create "$tag" "$zip" --verify-tag --title "slutty $version" --notes-file "$notes"
