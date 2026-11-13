import { join } from 'node:path'
import { defineCollection, defineDocumentArchetype } from '@krantho/schema'
import { z } from 'zod'

// One page per markdown file in the repository's docs/ folder, the same files
// GitHub shows. README.md is the home page, as it is the folder's page on
// GitHub, and every other file is served at /<file name>/. The path ends in a
// slash: each page is a folder in the static output.
const docs = defineCollection({
  name: 'docs',
  source: 'filesystem',
  directory: join(process.cwd(), '..', 'docs'),
  indexFile: 'README.md',
  path: '/:slug/',
  archetype: defineDocumentArchetype({
    id: 'site/docs',
    label: 'Docs',
    fields: z.object({
      title: z.string(),
      description: z.string(),
      group: z.string().describe('The sidebar group the page is listed under').optional(),
      order: z.number().describe("The page's place in its group").optional(),
    }),
  }),
})

export const collections = { docs }
