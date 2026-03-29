<script setup lang="ts">
/**
 * Renders markdown description content inside a RoadmapItem card.
 * Wrap your markdown in this tag with blank lines so VitePress processes it.
 */
</script>

<template>
  <div class="roadmap-desc">
    <slot />
  </div>
</template>

<style scoped>
.roadmap-desc {
  font-size: 14px;
  color: var(--vp-c-text-2);
  line-height: 1.65;
  margin: 0 0 16px;
}

.roadmap-desc :deep(p) {
  margin: 0 0 8px;
}

.roadmap-desc :deep(p:last-child) {
  margin-bottom: 0;
}

.roadmap-desc :deep(code) {
  font-size: 13px;
  background: var(--vp-c-bg-mute);
  padding: 2px 6px;
  border-radius: 4px;
}

.roadmap-desc :deep(a) {
  color: var(--vp-c-brand-1);
  text-decoration: underline;
  text-underline-offset: 2px;
}
</style>
