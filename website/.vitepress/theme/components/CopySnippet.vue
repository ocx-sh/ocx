<script setup lang="ts">
import { ref } from 'vue'
import { useClipboard } from '@vueuse/core'

const props = defineProps<{
  code: string
  label?: string
}>()

const { copy } = useClipboard()
const copied = ref(false)
let timeout: ReturnType<typeof setTimeout> | null = null

async function handleCopy() {
  await copy(props.code)
  copied.value = true
  if (timeout) clearTimeout(timeout)
  timeout = setTimeout(() => {
    copied.value = false
  }, 1500)
}
</script>

<template>
  <span class="copy-snippet" @click="handleCopy">
    <span v-if="label" class="snippet-label">{{ label }}</span>
    <code class="snippet-code">{{ code }}</code>
    <button class="snippet-btn" :title="copied ? 'Copied!' : 'Copy to clipboard'">
      <svg v-if="!copied" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
      </svg>
      <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <polyline points="20 6 9 17 4 12" />
      </svg>
    </button>
  </span>
</template>

<style scoped>
.copy-snippet {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  padding: 0.3rem 0.5rem;
  background: var(--vp-c-bg-soft);
  border: 1px solid var(--vp-c-divider);
  border-radius: 6px;
  cursor: pointer;
  transition: border-color 0.2s;
  line-height: 1.4;
}

.copy-snippet:hover {
  border-color: var(--vp-c-brand);
}

.snippet-label {
  color: var(--vp-c-text-3);
  font-size: 0.8rem;
  user-select: none;
}

.snippet-code {
  font-family: var(--vp-font-family-mono);
  font-size: 0.8rem;
  color: var(--vp-c-text-1);
  background: none;
  border: none;
  padding: 0;
}

.snippet-btn {
  display: inline-flex;
  align-items: center;
  background: none;
  border: none;
  color: var(--vp-c-text-3);
  cursor: pointer;
  padding: 0;
  margin-left: 0.15rem;
  transition: color 0.15s;
}

.snippet-btn:hover {
  color: var(--vp-c-brand);
}
</style>
