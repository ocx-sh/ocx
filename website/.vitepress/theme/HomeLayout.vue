<script setup lang="ts">
import DefaultTheme from 'vitepress/theme'

const { Layout } = DefaultTheme
</script>

<template>
  <Layout>
    <template #home-features-before>
      <div class="early-dev-banner-wrapper">
        <div class="early-dev-banner">
          <strong>Early Development</strong> — OCX is in early development. The CLI is functional but APIs may change.
        </div>
      </div>
    </template>
  </Layout>
</template>

<style scoped>
.early-dev-banner-wrapper {
  padding: 0 24px;
}

@media (min-width: 640px) {
  .early-dev-banner-wrapper {
    padding: 0 48px;
  }
}

@media (min-width: 960px) {
  .early-dev-banner-wrapper {
    padding: 0 64px;
  }
}

.early-dev-banner {
  max-width: 1152px;
  margin: 0 auto 24px;
  padding: 8px 24px;
  border-radius: 8px;
  border: 1px solid var(--vp-c-divider);
  color: var(--vp-c-text-2);
  font-size: 13px;
  text-align: center;
}
</style>
