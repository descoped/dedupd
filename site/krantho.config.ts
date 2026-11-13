import { defineKranthoConfig } from '@krantho/config'

/**
 * The documentation site. Its pages are the markdown files in ../docs, the same
 * files GitHub shows in the repository. The Pages workflow sets
 * KRANTHO_SITE_BASE_URL to the address Pages serves the site at; its path is the
 * path every page is served under.
 */
export default defineKranthoConfig({
  site: {
    domain: 'localhost',
    name: 'dedupd',
    description:
      'One copy of each AI model on disk, shared by every app that runs it: Ollama, LM Studio, MLX tools, llama.cpp and Hugging Face clients.',
    language: 'en',
    baseUrl: 'http://localhost:3000',
  },
  // The folder whose git history dates each page
  paths: { contentDir: '../docs' },
  // The markdown files are the content: no database, every page written at build
  persistence: { provider: 'filesystem' },
  cache: { pages: 'none' },
  // The structured data each page carries
  seo: { identity: 'SoftwareApplication', collections: { docs: ['TechArticle'] } },
})
