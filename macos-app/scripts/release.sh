#!/bin/sh
# Make the app's release: a disk image that opens on any Mac without a warning.
#
# The app is distributed outside the App Store. For macOS to open it without
# complaint it has to be signed with the team's Developer ID certificate and
# notarised: Apple's service scans it and issues a ticket, which is then
# stapled to the app and to the disk image so that the check also passes on a
# Mac that is offline.
#
# Steps: a last version check, archive a Release build, check its signatures
# and its version, have Apple notarise the app, staple it, put it in a disk
# image, sign that, have Apple notarise the image, staple it, ask macOS
# whether it would open both, and write the signed update feed (appcast.xml)
# that installed copies read to offer the new version.
#
# The last version check refuses a release before anything is built when:
#   - the version in Cargo.toml is not three numbers (0.3.0; no -rc suffix,
#     which a build number cannot carry);
#   - it is not greater than the latest published release on GitHub;
#   - the tag v<version> exists already;
#   - the working tree has changes, or HEAD is not on origin/main;
#   - there are no release notes in macos-app/release-notes/<version>.md;
#   - the app has no update key, or it is not the one in the keychain.
# After the build it checks that the app's version and build number are that
# version.
#
# Needs, on this Mac:
#   - the "Developer ID Application" certificate with its private key in the
#     keychain;
#   - a notary credential stored in the keychain under the name in
#     DEDUPD_NOTARY_PROFILE (default: dedupd-notary), made once with
#     `xcrun notarytool store-credentials`;
#   - the update key: its private half in the keychain (Sparkle's
#     generate_keys), its public half as SPARKLE_PUBLIC_KEY in
#     Developer.xcconfig;
#   - gh, signed in, to ask GitHub for the latest release.
#
# Usage: release.sh [--skip-notary | --check]
#   --skip-notary   stop short of Apple: the image is signed but not
#                   notarised, and is named so that nobody ships it. The
#                   version check only warns, and the feed is written only
#                   when the update key is there.
#   --check         run the last version check only, and build nothing.
#
# Writes: macos-app/build/release/dedupd-<version>.dmg and appcast.xml.
# Publishing is `just app-publish`, after the owner has looked at both.
set -eu

app_dir=$(cd "$(dirname "$0")/.." && pwd)
identity="Developer ID Application"
profile=${DEDUPD_NOTARY_PROFILE:-dedupd-notary}
notarise=yes
check_only=no
case "${1:-}" in
  --skip-notary) notarise=no ;;
  --check) check_only=yes ;;
  "") ;;
  *) echo "usage: release.sh [--skip-notary | --check]" >&2; exit 2 ;;
esac
repo=descoped/dedupd
sparkle=$app_dir/build/DerivedData/SourcePackages/artifacts/sparkle/Sparkle/bin

out=$app_dir/build/release
archive=$out/Dedupd.xcarchive
app=$out/Dedupd.app

fail() {
  echo "error: $*" >&2
  exit 1
}

step() {
  printf '\n== %s\n' "$*"
}

# A refusal of a real release; for a package that is only checked, a warning.
refuse() {
  if [ "$notarise" = yes ]; then
    fail "$*"
  fi
  echo "warning: $* (a real release would stop here)" >&2
}

# Whether version $1 is greater than version $2, by the numbers between the
# dots.
greater() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)" = "$1" ]
}

step "The last version check"
cd "$app_dir/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
echo "Cargo.toml says $version."
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
  fail "the version \"$version\" is not three numbers such as 0.3.0; a build number cannot carry anything else."
latest=$(gh release view --repo "$repo" --json tagName --jq .tagName 2>/dev/null | sed 's/^v//') ||
  latest=""
if [ -z "$latest" ]; then
  refuse "GitHub did not say which release is the latest (is gh signed in?)."
elif ! greater "$version" "$latest"; then
  refuse "$version is not greater than the latest release, $latest. Raise the version in Cargo.toml first."
else
  echo "The latest release is $latest; $version is greater."
fi
if git rev-parse -q --verify "refs/tags/v$version" >/dev/null ||
  git ls-remote --exit-code --tags origin "refs/tags/v$version" >/dev/null 2>&1; then
  refuse "the tag v$version exists already."
