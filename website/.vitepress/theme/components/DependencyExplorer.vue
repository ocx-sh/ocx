<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'

interface ComponentLinks {
  cratesIo?: string
  docsRs?: string
  repository?: string
  website?: string
}

interface DependencyComponent {
  name: string
  version: string
  license: string
  description: string
  author: string
  scope: string
  links: ComponentLinks
}

interface BinarySummary {
  total: number
  required: number
  excluded: number
  uniqueLicenses: number
  licenses: Record<string, number>
}

interface BinaryData {
  version: string
  license: string
  target: string
  summary: BinarySummary
  components: DependencyComponent[]
}

interface DependencyData {
  generated: string
  binaries: Record<string, BinaryData>
}

const data = ref<DependencyData | null>(null)
const loading = ref(true)
const error = ref('')
const searchQuery = ref('')
const selectedLicense = ref('')
const expandedRows = ref<Set<string>>(new Set())

function compKey(comp: DependencyComponent): string {
  return `${comp.name}@${comp.version}`
}

const binary = computed(() => {
  if (!data.value) return null
  const name = Object.keys(data.value.binaries)[0]
  return name ? data.value.binaries[name] : null
})

const licenseOptions = computed(() => {
  if (!binary.value) return []
  const entries = Object.entries(binary.value.summary.licenses)
  entries.sort((a, b) => b[1] - a[1])
  return entries
})

const filteredComponents = computed(() => {
  if (!binary.value) return []
  let result = binary.value.components

  const query = searchQuery.value.toLowerCase()
  if (query) {
    result = result.filter(c =>
      c.name.toLowerCase().includes(query)
      || c.description.toLowerCase().includes(query),
    )
  }

  if (selectedLicense.value) {
    result = result.filter(c => c.license === selectedLicense.value)
  }

  return result
})

function toggleRow(key: string) {
  if (expandedRows.value.has(key)) {
    expandedRows.value.delete(key)
  } else {
    expandedRows.value.add(key)
  }
}

function hasDetail(comp: DependencyComponent): boolean {
  return !!(comp.description || comp.author)
}

function hasLinks(comp: DependencyComponent): boolean {
  return !!(comp.links.cratesIo || comp.links.docsRs || comp.links.repository)
}

