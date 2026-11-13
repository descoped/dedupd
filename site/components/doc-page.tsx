import type { Metadata } from 'next'
import Link from 'next/link'
import { notFound } from 'next/navigation'
import { CMSMarkdownServer, resolveSeoContent, SeoJsonLd } from '@krantho/ui-react/server'
import { docHref, docLinks, site } from '../lib/krantho'

/** A docs page's title, description and canonical address */
export async function docMetadata(slug: string): Promise<Metadata> {
  const seo = await resolveSeoContent('docs', slug)
  return {
    title: slug === 'index' ? { absolute: seo.title } : seo.title,
    description: seo.description,
    alternates: { canonical: docHref(slug) },
    openGraph: { title: seo.title, description: seo.description, url: docHref(slug) },
  }
}

/** A docs page: its title, description, markdown body and the pages before and after it */
export async function DocPage({ slug }: { slug: string }): Promise<React.ReactElement> {
  const doc = await site.loader.getPublished({ collection: 'docs', slug })
  if (!doc) notFound()
  const links = await docLinks()
  const at = links.findIndex((link) => link.slug === slug)
  const previous = at > 0 ? links[at - 1] : undefined
  const next = at >= 0 ? links[at + 1] : undefined
  // The files in docs/ are also read on GitHub, so each starts with its own
  // `# Title`; the page then shows that and not the title from the frontmatter
  const ownHeading = /^\s*# /.test(doc.body)
  return (
    <article className="doc">
      <SeoJsonLd collection="docs" slug={slug} />
      {!ownHeading && <h1>{String(doc.fields.title)}</h1>}
      {!ownHeading && <p className="lead">{String(doc.fields.description)}</p>}
      <CMSMarkdownServer collection="docs" slug={slug} className="prose" />
      <nav className="pager" aria-label="Pages">
        {previous && (
          <Link href={previous.href} rel="prev">
            ← {previous.title}
          </Link>
        )}
        {next && (
          <Link href={next.href} rel="next">
            {next.title} →
          </Link>
        )}
      </nav>
    </article>
  )
}
