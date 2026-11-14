# Documentation site

The documentation site is built with [krantho](https://krantho.com)'s static mode and Next.js. Its pages are the
markdown files in [`docs/`](../docs), the same files GitHub shows in the repository; there is one copy of the
documentation. `.github/workflows/pages.yml` builds the site and publishes it to GitHub Pages when `docs/` or
`site/` changes on `main`.

## Writing a page

A page is a markdown file in `docs/`. It starts with frontmatter, which the site uses for the sidebar, the page title
and the search engine description, and then its own `# Title`:

```markdown
---
title: "Apps and formats"
description: "Which app can use which model format, by which method and at what cost in disk space."
group: Guide
order: 4
---

# Apps and formats
```

- `group` is the sidebar section, and `order` the place in the reading order. The home page is `README.md`.
- Link to another page by its file, as GitHub does: `[Apps](apps.md#limitations)`.
- Pictures go in `docs/images/` and are linked as `images/<name>.png`. The build copies them into the site.
- Quote a frontmatter value that holds `: `.

## Building

Needs Node 24 and pnpm (the version is in `package.json`).

```sh
cd site
pnpm install
pnpm dev        # http://localhost:3000, reloads when a page changes
pnpm build      # the whole site as static files in out/
```

## How it is put together

| File | What it does |
|---|---|
| `krantho.config.ts` | The site's name, description, address and structured data; `paths.contentDir` points at `../docs` so each page is dated by its last commit |
| `krantho.collections.ts` | The `docs` collection: the folder, the address of a page and its frontmatter fields |
| `lib/krantho.ts` | `createStaticSite()` and the sidebar's reading order |
| `app/` | The layout, the pages, `sitemap.xml`, `llms.txt` and `robots.txt` |
| `components/doc-page.tsx` | One page; it shows the file's own `# Title` |
| `components/brand.tsx` | The header's logo and GitHub link, drawn in the text colour |
| `app/icon.svg` | The tab icon: a copy of `docs/images/monogram.svg` |
| `next.config.ts` | The static export, served under the path of the site's address |

## The logo

`docs/images/logo.svg` is the logo, `monogram.svg` and `wordmark.svg` its two parts. They are drawn with strokes
only: circles of one radius and straight stems with round ends. The monogram is one bowl with a stem down on the
left and a stem up on the right, a p and a d sharing the bowl, and the wordmark ends in it, so it reads dedupd. The
files are black, and white in dark mode; the header draws the same shapes in the text colour.
