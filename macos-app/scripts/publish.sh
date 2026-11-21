#!/bin/sh
# Publish the release that release.sh made: tag the commit it was built from
# as v<version>, push the tag, and make the GitHub release with the disk image
# and the signed update feed (appcast.xml), the release notes and the image's
# sha256. Installed copies read the feed of the latest release
# (https://github.com/descoped/dedupd/releases/latest/download/appcast.xml),
# so from this moment they offer the new version.
#
# Run only after the owner has looked at the disk image and said to publish.
#
# Usage: publish.sh
set -eu

app_dir=$(cd "$(dirname "$0")/.." && pwd)
repo=descoped/dedupd
out=$app_dir/build/release

fail() {
  echo "error: $*" >&2
  exit 1
}

cd "$app_dir/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
dmg=$out/dedupd-$version.dmg
feed=$out/appcast.xml
notes=$app_dir/release-notes/$version.md

[ -f "$dmg" ] || fail "no notarised disk image at $dmg; make it with just app-release."
[ -f "$feed" ] || fail "no update feed at $feed; just app-release writes it."
[ -s "$notes" ] || fail "no release notes in macos-app/release-notes/$version.md."
grep -q "<sparkle:version>$version</sparkle:version>" "$feed" || fail "the feed does not offer $version."
xcrun stapler validate "$dmg" >/dev/null || fail "$dmg is not notarised."
[ -z "$(git status --porcelain)" ] || fail "the working tree has changes; the tag must name the commit that was built."
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
  fail "the tag v$version exists already."
fi

sum=$(shasum -a 256 "$dmg" | awk '{print $1}')
body=$(mktemp)
trap 'rm -f "$body"' EXIT
{
  cat "$notes"
  printf '\n\n`dedupd-%s.dmg` sha256: `%s`\n' "$version" "$sum"
} >"$body"

git tag "v$version"
git push origin "v$version"
gh release create "v$version" --repo "$repo" --title "dedupd $version" --notes-file "$body" "$dmg" "$feed"
echo "Published dedupd $version: https://github.com/$repo/releases/tag/v$version"
