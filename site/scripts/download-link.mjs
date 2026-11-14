// Finds the disk image of the latest release, so the home page's download
// button can download the app. Runs before `next build` (and `next dev`) and
// writes generated/download.json, which lib/download.ts reads.
//
// docs/README.md links the button to the latest release's page on GitHub,
// which is what the button keeps when GitHub cannot be asked (offline, or a
// release without a disk image): the file then names no address. The Pages
// workflow builds again when a release is published.

import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

const repo = 'descoped/dedupd'
const file = join(import.meta.dirname, '..', 'generated', 'download.json')

// Only a disk image attached to this repository's own release is used
const downloadAddress =
  /^https:\/\/github\.com\/descoped\/dedupd\/releases\/download\/v[0-9][0-9A-Za-z.+-]*\/dedupd-[0-9][0-9A-Za-z.+-]*\.dmg$/

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

let image = null
try {
  image = await latestDiskImage()
  console.log(`download button: ${image}`)
} catch (error) {
  console.warn(`download button: the latest release's page (${error.message})`)
}
await mkdir(join(file, '..'), { recursive: true })
await writeFile(file, `${JSON.stringify({ image }, null, 2)}\n`)
