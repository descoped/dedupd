// Copies the pictures of ../docs/images into public/images, where the pages
// link them (images/x.png in docs/ is /images/x.png on the site). Runs before
// `next build` and `next dev`; krantho's prepareImages (next.config.ts) then
// makes the AVIF and WebP copies the pages use. A picture docs/ no longer has
// is removed. docs/ keeps only the originals, so the pages read the same on
// GitHub.

import { copyFile, mkdir, readdir, rm, stat } from 'node:fs/promises'
import { join } from 'node:path'

const site = join(import.meta.dirname, '..')
const from = join(site, '..', 'docs', 'images')
const to = join(site, 'public', 'images')

const isNewer = async (source, target) => {
  try {
    return (await stat(source)).mtimeMs > (await stat(target)).mtimeMs
  } catch {
    return true
  }
}

await mkdir(to, { recursive: true })
const names = (await readdir(from)).filter((name) => !name.startsWith('.'))
for (const name of names) {
  // Only a changed picture is copied, so its time tells krantho it changed
  if (await isNewer(join(from, name), join(to, name))) await copyFile(join(from, name), join(to, name))
}
for (const name of await readdir(to)) {
  if (!names.includes(name)) await rm(join(to, name), { recursive: true })
}
console.log(`images: ${names.length} pictures copied from docs/images`)
