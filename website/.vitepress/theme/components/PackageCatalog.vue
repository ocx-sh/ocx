<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vitepress'
import CopySnippet from './CopySnippet.vue'

interface PackageSummary {
  name: string
  registry: string
  repository: string
  title: string
  description: string
  keywords: string[]
  hasLogo: boolean
  logoExt: string
  hasReadme: boolean
  tagCount: number
  platforms: string[]
  latestTag: string
  latestVersion: string
}

interface CatalogData {
  generated: string
  registry: string
  packages: PackageSummary[]
}

const data = ref<CatalogData | null>(null)
const loading = ref(true)
const error = ref('')
const searchQuery = ref('')

const filteredPackages = computed(() => {
  if (!data.value) return []
  const query = searchQuery.value.toLowerCase()
  if (!query) return data.value.packages

  return data.value.packages.filter(pkg =>
    pkg.name.toLowerCase().includes(query)
    || pkg.title.toLowerCase().includes(query)
    || pkg.description.toLowerCase().includes(query)
    || pkg.keywords.some(k => k.toLowerCase().includes(query)),
  )
})

function uniqueOsLabels(platforms: string[]): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const p of platforms) {
    const os = p.split('/')[0]
    if (!seen.has(os)) {
      seen.add(os)
      result.push(os)
    }
  }
  return result
}

const router = useRouter()

function navigateToPackage(name: string) {
  router.go(`/docs/catalog/${name}`)
}

onMounted(async () => {
  try {
    const resp = await fetch('/data/catalog/catalog.json')
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
    data.value = await resp.json()
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Failed to load catalog'
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="catalog">
    <!-- Loading -->
    <div v-if="loading" class="loading">
      <div class="spinner" />
      <span>Loading package catalog…</span>
    </div>

    <!-- Error -->
    <div v-else-if="error" class="error">
      Failed to load catalog: {{ error }}
    </div>

    <!-- Content -->
    <template v-else-if="data">
      <!-- Controls -->
      <div class="controls">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search packages…"
          class="search"
        >
        <span class="results-count">
          {{ filteredPackages.length }} package{{ filteredPackages.length !== 1 ? 's' : '' }}
        </span>
      </div>

      <!-- Grid -->
      <div class="grid">
        <div
          v-for="pkg in filteredPackages"
          :key="pkg.name"
          class="card"
          @click="navigateToPackage(pkg.name)"
        >
          <div class="card-header">
            <img
              v-if="pkg.hasLogo"
              :src="`/data/catalog/packages/${pkg.name}/logo.${pkg.logoExt}`"
              :alt="`${pkg.title} logo`"
              class="card-logo"
            >
            <div v-else class="card-logo-placeholder">
              <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
                <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
                <polyline points="3.27 6.96 12 12.01 20.73 6.96" />
                <line x1="12" y1="22.08" x2="12" y2="12" />
              </svg>
            </div>
            <div class="card-title-group">
              <h3 class="card-title">{{ pkg.title }}</h3>
              <span v-if="pkg.latestVersion" class="card-version">{{ pkg.latestVersion }}</span>
            </div>
          </div>

          <p v-if="pkg.description" class="card-desc">
            {{ pkg.description }}
          </p>

          <div class="card-meta">
            <span class="card-platforms">
              <span
                v-for="os in uniqueOsLabels(pkg.platforms)"
                :key="os"
                class="platform-badge"
              >{{ os }}</span>
            </span>
            <span class="card-tags">{{ pkg.tagCount }} version{{ pkg.tagCount !== 1 ? 's' : '' }}</span>
          </div>

          <div class="card-install" @click.stop>
            <CopySnippet label="$" :code="`ocx --remote shell profile add ${pkg.registry || data?.registry || ''}/${pkg.name}`" />
          </div>
        </div>
      </div>

      <!-- Empty state -->
      <div v-if="filteredPackages.length === 0" class="empty">
        {{ searchQuery ? 'No packages match your search.' : 'No packages available.' }}
      </div>
    </template>
  </div>
</template>

<style scoped>
.catalog {
  margin: 1rem 0;
}

/* Controls */
.controls {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  margin-bottom: 1.25rem;
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

.results-count {
  font-size: 0.8rem;
  color: var(--vp-c-text-3);
  white-space: nowrap;
}

/* Grid */
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 1rem;
}

/* Card */
.card {
  display: flex;
  flex-direction: column;
  padding: 1.25rem;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  text-decoration: none;
  color: inherit;
  cursor: pointer;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.card:hover:not(:has(.card-install:hover)) {
  border-color: var(--vp-c-brand);
  box-shadow: 0 2px 12px rgba(0, 0, 0, 0.06);
}

.card-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  margin-bottom: 0.5rem;
}

.card-logo {
  width: 36px;
  height: 36px;
  object-fit: contain;
  flex-shrink: 0;
}

.card-logo-placeholder {
  width: 36px;
  height: 36px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  color: var(--vp-c-text-3);
}

.card-title-group {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
  min-width: 0;
}

.card-title {
  font-size: 1rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
  margin: 0;
  border: none;
  padding: 0;
  line-height: 1.4;
}

.card-version {
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  color: var(--vp-c-text-3);
  white-space: nowrap;
}

.card-desc {
  font-size: 0.85rem;
  color: var(--vp-c-text-2);
  line-height: 1.5;
  margin: 0 0 0.75rem;
  flex: 1;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.card-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 0.75rem;
}

.card-platforms {
  display: flex;
  gap: 0.25rem;
}

.platform-badge {
  font-family: var(--vp-font-family-mono);
  font-size: 0.7rem;
  padding: 0.1rem 0.4rem;
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-divider);
  border-radius: 3px;
  color: var(--vp-c-text-3);
}

.card-tags {
  font-size: 0.75rem;
  color: var(--vp-c-text-3);
}

.card-install {
  border-top: 1px solid var(--vp-c-divider);
  padding-top: 0.75rem;
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
  .grid {
    grid-template-columns: 1fr;
  }

  .controls {
    flex-direction: column;
    align-items: stretch;
  }

  .results-count {
    text-align: right;
  }
}
</style>
