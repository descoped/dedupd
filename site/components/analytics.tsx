'use client'

import { init } from '@plausible-analytics/tracker'
import { useEffect } from 'react'

// The address the site is published at; nowhere else is a visit counted
const DOMAIN = 'dedupd.com'

// Events go to Descoped's own relay, which forwards them to Plausible
const ENDPOINT = 'https://e.descoped.io/e'

let started = false

/**
 * Counts page views with Plausible: no cookies, and nothing that identifies the reader is kept. The tracker is part of
 * the site's own script, so no script is loaded from elsewhere. It starts only at the published address, so a local
 * build is not counted.
 */
export function Analytics(): null {
  useEffect(() => {
    // init() throws when called twice, and effects run twice in development
    if (started || window.location.hostname !== DOMAIN) return
    started = true
    init({ domain: DOMAIN, endpoint: ENDPOINT })
  }, [])

  return null
}
