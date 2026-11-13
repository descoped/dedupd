import { resolveConfig } from '@krantho/config'
import { basePathOf } from '@krantho/static/base-path'
import type { NextConfig } from 'next'
import kranthoConfig from './krantho.config'

// `next build` writes the site to out/ as static files. They are served under
// the path of site.baseUrl: /<repository> on a GitHub Pages project site, and
// none on a custom domain. Each page is a folder (about/index.html), which
// every static host serves at /about/.
const nextConfig: NextConfig = {
  output: 'export',
  basePath: basePathOf(resolveConfig(kranthoConfig, process.env).site.baseUrl),
  trailingSlash: true,
  images: { unoptimized: true },
  // Loaded by Node at build, not bundled: they load optional packages on demand
  serverExternalPackages: ['@krantho/static', '@krantho/server'],
}

export default nextConfig
