/**
 * Vite plugin that stubs missing licensed assets during build.
 *
 * Licensed assets (Icons8 SVGs) are gitignored and only exist locally.
 * On CI where these files are absent, Rollup fails to resolve `/licensed/`
 * imports. This plugin intercepts those missing imports and returns the
 * original URL path as-is, so the built HTML still references `/licensed/...`
 * at runtime — where the real assets exist on the server, deployed separately
 * via `task website:deploy:licensed`.
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import type { Plugin } from 'vite'

const PREFIX = '\0licensed:'

export default function licensedAssetFallback(): Plugin {
  return {
    name: 'licensed-asset-fallback',
    resolveId(id) {
      if (id.startsWith('/licensed/') && !existsSync(join('src/public', id))) {
        return PREFIX + id
      }
    },
    load(id) {
      if (id.startsWith(PREFIX)) {
        const originalPath = id.slice(PREFIX.length)
        return `export default ${JSON.stringify(originalPath)}`
      }
    },
  }
}
