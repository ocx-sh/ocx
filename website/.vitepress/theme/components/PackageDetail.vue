<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute } from 'vitepress'
import { useClipboard } from '@vueuse/core'
import CopySnippet from './CopySnippet.vue'

interface PackageInfo {
  name: string
  registry: string
  repository: string
  title: string
  description: string
  keywords: string[]
  hasLogo: boolean
  logoExt: string
  hasReadme: boolean
  latestTag: string
  tags: string[]
  platforms: string[]
}

const route = useRoute()
const pkgName = computed(() => {
  const segments = route.path.split('/')
  return segments[segments.length - 1].replace(/\.html$/, '')
})

const info = ref<PackageInfo | null>(null)
const loading = ref(true)
const error = ref('')

const latestTag = computed(() => info.value?.latestTag ?? '')

const qualifiedName = computed(() => {
  if (!info.value) return ''
  const registry = info.value.registry || ''
  return registry ? `${registry}/${info.value.name}` : info.value.name
})

const installCmd = computed(() => {
  if (!info.value) return ''
  const tag = latestTag.value ? `:${latestTag.value}` : ''
  return `ocx --remote install ${qualifiedName.value}${tag}`
})

const profileCmd = computed(() => {
  if (!info.value) return ''
  const tag = latestTag.value ? `:${latestTag.value}` : ''
  return `ocx --remote shell profile add ${qualifiedName.value}${tag}`
})

function platformOs(platform: string): string {
  return platform.split('/')[0]
}

const { copy } = useClipboard()
const copiedTag = ref('')

async function copyInstallForTag(event: MouseEvent, tag: string) {
  if (!info.value) return
  const cmd = event.shiftKey
    ? `ocx --remote shell profile add ${qualifiedName.value}:${tag}`
    : `ocx --remote install ${qualifiedName.value}:${tag}`
  await copy(cmd)
  copiedTag.value = tag
  setTimeout(() => { copiedTag.value = '' }, 1500)
}

