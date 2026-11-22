// Puts the pictures of ../docs/images into public/images for the site, with
// smaller copies in AVIF and WebP beside each PNG, and writes
// generated/images.json, which lib/images.ts reads to turn each <img> of a
// page into a <picture> the browser picks the smallest fitting copy from.
//
// docs/ keeps only the PNGs, so the pages read the same on GitHub. A copy is
// made again only when its PNG is newer, or when the widths or the formats'
// settings below have changed, so `pnpm dev` starts quickly.

import { copyFile, mkdir, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import sharp from 'sharp'

const site = join(import.meta.dirname, '..')
const from = join(site, '..', 'docs', 'images')
const to = join(site, 'public', 'images')
const manifest = join(site, 'generated', 'images.json')

// The text column is 46rem (736 px); 340 px is the width the docs give the
// menu bar panel. Each picture gets those of these widths it is wider than,
// and its own width when that is no more than the largest.
const widths = [340, 736, 1472]
const formats = {
  avif: { quality: 55, effort: 6 },
  webp: { quality: 82, effort: 6 },
}

// The settings the copies in public/images were made with; other settings
// make every copy again
const settingsFile = join(to, '.settings.json')
const settings = JSON.stringify({ widths, formats })
const sameSettings = await readFile(settingsFile, 'utf8').then(
  (text) => text === settings,
  () => false,
)

const isNewer = async (source, target) => {
  if (!sameSettings && !target.endsWith('.png')) return true
  try {
    return (await stat(source)).mtimeMs > (await stat(target)).mtimeMs
  } catch {
    return true
  }
}

await mkdir(to, { recursive: true })
await mkdir(join(manifest, '..'), { recursive: true })

const sources = (await readdir(from)).filter((name) => !name.startsWith('.'))
const keep = new Set(['.settings.json'])
const pictures = {}
for (const name of sources) {
  const source = join(from, name)
  keep.add(name)
  if (await isNewer(source, join(to, name))) await copyFile(source, join(to, name))
  if (!name.endsWith('.png')) continue

  const base = name.slice(0, -'.png'.length)
  const { width, height } = await sharp(source).metadata()
  const sizes = widths.filter((w) => w < width)
  if (width <= widths.at(-1)) sizes.push(width)
  for (const w of sizes) {
    for (const [format, options] of Object.entries(formats)) {
      const file = `${base}-${w}.${format}`
      keep.add(file)
      if (await isNewer(source, join(to, file))) {
        await sharp(source).resize({ width: w }).toFormat(format, options).toFile(join(to, file))
      }
    }
  }
  pictures[name] = { width, height, widths: sizes, formats: Object.keys(formats) }
}

// Copies of a picture docs/ no longer has
for (const name of await readdir(to)) {
  if (!keep.has(name)) await rm(join(to, name), { recursive: true })
}

await writeFile(settingsFile, settings)
await writeFile(manifest, `${JSON.stringify(pictures, null, 2)}\n`)
console.log(`images: ${Object.keys(pictures).length} pictures, in ${Object.keys(formats).join(' and ')}`)
