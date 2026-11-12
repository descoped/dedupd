#!/bin/sh
# A fast round trip on this Mac: put the app that was just built into
# /Applications, bring the installed daemon to the same build, and open the
# app again. For trying a change, not for a release (that is release.sh).
#
# Usage: dev-install.sh <built Dedupd.app>
#
# What it changes: /Applications/Dedupd.app is replaced. When a daemon is
# installed and is another build than the one inside the app, the app's own
# `dedupd install` replaces it and starts it again (a download that was
# running goes on from the queue). When no daemon is installed, none is
# installed here: that stays the owner's click on the Daemon screen.
set -eu

built=${1:?usage: dev-install.sh <built Dedupd.app>}
target=/Applications/Dedupd.app
bundle=io.descoped.dedupd.app

[ -x "$built/Contents/Helpers/dedupd" ] || {
  echo "error: $built has no dedupd inside it; build the app first" >&2
  exit 1
}

echo "== Quit the running app"
osascript -e "tell application id \"$bundle\" to quit" >/dev/null 2>&1 || true
n=0
while pgrep -f "$target/Contents/MacOS/Dedupd" >/dev/null 2>&1; do
  n=$((n + 1))
  [ "$n" -gt 50 ] && { echo "error: the app did not quit" >&2; exit 1; }
  sleep 0.2
done

echo "== Put the new build into /Applications"
rm -rf "$target"
ditto "$built" "$target"

helper=$target/Contents/Helpers/dedupd
state=$("$helper" agent --json)
says() { printf '%s' "$state" | grep -q "\"$1\": $2"; }

echo "== The daemon"
if ! says installed true; then
  echo "No daemon is installed. Install it from the app's Daemon screen."
elif says same_build true && says running true; then
  echo "The installed daemon is already this build."
else
  # Keep the owner's choice about HF_ENDPOINT for apps started from the Dock.
  if [ -n "$(launchctl getenv HF_ENDPOINT)" ]; then
    "$helper" install --env
  else
    "$helper" install
  fi
fi

echo "== Open the app"
open "$target" --args -mainWindow YES
echo "Done: dedupd $("$helper" --version | awk '{print $2}') is in /Applications."
