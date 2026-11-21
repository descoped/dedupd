'use client'

import { init } from '@plausible-analytics/tracker'
import { useEffect } from 'react'

// The address the site is published at; nowhere else is a visit counted
const DOMAIN = 'dedupd.com'

// Events go to a relay on the site's own domain
const ENDPOINT = 'https://d.dedupd.com/e'

let started = false

/**
 * Counts page views, without cookies. It is part of the site's own script and starts only at the published address,
 * so a local build is not counted.
 */
export function Analytics(): null {
  useEffect(() => {
    // init() throws when called twice, and effects run twice in development
    if (started || window.location.hostname !== DOMAIN) return
    started = true
    init({ domain: DOMAIN, endpoint: ENDPOINT, bindToWindow: false })
  }, [])

  return null
}
