<script setup lang="ts">
import { computed } from 'vue'
import { useData } from 'vitepress'

const { theme } = useData()

const isDev = computed(
  () => (theme.value as { deployTarget?: string }).deployTarget !== 'prod',
)
</script>

<template>
  <div v-if="isDev" class="dev-banner" role="status">
    <span class="dev-banner__badge">Development</span>
    <span class="dev-banner__text">
      Preview build — APIs and content may change. Visit
      <a class="dev-banner__link" href="https://ocx.sh">ocx.sh</a>
      for the current release.
    </span>
  </div>
</template>

<style scoped>
.dev-banner {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  z-index: 60;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  height: 40px;
  padding: 0 16px;
  background: color-mix(in srgb, var(--vp-c-warning-1) 14%, var(--vp-c-bg));
  color: var(--vp-c-text-1);
  border-bottom: 1px solid var(--vp-c-warning-3);
  font-size: 13px;
  line-height: 1.3;
  text-align: center;
}

.dev-banner__text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.dev-banner__badge {
  background: var(--vp-c-warning-1);
  color: var(--vp-c-bg);
  padding: 2px 8px;
  border-radius: 4px;
  font-weight: 700;
  font-size: 11px;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  white-space: nowrap;
}

.dev-banner__link {
  color: var(--vp-c-brand-1);
  text-decoration: underline;
  font-weight: 600;
}

.dev-banner__link:hover {
  color: var(--vp-c-brand-2);
  text-decoration: none;
}

@media (max-width: 768px) {
  .dev-banner {
    height: 56px;
    padding: 6px 12px;
    flex-wrap: wrap;
    gap: 6px;
  }

  .dev-banner__text {
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
    flex-basis: 100%;
    font-size: 12px;
  }
}
</style>
