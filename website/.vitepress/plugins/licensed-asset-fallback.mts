/**
 * Vite plugin that stubs missing licensed assets during build.
 *
 * Licensed assets (Icons8 SVGs) are gitignored and only exist locally.
 * On CI where these files are absent, this plugin returns a placeholder
 * SVG for any `/licensed/` import that can't be resolved on disk, allowing
 * the VitePress build to succeed. Real assets are deployed separately
 * via `task website:deploy:licensed`.
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import type { Plugin } from 'vite'

const PLACEHOLDER_ID = '\0licensed-placeholder'

// A minimal SVG placeholder: dashed border box with an "image missing" icon
// (mountain + broken corner). Renders at any size, neutral gray, no text.
const PLACEHOLDER_SVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" fill="none">
  <rect x="2" y="2" width="44" height="44" rx="4" stroke="#ccc" stroke-width="2" stroke-dasharray="4 3"/>
  <path d="M14 32l6-8 4 5 6-10 8 13H14z" fill="#ddd"/>
  <circle cx="18" cy="18" r="3" fill="#ddd"/>
</svg>`

const DATA_URI = `data:image/svg+xml,${encodeURIComponent(PLACEHOLDER_SVG)}`

export default function licensedAssetFallback(): Plugin {
  return {
    name: 'licensed-asset-fallback',
    resolveId(id) {
      if (id.startsWith('/licensed/') && !existsSync(join('src/public', id))) {
        return PLACEHOLDER_ID
      }
    },
    load(id) {
      if (id === PLACEHOLDER_ID) {
        return `export default ${JSON.stringify(DATA_URI)}`
      }
    },
  }
}
