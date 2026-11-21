# Release notes

One file per release, `<version>.md`, written before `just app-release`. The release script refuses a release
without it. Its text is shown in the app's update offer (embedded in the signed feed) and is the body of the
GitHub release, followed by the disk image's sha256.

Write what changed for the person who uses the app, in plain words: what is new, what was fixed, and anything they
must do. Markdown, a few short lines or a short list.