onMounted(async () => {
  try {
    const resp = await fetch('/data/dependencies.json')
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
    data.value = await resp.json()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Failed to load dependency data'
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="explorer">
    <!-- Loading -->
    <div v-if="loading" class="loading">
      <div class="spinner" />
      <span>Loading dependency data…</span>
    </div>

    <!-- Error -->
    <div v-else-if="error" class="error">
      Failed to load dependencies: {{ error }}
    </div>

    <!-- Content -->
    <template v-else-if="binary">
      <!-- Summary -->
      <div class="summary">
        <div class="stat">
          <span class="stat-value">{{ binary.summary.total }}</span>
          <span class="stat-label">Dependencies</span>
        </div>
        <div class="stat">
          <span class="stat-value">{{ binary.summary.uniqueLicenses }}</span>
          <span class="stat-label">Licenses</span>
        </div>
        <div class="stat">
          <span class="stat-value">{{ data!.generated }}</span>
          <span class="stat-label">Generated</span>
        </div>
      </div>

      <!-- Controls -->
      <div class="controls">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search dependencies…"
          class="search"
        >
        <select v-model="selectedLicense" class="select">
          <option value="">
            All licenses
          </option>
          <option v-for="[lic, count] in licenseOptions" :key="lic" :value="lic">
            {{ lic }} ({{ count }})
          </option>
        </select>
      </div>

      <!-- Results count -->
      <div class="results-count">
        Showing {{ filteredComponents.length }} of {{ binary.summary.total }} components
      </div>

      <!-- Table -->
      <div class="table-wrap">
        <table class="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Version</th>
              <th class="hide-mobile">License</th>
              <th class="hide-mobile">Links</th>
            </tr>
          </thead>
          <tbody>
            <template v-for="comp in filteredComponents" :key="compKey(comp)">
              <tr
                :class="{ clickable: hasDetail(comp) }"
                @click="hasDetail(comp) && toggleRow(compKey(comp))"
              >
                <td>
                  <span class="name">
                    <svg
                      v-if="hasDetail(comp)"
                      class="chevron"
                      :class="{ open: expandedRows.has(compKey(comp)) }"
                      width="12"
                      height="12"
                      viewBox="0 0 12 12"
                      fill="none"
                    >
                      <path d="M4 2.5L7.5 6L4 9.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                    <span v-else class="chevron-spacer" />
                    {{ comp.name }}
                  </span>
                </td>
                <td class="version">
                  {{ comp.version }}
                </td>
                <td class="hide-mobile license">
                  {{ comp.license }}
                </td>
                <td class="hide-mobile" @click.stop>
                  <span class="links">
                    <a v-if="comp.links.cratesIo" :href="comp.links.cratesIo" target="_blank" rel="noopener">crates.io</a>
                    <a v-if="comp.links.docsRs" :href="comp.links.docsRs" target="_blank" rel="noopener">docs</a>
                    <a v-if="comp.links.repository" :href="comp.links.repository" target="_blank" rel="noopener">repo</a>
                  </span>
                </td>
              </tr>
              <tr v-if="hasDetail(comp) && expandedRows.has(compKey(comp))" class="detail-row">
                <td colspan="4">
                  <div class="detail">
                    <div v-if="comp.description" class="detail-desc">
                      {{ comp.description }}
                    </div>
                    <div v-if="comp.author" class="detail-author">
                      By {{ comp.author }}
                    </div>
                    <!-- Mobile-only: license and links hidden from table columns -->
                    <div class="detail-mobile">
                      <div v-if="comp.license" class="detail-license">
                        License: {{ comp.license }}
                      </div>
                      <div v-if="hasLinks(comp)" class="detail-links">
                        <a v-if="comp.links.cratesIo" :href="comp.links.cratesIo" target="_blank" rel="noopener">crates.io</a>
                        <a v-if="comp.links.docsRs" :href="comp.links.docsRs" target="_blank" rel="noopener">docs</a>
                        <a v-if="comp.links.repository" :href="comp.links.repository" target="_blank" rel="noopener">repo</a>
                      </div>
                    </div>
                  </div>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </div>

      <!-- Empty state -->
      <div v-if="filteredComponents.length === 0 && !loading" class="empty">
        No dependencies match the current filters.
      </div>
    </template>
  </div>
</template>

<style scoped>
.explorer {
  margin: 1rem 0;
}

/* Summary cards */
.summary {
  display: flex;
  gap: 1rem;
  flex-wrap: wrap;
  margin-bottom: 1.25rem;
}

.stat {
  flex: 1;
  min-width: 120px;
  padding: 0.75rem 1rem;
  background: var(--vp-c-bg-soft);
  border-radius: 8px;
  border: 1px solid var(--vp-c-divider);
}

.stat-value {
  display: block;
  font-size: 1.1rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
  line-height: 1.4;
}

.stat-label {
  display: block;
  font-size: 0.8rem;
  color: var(--vp-c-text-3);
  margin-top: 0.15rem;
}

/* Controls */
.controls {
  display: flex;
  gap: 0.75rem;
  margin-bottom: 0.75rem;
}

.search {
  flex: 1;
  padding: 0.5rem 0.75rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-size: 0.875rem;
  outline: none;
  transition: border-color 0.2s;
}

.search:focus {
  border-color: var(--vp-c-brand);
}

.search::placeholder {
  color: var(--vp-c-text-3);
}

.select {
  padding: 0.5rem 0.75rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-size: 0.875rem;
  outline: none;
  cursor: pointer;
  min-width: 180px;
  transition: border-color 0.2s;
}

.select:focus {
  border-color: var(--vp-c-brand);
}

/* Results count */
.results-count {
  font-size: 0.8rem;
  color: var(--vp-c-text-3);
  margin-bottom: 0.5rem;
}

/* Table — override VitePress .vp-doc table defaults */
.table-wrap {
  overflow-x: auto;
}

.table {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.875rem;
  margin: 0;
  display: table;
}

.table th {
  text-align: left;
  padding: 0.6rem 0.75rem;
  border: none;
  border-bottom: 2px solid var(--vp-c-divider);
  background: transparent;
  color: var(--vp-c-text-2);
  font-weight: 600;
  font-size: 0.8rem;
  text-transform: uppercase;
  letter-spacing: 0.02em;
  white-space: nowrap;
}

.table tr {
  background: transparent !important;
  border-top: none;
  transition: none;
}

.table td {
  padding: 0.5rem 0.75rem;
  border: none;
  border-bottom: 1px solid var(--vp-c-divider);
  background: transparent;
  color: var(--vp-c-text-1);
  vertical-align: top;
}

.clickable {
  cursor: pointer;
}

.table tbody tr:not(.detail-row):hover {
  background: var(--vp-c-bg-soft) !important;
}

/* Name */
.name {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  font-weight: 500;
}

.chevron {
  flex-shrink: 0;
  color: var(--vp-c-text-3);
  transition: transform 0.15s;
}

.open {
  transform: rotate(90deg);
}

.chevron-spacer {
  display: inline-block;
  width: 12px;
}

.version {
  font-family: var(--vp-font-family-mono);
  font-size: 0.8rem;
  white-space: nowrap;
}

.license {
  font-size: 0.8rem;
  color: var(--vp-c-text-2);
}

/* Links */
.links {
  display: inline-flex;
  gap: 0.5rem 0.75rem;
  flex-wrap: wrap;
}

.links a,
.detail-links a {
  font-size: 0.75rem;
  font-weight: 500;
  color: var(--vp-c-brand);
  text-decoration: none;
  transition: color 0.15s;
}

.links a:hover,
.detail-links a:hover {
  color: var(--vp-c-brand-dark);
  text-decoration: underline;
}

/* Detail row */
.detail-row td {
  border-bottom: 1px solid var(--vp-c-divider);
}

.detail {
  padding: 0.5rem 0.75rem 0.5rem 2.1rem;
  font-size: 0.825rem;
  line-height: 1.5;
}

.detail-desc {
  color: var(--vp-c-text-2);
  margin-bottom: 0.15rem;
}

.detail-author {
  color: var(--vp-c-text-3);
  font-size: 0.8rem;
}

.detail-mobile {
  display: none;
}

/* Loading */
.loading {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  padding: 2rem;
  color: var(--vp-c-text-3);
  font-size: 0.875rem;
}

.spinner {
  width: 20px;
  height: 20px;
  border: 2px solid var(--vp-c-divider);
  border-top-color: var(--vp-c-brand);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

/* Error */
.error {
  padding: 1rem;
  background: var(--vp-c-danger-soft);
  color: var(--vp-c-danger-1);
  border-radius: 8px;
  font-size: 0.875rem;
}

/* Empty state */
.empty {
  padding: 2rem;
  text-align: center;
  color: var(--vp-c-text-3);
  font-size: 0.875rem;
}

/* Responsive */
@media (max-width: 640px) {
  .controls {
    flex-direction: column;
  }

  .select {
    min-width: 0;
    width: 100%;
  }

  .hide-mobile {
    display: none;
  }

  .detail {
    padding-left: 0.75rem;
  }

  .detail-mobile {
    display: block;
    margin-top: 0.3rem;
  }

  .detail-license {
    color: var(--vp-c-text-2);
    font-size: 0.8rem;
    margin-bottom: 0.25rem;
  }

  .detail-links {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .summary {
    gap: 0.5rem;
  }

  .stat {
    min-width: 0;
    flex-basis: calc(50% - 0.25rem);
  }
}
</style>
