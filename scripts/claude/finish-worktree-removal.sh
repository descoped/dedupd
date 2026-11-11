#!/bin/sh
# Finish worktree removals that were left half done.
#
# A worktree's build cache is its own target/, so removing the worktree removes
# the cache. But a removal is a long recursive delete, and on macOS a
# `.DS_Store` written into the tree while it runs makes git stop with
# "Directory not empty": git has then dropped the worktree from its list and
# left most of the directory behind, with nothing pointing at it any more.
#
# This removes exactly those leftovers: a directory directly under
# .claude/worktrees/ that git does not list as a worktree and whose `.git` is
# missing or names a git dir that no longer exists. git only gets that far
# after it has checked the worktree is clean, so nothing in it is unsaved work.
# A listed worktree, and a directory that is its own repository, are never
# touched.
#
# Two states look like a leftover and are not:
# - A live worktree after the main checkout was moved: both of its links name
#   the old path. `git worktree repair` relinks it, because its admin dir still
#   exists, and it is listed again. A real leftover has no admin dir, so repair
#   cannot relink it.
# - A worktree `git worktree add` is still creating: the directory exists
#   before its `.git` file does. git holds an "initializing" lock for that
#   time, and this does nothing while one exists.
#
# Usage: finish-worktree-removal.sh   (run from anywhere inside the repository)
# Exit 2 when a leftover could not be removed, so the failure is never silent.

common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
main=$(dirname "$common")
wts=$main/.claude/worktrees
[ -d "$wts" ] || exit 0

for lock in "$common"/worktrees/*/locked; do
  grep -qs initializing "$lock" && exit 0
done

is_listed() {
  git -C "$main" worktree list --porcelain | sed -n 's/^worktree //p' | grep -Fxq "$1"
}

rc=0
for d in "$wts"/*/; do
  d=${d%/}
  [ -d "$d" ] || continue
  [ -L "$d" ] && continue
  real=$(cd "$d" 2>/dev/null && pwd -P) || continue
  is_listed "$real" && continue
  [ -d "$d/.git" ] && continue
  if [ -f "$d/.git" ]; then
    g=$(sed -n 's/^gitdir: //p' "$d/.git")
    case "$g" in /*) ;; ?*) g=$d/$g ;; esac
    [ -n "$g" ] && [ -e "$g" ] && continue
  fi
  git -C "$main" worktree repair "$d" >/dev/null 2>&1
  is_listed "$real" && continue

  echo "removing the leftover of a removed worktree: $d"
  # The same writer can drop a new .DS_Store during this delete too; a later
  # pass has almost nothing left to delete and wins.
  n=0
  while [ -e "$d" ] && [ "$n" -lt 5 ]; do
    rm -rf "$d" 2>/dev/null
    n=$((n + 1))
    [ -e "$d" ] && sleep 1
  done
  if [ -e "$d" ]; then
    echo "COULD NOT remove the leftover worktree directory $d" >&2
    rc=2
  fi
done
exit $rc
