import type { Metadata, Viewport } from 'next'
import Link from 'next/link'
import { GitHubLink, Logo } from '../components/brand'
import { MenuButton } from '../components/menu-button'
import { docLinks, site } from '../lib/krantho'
import './globals.css'

const { name, description, baseUrl } = site.config.site

export const metadata: Metadata = {
  // Absolute URLs (canonical, Open Graph) start at the site's address, path included
  metadataBase: new URL(`${baseUrl.replace(/\/$/, '')}/`),
  title: { default: name, template: `%s — ${name}` },
  description,
}

// A phone lays the page out at its own width; its browser bars take the
// page's background, light or dark
export const viewport: Viewport = {
  width: 'device-width',
  initialScale: 1,
  themeColor: [
    { media: '(prefers-color-scheme: light)', color: '#ffffff' },
    { media: '(prefers-color-scheme: dark)', color: '#111418' },
  ],
}

// Plausible's start code, as its site gives it: events sent before the script
// has loaded wait in a queue
const plausibleStart =
  'window.plausible=window.plausible||function(){(plausible.q=plausible.q||[]).push(arguments)},plausible.init=plausible.init||function(i){plausible.o=i||{}};plausible.init()'

export default async function RootLayout({
  children,
}: {
  children: React.ReactNode
}): Promise<React.ReactElement> {
  const links = await docLinks()
  const groups = [...new Set(links.map((link) => link.group))]
  return (
    <html lang="en">
      <head>
        {/* Visitor counts by Plausible: no cookies and nothing personal kept.
            It counts on dedupd.com only, not on a local build. */}
        <script async src="https://plausible.io/js/pa-pVJk-vPEkrwnk8SufwfWj.js" />
        <script dangerouslySetInnerHTML={{ __html: plausibleStart }} />
      </head>
      <body>
        <header className="site-header">
          <div className="site-header-inner">
            <Link href="/" className="site-name" aria-label={`${name} home`}>
              <Logo />
            </Link>
            <div className="header-actions">
              <GitHubLink />
              <MenuButton />
            </div>
          </div>
        </header>
        <div className="site-body">
          <nav id="site-nav" className="sidebar" aria-label="Documentation">
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
        <footer className="site-footer">
          <div className="site-footer-inner">
            <span>
              {name} is open source under the{' '}
              <a href="https://github.com/descoped/dedupd/blob/main/LICENSE">MIT licence</a>
            </span>
            <span>
              Built with <a href="https://krantho.com">Krantho</a>
            </span>
          </div>
        </footer>
      </body>
    </html>
  )
}
