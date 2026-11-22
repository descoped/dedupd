import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

/** A picture as scripts/images.mjs made its copies */
interface Picture {
  width: number
  height: number
  widths: number[]
  formats: string[]
}

const mimeOf: Record<string, string> = { avif: 'image/avif', webp: 'image/webp' }

let pictures: Promise<Record<string, Picture>> | undefined

/** generated/images.json, or nothing when the script has not run */
function readPictures(): Promise<Record<string, Picture>> {
  pictures ??= readFile(join(process.cwd(), 'generated', 'images.json'), 'utf8')
    .then((text) => JSON.parse(text) as Record<string, Picture>)
    .catch(() => ({}))
  return pictures
}

/**
 * The attributes of an <img> tag as krantho renders it (name="value", in
 * double quotes), with their values still escaped as they were in the HTML
 */
function attributesOf(tag: string): Map<string, string> {
  const found = new Map<string, string>()
  for (const [, name, value] of tag.matchAll(/([a-zA-Z-]+)="([^"]*)"/g)) found.set(name, value)
  return found
}

/**
 * A page's HTML with each picture of docs/images as a <picture>: AVIF and
 * WebP copies at a few widths, the PNG as the fallback, its width and height
 * so the text does not move while it loads, and every picture after the
 * first loaded only when it is near the screen. A picture without copies
 * stays as it is.
 */
export async function withPictures(html: string): Promise<string> {
  const known = await readPictures()
  let first = true
  return html.replace(/<img\s[^>]*>/g, (tag) => {
    const attributes = attributesOf(tag)
    const src = attributes.get('src') ?? ''
    const at = /^(.*\/images\/)([^/]+\.png)$/.exec(src)
    const picture = at ? known[at[2]] : undefined
    if (!at || !picture) return tag
    const [, folder, name] = at
    const base = name.slice(0, -'.png'.length)

    // A width the markdown gives (the menu bar panel's 340) is the width shown
    const given = Number(attributes.get('width'))
    const width = given > 0 ? given : picture.width
    const height = Math.round((width * picture.height) / picture.width)
    const sizes = given > 0 ? `${given}px` : `(max-width: 46rem) 100vw, 46rem`

    const sources = picture.formats
      .map((format) => {
        const srcset = picture.widths.map((w) => `${folder}${base}-${w}.${format} ${w}w`).join(', ')
        return `<source type="${mimeOf[format]}" srcset="${srcset}" sizes="${sizes}">`
      })
      .join('')
    attributes.set('width', String(width))
    attributes.set('height', String(height))
    attributes.set('decoding', 'async')
    if (!first) attributes.set('loading', 'lazy')
    first = false
    const img = `<img ${[...attributes].map(([key, value]) => `${key}="${value}"`).join(' ')}>`
    return `<picture>${sources}${img}</picture>`
  })
}