onMounted(async () => {
  try {
    const resp = await fetch(`/data/catalog/packages/${pkgName.value}/info.json`)
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`)
    info.value = await resp.json()

  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Failed to load package info'
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="pkg-detail">
    <!-- Loading -->
    <div v-if="loading" class="loading">
      <div class="spinner" />
      <span>Loading package info…</span>
    </div>

    <!-- Error -->
    <div v-else-if="error" class="error">
      Failed to load package info: {{ error }}
    </div>

    <!-- Content -->
    <template v-else-if="info">
      <!-- Back link -->
      <a href="/docs/catalog" class="back-link">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M19 12H5" />
          <polyline points="12 19 5 12 12 5" />
        </svg>
        All packages
      </a>

      <!-- Header -->
      <div class="header">
        <img
          v-if="info.hasLogo"
          :src="`/data/catalog/packages/${info.name}/logo.${info.logoExt}`"
          :alt="`${info.title} logo`"
          class="header-logo"
        >
        <div v-else class="header-logo-placeholder">
          <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
            <polyline points="3.27 6.96 12 12.01 20.73 6.96" />
            <line x1="12" y1="22.08" x2="12" y2="12" />
          </svg>
        </div>
        <div class="header-text">
          <h1 class="header-title">{{ info.title }}</h1>
          <p v-if="info.description" class="header-desc">
            {{ info.description }}
          </p>
          <div class="header-meta">
            <div v-if="info.keywords.length" class="meta-group">
              <span class="meta-label">Keywords</span>
              <div class="meta-badges">
                <span v-for="kw in info.keywords" :key="kw" class="keyword">{{ kw }}</span>
              </div>
            </div>
            <div v-if="info.platforms.length" class="meta-group">
              <span class="meta-label">Supported Platforms</span>
              <div class="meta-badges">
                <span
                  v-for="platform in info.platforms"
                  :key="platform"
                  class="platform-badge"
                >{{ platform }}</span>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- Install Snippets -->
      <div class="snippets">
        <h3 class="snippets-title">Install</h3>
        <div class="snippet-list">
          <div class="snippet-row">
            <span class="snippet-label">Install</span>
            <CopySnippet label="$" :code="installCmd" />
          </div>
          <div class="snippet-row">
            <span class="snippet-label">Profile</span>
            <CopySnippet label="$" :code="profileCmd" />
          </div>
        </div>
      </div>

      <!-- Versions -->
      <div class="versions-section">
        <div class="versions-header">
          <h3 class="versions-title">Versions ({{ info.tags.length }})</h3>
          <span v-if="info.tags.length" class="versions-hint">Click to copy install command. Hold Shift to copy profile add command.</span>
        </div>
        <div v-if="info.tags.length" class="tag-grid">
          <code
            v-for="tag in info.tags"
            :key="tag"
            class="tag-badge"
            :class="{ copied: copiedTag === tag }"
            :title="`Click: ocx --remote install ${qualifiedName}:${tag} · Shift: ocx --remote shell profile add ${qualifiedName}:${tag}`"
            @click="copyInstallForTag($event, tag)"
          ><span class="tag-text">{{ tag }}</span><svg class="tag-check" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12" /></svg></code>
        </div>
        <div v-else class="empty">
          No versions available.
        </div>
      </div>

    </template>
  </div>
</template>

<style scoped>
.pkg-detail {
  margin: 1rem 0;
}

/* Back link */
.back-link {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  font-size: 0.85rem;
  font-weight: 500;
  color: var(--vp-c-brand);
  text-decoration: none;
  margin-bottom: 1rem;
  transition: color 0.15s;
}

.back-link:hover {
  color: var(--vp-c-brand-dark);
}

/* Header */
.header {
  display: flex;
  align-items: flex-start;
  gap: 1.25rem;
  margin-bottom: 1.5rem;
  padding-bottom: 1.5rem;
  border-bottom: 1px solid var(--vp-c-divider);
}

.header-logo {
  width: 64px;
  height: 64px;
  object-fit: contain;
  flex-shrink: 0;
}

.header-logo-placeholder {
  width: 64px;
  height: 64px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  color: var(--vp-c-text-3);
}

.header-text {
  min-width: 0;
}

.header-title {
  font-size: 1.75rem;
  font-weight: 700;
  margin: 0 0 0.35rem;
  border: none;
  padding: 0;
  line-height: 1.3;
}

.header-desc {
  font-size: 0.95rem;
  color: var(--vp-c-text-2);
  margin: 0 0 0.5rem;
  line-height: 1.5;
}

.header-meta {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.meta-group {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
}

.meta-label {
  font-size: 0.7rem;
  font-weight: 600;
  color: var(--vp-c-text-3);
  text-transform: uppercase;
  letter-spacing: 0.03em;
}

.meta-badges {
  display: flex;
  gap: 0.35rem;
  flex-wrap: wrap;
}

.keyword {
  font-size: 0.75rem;
  padding: 0.15rem 0.5rem;
  background: var(--vp-c-brand-soft);
  border-radius: 4px;
  color: var(--vp-c-brand-dark);
}

.platform-badge {
  display: inline-flex;
  align-items: center;
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  padding: 0.15rem 0.5rem;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 4px;
  color: var(--vp-c-text-2);
}

/* Install Snippets */
.snippets {
  margin-bottom: 1.5rem;
  padding: 1rem 1.25rem;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
}

.snippets-title {
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--vp-c-text-2);
  text-transform: uppercase;
  letter-spacing: 0.02em;
  margin: 0 0 0.75rem;
  border: none;
  padding: 0;
}

.snippet-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.snippet-row {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.snippet-label {
  font-size: 0.8rem;
  color: var(--vp-c-text-3);
  min-width: 100px;
  flex-shrink: 0;
}

/* Versions */
.versions-section {
  margin-bottom: 1.5rem;
}

.versions-title {
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--vp-c-text-2);
  text-transform: uppercase;
  letter-spacing: 0.02em;
  margin: 0;
  border: none;
  padding: 0;
}

.versions-header {
  margin-bottom: 0.75rem;
}

.versions-hint {
  display: block;
  font-size: 0.7rem;
  color: var(--vp-c-text-3);
  margin-top: -0.4rem;
}

.tag-grid {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
}

.tag-badge {
  position: relative;
  font-size: 0.8rem;
  font-weight: 500;
  padding: 0.2rem 0.6rem;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 4px;
  color: var(--vp-c-text-2);
  cursor: pointer;
  transition: border-color 0.3s, color 0.3s, background 0.3s;
  user-select: none;
}

.tag-text {
  transition: opacity 0.15s ease-in;
}

.tag-check {
  position: absolute;
  inset: 0;
  margin: auto;
  opacity: 0;
  transition: opacity 0.15s ease-in;
}

.tag-badge:hover {
  border-color: var(--vp-c-brand);
  color: var(--vp-c-brand);
}

.tag-badge.copied {
  border-color: var(--vp-c-green-2);
  color: var(--vp-c-green-2);
  background: var(--vp-c-green-soft);
}

.tag-badge.copied .tag-text {
  opacity: 0;
  transition: opacity 0.1s ease-out;
}

.tag-badge.copied .tag-check {
  opacity: 1;
  transition: opacity 0.1s ease-out;
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
  .header {
    flex-direction: column;
    align-items: center;
    text-align: center;
  }

  .meta-badges {
    justify-content: center;
  }

  .snippet-row {
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
  }

  .snippet-label {
    min-width: 0;
  }
}
</style>
