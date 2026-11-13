import type { Metadata } from 'next'
import { DocPage, docMetadata } from '../components/doc-page'

// content/docs/index.md
export function generateMetadata(): Promise<Metadata> {
  return docMetadata('index')
}

export default function Home(): Promise<React.ReactElement> {
  return DocPage({ slug: 'index' })
}
