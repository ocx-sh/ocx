/**
 * TypeScript port of the Rust version parsing and ordering logic.
 * Source of truth: crates/ocx_lib/src/package/version.rs
 *
 * Handles OCX's variant-prefix tag format: `{variant}-{version}` where
 * variants match `[a-z][a-z0-9.]*` and versions start with a digit.
 */

// --- Types ---

export interface Version {
  variant: string | null
  major: number
  minor: number | null
  patch: number | null
  prerelease: string | null
  build: string | null
}

export type ParsedTag =
  | { kind: 'latest' }
  | { kind: 'version'; version: Version; raw: string }
  | { kind: 'other'; raw: string }

/** A collapsible minor version group. */
export interface MinorGroup {
  minorTag: string   // full tag string, e.g. "slim-3.12"
  children: string[] // patch/build full tags, sorted newest-first
}

/** A major version group in the expanded view. */
export interface MajorGroup {
  major: number
  majorTag: string | null  // rolling major tag (e.g. "3", "slim-3") — null if not present in tags
  minorGroups: MinorGroup[]
}

/** A variant row in the version table. */
export interface VariantRow {
  variant: string | null
  label: string               // display label
  isDefault: boolean          // true when variant is null (the default variant)
  keyTags: string[]           // prominent tags: rolling → major → minor → patch → build (latest at each depth)
  majorGroups: MajorGroup[]   // expanded view: grouped by major version, sorted descending
}

/** Result of building the version table from a tag list. */
export interface VersionTable {
  rows: VariantRow[]      // one per variant, default first
  unknownTags: string[]   // tags that don't parse as versions or known rolling names
}

// --- Parsing ---

// Exact port of the Rust regex from Version::parse()
const VERSION_RE =
  /^(([a-z][a-z0-9.]*)-)?(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*)(-([0-9a-zA-Z]+))?([_+]([0-9a-zA-Z]+))?)?)?$/

export function parseVersion(tag: string): Version | null {
  const m = VERSION_RE.exec(tag)
  if (!m) return null

  // Group 2: variant name. "latest" is reserved.
  const variantStr = m[2] ?? null
  if (variantStr === 'latest') return null
  const variant = variantStr

  const major = parseInt(m[3], 10)
  const minorStr = m[5]
  if (minorStr === undefined) {
    return { variant, major, minor: null, patch: null, prerelease: null, build: null }
  }
  const minor = parseInt(minorStr, 10)

  const patchStr = m[7]
  if (patchStr === undefined) {
    return { variant, major, minor, patch: null, prerelease: null, build: null }
  }
  const patch = parseInt(patchStr, 10)

  const prerelease = m[9] ?? null
  const build = m[11] ?? null

  return { variant, major, minor, patch, prerelease, build }
}

export function parseTag(tag: string): ParsedTag {
  if (tag === 'latest') return { kind: 'latest' }
  const version = parseVersion(tag)
  if (version) return { kind: 'version', version, raw: tag }
  return { kind: 'other', raw: tag }
}

// --- Ordering ---

/**
 * Mirrors the Rust Ord impl for Version.
 * Returns negative if a < b, positive if a > b, 0 if equal.
 */
export function compareVersions(a: Version, b: Version): number {
  // Variant: None > Some (default variant sorts last)
  if (a.variant === null && b.variant !== null) return 1
  if (a.variant !== null && b.variant === null) return -1
  if (a.variant !== null && b.variant !== null) {
    const cmp = a.variant.localeCompare(b.variant)
    if (cmp !== 0) return cmp
  }

  // Major
  if (a.major !== b.major) return a.major - b.major

  // Minor: null > non-null (rolling sorts greater)
  if (a.minor === null && b.minor !== null) return 1
  if (a.minor !== null && b.minor === null) return -1
  if (a.minor !== null && b.minor !== null && a.minor !== b.minor) return a.minor - b.minor

  // Patch: null > non-null
  if (a.patch === null && b.patch !== null) return 1
  if (a.patch !== null && b.patch === null) return -1
  if (a.patch !== null && b.patch !== null && a.patch !== b.patch) return a.patch - b.patch

  // Prerelease: Some < None
  if (a.prerelease !== null && b.prerelease === null) return -1
  if (a.prerelease === null && b.prerelease !== null) return 1
  if (a.prerelease !== null && b.prerelease !== null) {
    const cmp = a.prerelease.localeCompare(b.prerelease)
    if (cmp !== 0) return cmp
  }

  // Build: Some < None
  if (a.build !== null && b.build === null) return -1
  if (a.build === null && b.build !== null) return 1
  if (a.build !== null && b.build !== null) {
    return a.build.localeCompare(b.build)
  }

  return 0
}

// --- Version depth ---

export function versionDepth(v: Version): number {
  if (v.patch !== null) return v.build !== null || v.prerelease !== null ? 4 : 3
  if (v.minor !== null) return 2
  return 1
}

// --- Table building ---

/**
 * Build the version table from a flat list of tags.
 *
 * Each variant gets a row with:
 * - keyTags: the cascade chain of the latest version (one tag per depth level),
 *   prefixed by rolling tags (latest, bare variant name).
 * - majorGroups: versioned tags grouped by major → minor, excluding keyTags.
 *
 * Tags that don't parse as versions and aren't known rolling names go into unknownTags.
 */
