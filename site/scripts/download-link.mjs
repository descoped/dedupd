// Points the home page's download button at the disk image of the latest
// release, so a click downloads the app. Runs after `next build`.
//
// docs/README.md links the button to the latest release's page on GitHub,
// which is also what the button keeps when GitHub cannot be asked: offline, or
// a release without a disk image. Otherwise every copy of that link in out/
// (the page, and the data Next loads when a reader comes to the page from
// another one) becomes the address of the release's .dmg, as GitHub lists it.
// The Pages workflow builds again when a release is published.

import { readdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const repo = 'descoped/dedupd'
const latest = `https://github.com/${repo}/releases/latest`
const out = join(import.meta.dirname, '..', 'out')

// Only a disk image attached to this repository's own release is used
const downloadAddress = new RegExp(
  `^https://github\\.com/${repo.replace('/', '\\/')}/releases/download/v[0-9][0-9A-Za-z.+-]*/dedupd-[0-9][0-9A-Za-z.+-]*\\.dmg$`,
)

async function latestDiskImage() {
  const headers = { Accept: 'application/vnd.github+json', 'User-Agent': 'dedupd-site-build' }
  // In the workflow the job's own read-only token, against the API's rate limit
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`
  const response = await fetch(`https://api.github.com/repos/${repo}/releases/latest`, {
    headers,
    signal: AbortSignal.timeout(15_000),
  })
  if (!response.ok) throw new Error(`GitHub answered ${response.status}`)
  const release = await response.json()
  const image = (release.assets ?? [])
    .map((asset) => asset.browser_download_url)
    .find((url) => typeof url === 'string' && downloadAddress.test(url))
  if (!image) throw new Error(`release ${release.tag_name} has no disk image`)
  return image
}

async function filesIn(dir) {
  const entries = await readdir(dir, { recursive: true, withFileTypes: true })
  return entries
    .filter((entry) => entry.isFile() && /\.(html|txt)$/.test(entry.name))
    .map((entry) => join(entry.parentPath, entry.name))
}

let image
try {
  image = await latestDiskImage()
} catch (error) {
  console.warn(`download button: kept ${latest} (${error.message})`)
  process.exit(0)
}

let changed = 0
for (const file of await filesIn(out)) {
  const text = await readFile(file, 'utf8')
  if (!text.includes(latest)) continue
  await writeFile(file, text.replaceAll(latest, image))
  changed += 1
}
console.log(`download button: ${image} (${changed} files)`)