fi
if [ -n "$(git status --porcelain)" ]; then
  refuse "the working tree has changes; a release is built from a commit."
fi
git fetch -q origin main 2>/dev/null || true
git merge-base --is-ancestor HEAD origin/main 2>/dev/null ||
  refuse "HEAD is not on origin/main; a release is built from main."
notes=$app_dir/release-notes/$version.md
[ -s "$notes" ] || refuse "there are no release notes in macos-app/release-notes/$version.md; the update offer shows them."

# The update key: the app's public half (Developer.xcconfig, or a
# Local.xcconfig over it) and the private half in the keychain must match,
# or installed copies could never take this release.
xcodebuild -resolvePackageDependencies -scheme Dedupd -derivedDataPath "$app_dir/build/DerivedData" \
  -project "$app_dir/Dedupd.xcodeproj" -quiet >/dev/null
public_key=$(xcodebuild -showBuildSettings -scheme Dedupd -configuration Release \
  -project "$app_dir/Dedupd.xcodeproj" 2>/dev/null | sed -n 's/^ *SPARKLE_PUBLIC_KEY = //p' | head -1)
feed=yes
if [ -z "$public_key" ]; then
  refuse "SPARKLE_PUBLIC_KEY is empty in Developer.xcconfig: a release without it could never be updated. Make the key with $sparkle/generate_keys and put the public key printed there."
  feed=no
elif [ "$("$sparkle/generate_keys" -p 2>/dev/null)" != "$public_key" ]; then
  refuse "the update key in the keychain is not the one the app carries (SPARKLE_PUBLIC_KEY)."
  feed=no
else
  echo "The app carries the update key, and its private half is in the keychain."
fi
if [ "$check_only" = yes ]; then
  echo "The last version check passed for $version."
  exit 0
fi

# Everything this needs is checked before the long build starts.
security find-identity -v -p codesigning | grep -q "\"$identity:" ||
  fail "no \"$identity\" certificate with a private key in the keychain."
if [ "$notarise" = yes ]; then
  xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1 ||
    fail "no usable notary credential named \"$profile\" in the keychain. Store one with
       xcrun notarytool store-credentials $profile --key <AuthKey.p8> --key-id <key id> --issuer <issuer id>
       or run this with --skip-notary to stop short of Apple."
fi

# What notarisation requires of a signature, checked here so that a mistake
# shows up in seconds and not after an upload.
check_signature() {
  details=$(codesign --display --verbose=4 "$1" 2>&1)
  for wanted in "Authority=$identity" "Timestamp=" "(runtime)"; do
    case $details in
      *"$wanted"*) ;;
      *) fail "$1 is not signed for release: \"$wanted\" is missing from its signature." ;;
    esac
  done
  entitlements=$(codesign --display --entitlements - --xml "$1" 2>/dev/null || true)
  case $entitlements in
    *get-task-allow*) fail "$1 carries the get-task-allow entitlement, which Apple refuses." ;;
  esac
}

# Send a file to Apple, wait for the answer, and stop with Apple's own log
# when the answer is not "Accepted".
notarise() {
  log=$out/notary-$(basename "$1").log
  xcrun notarytool submit "$1" --keychain-profile "$profile" --wait 2>&1 | tee "$log"
  if ! grep -q "status: Accepted" "$log"; then
    id=$(sed -n 's/^ *id: //p' "$log" | head -1)
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --keychain-profile "$profile" >&2 || true
    fi
    fail "Apple did not accept $(basename "$1")."
  fi
}

step "Archive a Release build"
rm -rf "$out"
mkdir -p "$out"
cd "$app_dir"
xcodebuild -scheme Dedupd -destination 'generic/platform=macOS' -configuration Release \
  -derivedDataPath build/DerivedData -archivePath "$archive" -quiet archive
ditto "$archive/Products/Applications/Dedupd.app" "$app"
built=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
build_number=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Contents/Info.plist")
inside=$("$app/Contents/Helpers/dedupd" --version | awk '{print $2}')
[ "$built" = "$version" ] && [ "$build_number" = "$version" ] && [ "$inside" = "$version" ] ||
  fail "the build is not one version: the app says $built (build $build_number), the dedupd inside it $inside, Cargo.toml $version."
