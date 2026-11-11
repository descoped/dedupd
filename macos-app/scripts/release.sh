#!/bin/sh
# Make the app's release: a disk image that opens on any Mac without a warning.
#
# The app is distributed outside the App Store. For macOS to open it without
# complaint it has to be signed with the team's Developer ID certificate and
# notarised: Apple's service scans it and issues a ticket, which is then
# stapled to the app and to the disk image so that the check also passes on a
# Mac that is offline.
#
# Steps: archive a Release build, check its signatures, have Apple notarise
# the app, staple it, put it in a disk image, sign that, have Apple notarise
# the image, staple it, and ask macOS whether it would open both.
#
# Needs, on this Mac:
#   - the "Developer ID Application" certificate with its private key in the
#     keychain;
#   - a notary credential stored in the keychain under the name in
#     DEDUPD_NOTARY_PROFILE (default: dedupd-notary), made once with
#     `xcrun notarytool store-credentials`.
#
# Usage: release.sh [--skip-notary]
#   --skip-notary   stop short of Apple: the image is signed but not
#                   notarised, and is named so that nobody ships it.
#
# Writes: macos-app/build/release/dedupd-<version>.dmg
set -eu

app_dir=$(cd "$(dirname "$0")/.." && pwd)
identity="Developer ID Application"
profile=${DEDUPD_NOTARY_PROFILE:-dedupd-notary}
notarise=yes
if [ "${1:-}" = "--skip-notary" ]; then
  notarise=no
fi

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
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")

step "Check the signatures of dedupd $version"
codesign --verify --deep --strict "$app"
check_signature "$app"
check_signature "$app/Contents/Helpers/dedupd"
echo "The app and the dedupd command inside it are signed for release."

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

step "Done"
shasum -a 256 "$dmg"
if [ "$notarise" = no ]; then
  echo "Signed but not notarised: macOS will warn about this image on another Mac. Do not ship it."
fi
