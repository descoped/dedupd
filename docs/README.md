---
title: "dedupd documentation"
description: "dedupd keeps one copy of each AI model on disk and shares it, as APFS clones, with every app that runs it."
order: 0
---

# dedupd documentation

- [How it works](how-it-works.md): architecture, requirements, the store, clones, placement, the archive disk.
- [The macOS app](app.md): the screens, with pictures.
- [The command line](command-line.md): command reference and settings.
- [Apps and formats](apps.md): compatibility per application and format, cost in disk space, limitations.
- [Development](development.md): building, testing and code layout.
- [Roadmap](roadmap.md): what is available and what is planned: the catalogue, ModelScope, Linux with NVIDIA GPUs.

The pictures in these pages are taken by the app's UI tests, with sample data. `just
app-uitest` takes them again and `just app-screenshots` exports them.
