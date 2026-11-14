import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

/** Where docs/README.md's download button leads: the latest release's page */
const latestRelease = 'https://github.com/descoped/dedupd/releases/latest'

const image =
  /^https:\/\/github\.com\/descoped\/dedupd\/releases\/download\/v[0-9][0-9A-Za-z.+-]*\/dedupd-[0-9][0-9A-Za-z.+-]*\.dmg$/

/**
 * A page's HTML with the download button pointed at the latest release's disk
 * image, as scripts/download-link.mjs found it before the build. Without one
 * the button keeps the release page.
 */
export async function withDownloadLink(html: string): Promise<string> {
  if (!html.includes(latestRelease)) return html
  let found: unknown
  try {
    const text = await readFile(join(process.cwd(), 'generated', 'download.json'), 'utf8')
    found = (JSON.parse(text) as { image?: unknown }).image
  } catch {
    return html
  }
  return typeof found === 'string' && image.test(found)
    ? html.replaceAll(`href="${latestRelease}"`, `href="${found}"`)
    : html
}
