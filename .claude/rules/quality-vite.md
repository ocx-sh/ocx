---
paths:
  - "**/vite.config.*"
  - "**/vitest.config.*"
  - "**/astro.config.*"
---

# Vite Build Tool Quality

Vite opinions for Vite 7/8 (2026). Not cookbook — config examples + API ref at [vite.dev](https://vite.dev/). This file = **opinions** (avoid/prefer) senior dev insist on in review.

Project-independent, shareable.

---

## Anti-Patterns

### Block (must fix before merge)

1. **Hardcoded credentials or secrets** in `vite.config.*` — use env vars, never commit.
2. **`VITE_` prefix on server-only env vars** — exposes to client bundle at build. Never prefix secrets `VITE_`.
3. **Browser API at module scope** in components used in Astro/SSR — `window`, `document`, `localStorage` at import time crash pre-render. Move it into a client `<script>` or guard `import.meta.env.SSR`.
4. **`vite.config.ts` at root alongside `astro.config.mjs`** — Astro takes Vite options from the `vite` key and ignores root `vite.config.ts`; split = silent override bugs.
5. **`build.target: "es5"`** with Rolldown (Vite 8) — not supported. Rolldown = modern JS only.
6. **Mixing SSR and client globals** in shared modules — pre-render crash.
7. **Missing env var validation** at config load — validate `zod`/`valibot`; fail build on invalid env.

### Warn (should fix)

- Missing `base` option for non-root deployments
- `optimizeDeps.include` as workaround instead of fixing underlying ESM incompat
- `vite-plugin-*` wrappers duplicating Vite 8 built-ins (e.g., `vite-tsconfig-paths` now built-in)
- Missing `.astro` cache dir in `.gitignore`
- Experimental `fullBundleMode` in production — not stable
- No explicit `build.outDir` — defaults differ between app mode and library mode
- `resolve.alias` with absolute paths — use `fileURLToPath(new URL(..., import.meta.url))` for portability

---

## Env Var Discipline

- **`VITE_*` prefix = client-exposure switch** — prefix → browser bundle at build
- **`.env.local` gitignored**; `.env` committed (NO secrets, public defaults only)
- **Validate env vars at config load** with `zod`/`valibot`; fail fast on missing/invalid
- **Never read `process.env` at module scope in client code** — resolved at build, not runtime

```ts
// Good: validate at config load
import { z } from "zod";
const env = z.object({
  VITE_API_URL: z.string().url(),
  VITE_PUBLIC_KEY: z.string().min(1),
}).parse(import.meta.env);
```

---

## Vite 7 → Vite 8 Migration (2026)

- **Rolldown replaces Rollup** for prod builds in Vite 8 — 10-30x faster
- Internal plugins (`alias`, `resolve`) now Rust-native via `nativePlugins: 'v1'`
- **`resolve.tsconfigPaths: true`** built-in — drop `vite-tsconfig-paths` plugin
- **Node.js baseline**: 20.19+ / 22.12+ (Vite 7+); Node 18 EOL for Vite — upgrade base images
- Default browser target changed: `'baseline-widely-available'` instead of `'modules'`
- `build.rollupOptions` maps directly to Rolldown options — same API surface
- Plugin API Rollup-compatible — existing plugins work unchanged

---

## Astro-Specific Gotchas

1. **Pre-render runs in Node** — any browser API at import time crashes `astro build`. Use a client `<script>` (runs in the browser only) or guard `import.meta.env.SSR`.
2. **Vite options live under the `vite` key of `astro.config.*`**, same `UserConfig` shape. No separate root `vite.config.ts` — Astro ignores it.
3. **Full typing**: `defineConfig` from `astro/config` catches malformed integration/sidebar config at author time.

---

## Config Structure Recommendations

```typescript
// vite.config.ts or the `vite` key of astro.config.mjs — typed, minimal
import { defineConfig } from "vite";

export default defineConfig(({ command, isSsrBuild }) => ({
  build: {
    target: "baseline-widely-available", // Vite 7+ default; explicit for visibility
  },
}));
```

- Use function form of `defineConfig` only when config branches on `command` or `isSsrBuild`
- **Library mode**: set `build.lib` with `entry`, `formats`, `fileName`. App-mode config for libraries wrong — tree-shaking + externalization differ
- **Extract reusable plugin arrays** to local helper — don't duplicate between `vitest.config.ts` and `vite.config.ts`

---

## Code Review Checklist (Vite-Specific)

See `quality-core.md` for universal checklist. Vite-specific additions:

- [ ] No secrets or credentials in `vite.config.*`
- [ ] No `VITE_` prefix on server-only env vars
- [ ] Env vars validated at config load (zod/valibot)
- [ ] `base` option set for non-root deployments
- [ ] Browser API access guarded for SSR if Astro
- [ ] No duplicate config files (root `vite.config.ts` + `astro.config.*`)
- [ ] `build.outDir` specified explicitly
- [ ] `.astro` cache dir in `.gitignore`
- [ ] Target set to `'baseline-widely-available'` or explicit browserslist

---

## Sources

Authoritative refs used in this rule:

- [Vite 8.0 announcement](https://vite.dev/blog/announcing-vite8)
- [Vite 7.0 announcement](https://vite.dev/blog/announcing-vite7)
- [Rolldown integration guide](https://v7.vite.dev/guide/rolldown)
- [Astro configuration reference](https://docs.astro.build/en/reference/configuration-reference/)