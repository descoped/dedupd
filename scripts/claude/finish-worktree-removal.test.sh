#!/bin/sh
# Test finish-worktree-removal.sh against a throwaway repository: it removes the
# leftover of a half-done worktree removal and nothing else.
# The checks are eval'd strings, so their variables look unexpanded and unused.
# shellcheck disable=SC2016,SC2034
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=$here/finish-worktree-removal.sh

tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
repo=$tmp/repo
pass=0
fail=0

check() {
  if eval "$2"; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    echo "FAIL: $1"
  fi
}

git init -q "$repo"
git -C "$repo" -c user.email=t@example.com -c user.name=t commit -q --allow-empty -m init
wts=$repo/.claude/worktrees
mkdir -p "$wts"

# A live worktree with a build cache.
git -C "$repo" worktree add -q "$wts/live" -b live
mkdir -p "$wts/live/target/debug"
echo artifact >"$wts/live/target/debug/a.o"

# A removal git abandoned: unlisted, with a .git file naming a git dir that is gone.
mkdir -p "$wts/dangling/target/debug"
echo "gitdir: $repo/.git/worktrees/dangling" >"$wts/dangling/.git"
echo artifact >"$wts/dangling/target/debug/.DS_Store"

# A removal abandoned after the .git file was already deleted.
mkdir -p "$wts/bare/target"
echo artifact >"$wts/bare/target/.DS_Store"

# A directory that is its own repository is not a leftover.
git init -q "$wts/own-repo"
echo keep >"$wts/own-repo/file"

# An unlisted directory whose git dir still exists is not a leftover.
mkdir -p "$wts/elsewhere" "$tmp/other-gitdir"
echo "gitdir: $tmp/other-gitdir" >"$wts/elsewhere/.git"

out=$(cd "$repo" && sh "$script" 2>&1)
rc=$?

check "exit 0 when every leftover is removed" '[ "$rc" -eq 0 ]'
check "a leftover with a dangling .git is removed" '[ ! -e "$wts/dangling" ]'
check "a leftover without a .git is removed" '[ ! -e "$wts/bare" ]'
check "a live worktree keeps its cache" '[ -f "$wts/live/target/debug/a.o" ]'
check "a directory that is its own repository is kept" '[ -f "$wts/own-repo/file" ]'
check "a directory whose git dir exists is kept" '[ -f "$wts/elsewhere/.git" ]'
check "each removal is reported" '[ "$(printf "%s\n" "$out" | grep -c "^removing the leftover")" -eq 2 ]'

# From inside a worktree it still resolves the main checkout.
mkdir -p "$wts/again"
out=$(cd "$wts/live" && sh "$script" 2>&1)
check "it works from inside a worktree" '[ ! -e "$wts/again" ]'

# A second run finds nothing.
out=$(cd "$repo" && sh "$script" 2>&1)
check "a clean tree is a silent no-op" '[ -z "$out" ]'

# While git is still creating a worktree, nothing is removed.
mkdir -p "$wts/half-made" "$repo/.git/worktrees/half-made"
echo initializing >"$repo/.git/worktrees/half-made/locked"
out=$(cd "$repo" && sh "$script" 2>&1)
check "a worktree being created is kept" '[ -d "$wts/half-made" ]'
rm -rf "$repo/.git/worktrees/half-made" "$wts/half-made"

# A moved main checkout leaves every live worktree with links to the old path:
# they are relinked, never removed.
echo unsaved >"$wts/live/unsaved.txt"
mv "$repo" "$tmp/moved"
repo=$tmp/moved
wts=$repo/.claude/worktrees
out=$(cd "$repo" && sh "$script" 2>&1)
check "a live worktree survives a moved checkout" '[ -f "$wts/live/unsaved.txt" ]'
check "the moved worktree is listed again" 'git -C "$repo" worktree list --porcelain | grep -Fq "$(cd "$wts/live" && pwd -P)"'
check "the own repository survives a moved checkout" '[ -f "$wts/own-repo/file" ]'

# Outside a repository it does nothing.
out=$(cd "$tmp" && sh "$script" 2>&1)
rc=$?
check "outside a repository it is a no-op" '[ "$rc" -eq 0 ] && [ -z "$out" ]'

echo "finish-worktree-removal: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