echo "The app, its build number and the dedupd inside it are all $version."

step "Sign the updater's helpers for release"
# Xcode signs Sparkle.framework when it copies it in, but not the programs
# inside it, which keep Sparkle's own ad-hoc signature; Apple notarises
# nothing that holds an ad-hoc signature. The app is not sandboxed, so the
# XPC services Sparkle offers for sandboxed apps are not used and are
# removed. The helpers, the framework and then the app are signed again,
# inside out, as Sparkle's documentation says.
sparkle_framework=$app/Contents/Frameworks/Sparkle.framework
rm -rf "$sparkle_framework/Versions/B/XPCServices" "$sparkle_framework/XPCServices"
for helper in "$sparkle_framework/Versions/B/Autoupdate" "$sparkle_framework/Versions/B/Updater.app" "$sparkle_framework"; do
  codesign --force --sign "$identity" --timestamp --options runtime "$helper"
done
codesign --force --sign "$identity" --timestamp --options runtime \
  --preserve-metadata=entitlements,requirements,flags "$app"

step "Check the signatures of dedupd $version"
codesign --verify --deep --strict "$app"
check_signature "$app"
check_signature "$app/Contents/Helpers/dedupd"
check_signature "$sparkle_framework"
check_signature "$sparkle_framework/Versions/B/Autoupdate"
check_signature "$sparkle_framework/Versions/B/Updater.app"
echo "The app, the dedupd command and the updater inside it are signed for release."

if [ "$notarise" = yes ]; then
  step "Have Apple notarise the app"
  ditto -c -k --keepParent "$app" "$out/Dedupd.zip"
  notarise "$out/Dedupd.zip"
  xcrun stapler staple "$app"
  rm "$out/Dedupd.zip"
  dmg=$out/dedupd-$version.dmg
else
  dmg=$out/dedupd-$version-not-notarised.dmg
fi

step "Make the disk image"
staging=$out/image
mkdir -p "$staging"
ditto "$app" "$staging/Dedupd.app"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "dedupd $version" -srcfolder "$staging" -format UDZO -ov "$dmg"
rm -rf "$staging"
codesign --sign "$identity" --timestamp "$dmg"

if [ "$notarise" = yes ]; then
  step "Have Apple notarise the disk image"
  notarise "$dmg"
  xcrun stapler staple "$dmg"

  step "Ask macOS whether it would open them"
  xcrun stapler validate "$app"
  xcrun stapler validate "$dmg"
  spctl --assess --type execute --verbose=2 "$app"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi

if [ "$feed" = yes ]; then
  step "Write the signed update feed"
  # The feed of the latest release is the start, so it keeps the earlier
  # entries; the new entry points at this release's disk image.
  feed_dir=$out/feed
  mkdir -p "$feed_dir"
  if [ -n "$latest" ]; then
    gh release download "v$latest" --repo "$repo" --pattern appcast.xml --dir "$feed_dir" 2>/dev/null ||
      echo "The latest release has no feed yet; this is the first entry."
  fi
  cp "$dmg" "$feed_dir/"
  [ -s "$notes" ] && cp "$notes" "$feed_dir/$(basename "$dmg" .dmg).md"
  "$sparkle/generate_appcast" --download-url-prefix "https://github.com/$repo/releases/download/v$version/" \
    --maximum-deltas 0 --embed-release-notes "$feed_dir"
  "$sparkle/sign_update" --verify "$feed_dir/appcast.xml"
  grep -q "<sparkle:version>$version</sparkle:version>" "$feed_dir/appcast.xml" ||
    fail "the feed has no entry for $version."
  cp "$feed_dir/appcast.xml" "$out/appcast.xml"
  echo "The feed is signed and offers $version: $out/appcast.xml"
fi

step "Done"
shasum -a 256 "$dmg"
if [ "$notarise" = no ]; then
  echo "Signed but not notarised: macOS will warn about this image on another Mac. Do not ship it."
else
  echo "Look at both, then publish with: just app-publish"
fi