export function buildVersionTable(tags: string[]): VersionTable {
  // Classify all tags
  interface TagEntry {
    tag: string
    variant: string | null
    version: Version | null  // null for "latest" and bare variant names
    depth: number            // 0 for rolling, 1-4 for versioned
  }

  const variantEntries = new Map<string | null, TagEntry[]>()
  const unknownTags: string[] = []
  const knownVariants = new Set<string>()

  // First pass: collect all variant names from version tags
  for (const tag of tags) {
    const parsed = parseTag(tag)
    if (parsed.kind === 'version' && parsed.version.variant !== null) {
      knownVariants.add(parsed.version.variant)
    }
  }

  // Second pass: classify each tag
  for (const tag of tags) {
    const parsed = parseTag(tag)

    if (parsed.kind === 'latest') {
      pushEntry(null, { tag, variant: null, version: null, depth: 0 })
    } else if (parsed.kind === 'version') {
      const v = parsed.version
      pushEntry(v.variant, { tag, variant: v.variant, version: v, depth: versionDepth(v) })
    } else if (parsed.kind === 'other' && knownVariants.has(parsed.raw)) {
      // Bare variant name (e.g., "slim") — rolling tag for that variant
      pushEntry(parsed.raw, { tag: parsed.raw, variant: parsed.raw, version: null, depth: 0 })
    } else {
      unknownTags.push(parsed.raw)
    }
  }

  function pushEntry(variant: string | null, entry: TagEntry) {
    if (!variantEntries.has(variant)) variantEntries.set(variant, [])
    variantEntries.get(variant)!.push(entry)
  }

  // Sort variants: default first, then alphabetically
  const sortedVariants = [...variantEntries.keys()].sort((a, b) => {
    if (a === null) return -1
    if (b === null) return 1
    return a.localeCompare(b)
  })

  const rows: VariantRow[] = sortedVariants.map(variant => {
    const entries = variantEntries.get(variant)!

    // Sort all versioned entries newest-first (highest version first)
    const versioned = entries
      .filter(e => e.version !== null)
      .sort((a, b) => compareVersions(b.version!, a.version!))

    // Find the latest tag at each depth level
    const latestByDepth = new Map<number, string>()
    for (const e of versioned) {
      if (!latestByDepth.has(e.depth)) {
        latestByDepth.set(e.depth, e.tag)
      }
    }

    // Build key tags: rolling first, then depth 1 → 2 → 3 → 4
    const keyTags: string[] = []
    const keyTagSet = new Set<string>()

    // Add rolling tags (latest, bare variant name)
    const rollingEntries = entries.filter(e => e.depth === 0)
    for (const e of rollingEntries) {
      if (!keyTagSet.has(e.tag)) {
        keyTags.push(e.tag)
        keyTagSet.add(e.tag)
      }
    }

    // Add latest at each version depth
    for (const depth of [1, 2, 3, 4]) {
      const tag = latestByDepth.get(depth)
      if (tag && !keyTagSet.has(tag)) {
        keyTags.push(tag)
        keyTagSet.add(tag)
      }
    }

    // Build major groups from ALL versioned tag
    const allMajorTags = new Map<number, string>() // major → rolling major tag
    const majorMinorMap = new Map<number, Map<string, { minorTag: string; children: { tag: string; version: Version }[] }>>()

    for (const e of versioned) {
      const v = e.version!

      if (e.depth === 1) {
        // Major rolling tag (e.g., "3", "slim-3")
        if (!allMajorTags.has(v.major)) {
          allMajorTags.set(v.major, e.tag)
        }
      } else if (e.depth >= 2) {
        if (!majorMinorMap.has(v.major)) majorMinorMap.set(v.major, new Map())
        const minorMap = majorMinorMap.get(v.major)!
        const mk = `${v.major}.${v.minor}`

        if (e.depth === 2) {
          if (!minorMap.has(mk)) {
            minorMap.set(mk, { minorTag: e.tag, children: [] })
          }
        } else {
          if (!minorMap.has(mk)) {
            const prefix = v.variant ? `${v.variant}-` : ''
            minorMap.set(mk, { minorTag: `${prefix}${v.major}.${v.minor}`, children: [] })
          }
          minorMap.get(mk)!.children.push({ tag: e.tag, version: v })
        }
      }
    }

    // Collect all majors
    const allMajors = new Set<number>([
      ...allMajorTags.keys(),
      ...majorMinorMap.keys(),
    ])

    // Build major groups, sorted descending
    const majorGroups: MajorGroup[] = [...allMajors]
      .sort((a, b) => b - a)
      .map(major => {
        const majorTag = allMajorTags.get(major) ?? null
        const minorMap = majorMinorMap.get(major)

        let minorGroups: MinorGroup[] = []
        if (minorMap) {
          const sorted = [...minorMap.entries()].sort((a, b) => {
            const aMin = parseInt(a[0].split('.')[1], 10)
            const bMin = parseInt(b[0].split('.')[1], 10)
            return bMin - aMin
          })
          minorGroups = sorted.map(([, { minorTag, children }]) => {
            children.sort((a, b) => compareVersions(b.version, a.version))
            return { minorTag, children: children.map(c => c.tag) }
          })
        }

        return { major, majorTag, minorGroups }
      })

    return {
      variant,
      label: variant ?? 'default',
      isDefault: variant === null,
      keyTags,
      majorGroups,
    }
  })

  return { rows, unknownTags }
}
