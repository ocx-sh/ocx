<script setup lang="ts">
/**
 * Table-like container for <RoadmapFeature> rows inside a RoadmapItem card.
 */
</script>

<template>
  <div class="roadmap-features">
    <slot />
  </div>
</template>

<style scoped>
.roadmap-features {
  display: flex;
  flex-direction: column;
  border-radius: 8px;
  overflow: hidden;
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 60%, transparent);
}
</style>
