import { createStaticSite } from '@krantho/static'
import { initServerLoader } from '@krantho/ui-react/server'
import config from '../krantho.config'
import { collections } from '../krantho.collections'

// Pages read the markdown in content/docs through the site's loader. Links and
// images in the markdown that start at the site's root (/setup/) get the path
// the site is served under, as next/link does for the site's own links.
export const site = createStaticSite({ config, collections })
initServerLoader(site.loader, { basePath: site.basePath })

/** A docs page as the sidebar and the pager list it */
export interface DocLink {
  slug: string
  title: string
  href: string
  group: string
}

/** The address of a docs page: index.md is the home page */
export function docHref(slug: string): string {
  return slug === 'index' ? '/' : `/${slug}/`
}

const orderOf = (fields: Record<string, unknown>): number =>
  typeof fields.order === 'number' ? fields.order : Number.MAX_SAFE_INTEGER

/**
 * The published docs pages in reading order: the home page, then each group
 * where its first page falls, and the pages of a group by `order`, then title
 */
export async function docLinks(): Promise<DocLink[]> {
  const docs = await site.loader.listPublishedSummaries('docs')
  const sorted = [...docs].sort(
    (a, b) =>
      orderOf(a.fields) - orderOf(b.fields) ||
      String(a.fields.title).localeCompare(String(b.fields.title)),
  )
  const groups: string[] = []
  for (const doc of sorted) {
    const group = typeof doc.fields.group === 'string' ? doc.fields.group : ''
    if (!groups.includes(group)) groups.push(group)
  }
  const links = sorted.map((doc) => ({
    slug: doc.slug,
    title: String(doc.fields.title),
    href: docHref(doc.slug),
    group: typeof doc.fields.group === 'string' ? doc.fields.group : '',
  }))
  const home = links.filter((link) => link.slug === 'index')
  const rest = links.filter((link) => link.slug !== 'index')
  return [...home, ...groups.flatMap((group) => rest.filter((link) => link.group === group))]
}
