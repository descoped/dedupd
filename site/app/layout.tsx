import type { Metadata } from 'next'
import Link from 'next/link'
import { GitHubLink, Logo } from '../components/brand'
import { docLinks, site } from '../lib/krantho'
import './globals.css'

const { name, description, baseUrl } = site.config.site

export const metadata: Metadata = {
  // Absolute URLs (canonical, Open Graph) start at the site's address, path included
  metadataBase: new URL(`${baseUrl.replace(/\/$/, '')}/`),
  title: { default: name, template: `%s — ${name}` },
  description,
}

export default async function RootLayout({
  children,
}: {
  children: React.ReactNode
}): Promise<React.ReactElement> {
  const links = await docLinks()
  const groups = [...new Set(links.map((link) => link.group))]
  return (
    <html lang="en">
      <body>
        <header className="site-header">
          <Link href="/" className="site-name" aria-label={`${name} home`}>
            <Logo />
          </Link>
          <GitHubLink />
        </header>
        <div className="site-body">
          <nav className="sidebar" aria-label="Documentation">
            {groups.map((group) => (
              <section key={group}>
                {group && <h2>{group}</h2>}
                <ul>
                  {links
                    .filter((link) => link.group === group)
                    .map((link) => (
                      <li key={link.slug}>
                        <Link href={link.href}>{link.title}</Link>
                      </li>
                    ))}
                </ul>
              </section>
            ))}
          </nav>
          <main>{children}</main>
        </div>
      </body>
    </html>
  )
}
