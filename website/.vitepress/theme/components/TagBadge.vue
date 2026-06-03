<script setup lang="ts">
import { ref } from 'vue'
import { useClipboard } from '@vueuse/core'
import {
  ContextMenuRoot,
  ContextMenuTrigger,
  ContextMenuContent,
  ContextMenuItem,
} from 'reka-ui'

const props = withDefaults(defineProps<{
  tag: string
  qualifiedName: string
  variant?: 'default' | 'rolling' | 'minor' | 'child'
}>(), {
  variant: 'default',
})

const emit = defineEmits<{ copied: [] }>()

const { copy } = useClipboard()
const copied = ref(false)

function addProjectCmd() {
  return `ocx add ${props.qualifiedName}:${props.tag}`
}

function addGlobalCmd() {
  return `ocx --global add ${props.qualifiedName}:${props.tag}`
}

function inspectCmd() {
  return `ocx package inspect ${props.qualifiedName}:${props.tag}`
}

async function copyText(text: string) {
  if (copied.value) return
  await copy(text)
  copied.value = true
  setTimeout(() => emit('copied'), 1300)  // start fade-out 200ms before checkmark ends
  setTimeout(() => { copied.value = false }, 1500)
}

function identifier() {
  return `${props.qualifiedName}:${props.tag}`
}

async function handleClick() {
  await copyText(identifier())
}
</script>

<template>
  <ContextMenuRoot :modal="false">
    <ContextMenuTrigger as-child>
      <code
        class="tag-badge"
        :class="[variant, { copied }]"
        :title="`Click to copy identifier`"
        @click="handleClick"
      >
        <span class="tag-text">{{ tag }}</span>
        <svg
          class="tag-check"
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="3"
          stroke-linecap="round"
          stroke-linejoin="round"
        ><polyline points="20 6 9 17 4 12" /></svg>
      </code>
    </ContextMenuTrigger>

    <ContextMenuContent class="ctx-menu">
        <ContextMenuItem class="ctx-item" @select="copyText(identifier())">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
          </svg>
          <span>Copy identifier</span>
        </ContextMenuItem>
        <ContextMenuItem class="ctx-item" @select="copyText(tag)">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M20.59 13.41l-7.17 7.17a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z" />
            <line x1="7" y1="7" x2="7.01" y2="7" />
          </svg>
          <span>Copy tag</span>
        </ContextMenuItem>
        <ContextMenuItem class="ctx-item" @select="copyText(addProjectCmd())">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <polyline points="7 10 12 15 17 10" />
            <line x1="12" y1="15" x2="12" y2="3" />
          </svg>
          <span>Add to project</span>
        </ContextMenuItem>
        <ContextMenuItem class="ctx-item" @select="copyText(addGlobalCmd())">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="10" />
            <line x1="2" y1="12" x2="22" y2="12" />
            <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
          </svg>
          <span>Add globally</span>
        </ContextMenuItem>
        <ContextMenuItem class="ctx-item" @select="copyText(inspectCmd())">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
          <span>Inspect command</span>
        </ContextMenuItem>
    </ContextMenuContent>
  </ContextMenuRoot>
</template>

<style scoped>
.tag-badge {
  position: relative;
  display: inline-flex;
  align-items: center;
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

.tag-badge.rolling {
  font-weight: 600;
}

.tag-badge.child {
  font-size: 0.75rem;
  color: var(--vp-c-text-3);
}

.tag-badge:hover {
  border-color: var(--vp-c-brand);
  color: var(--vp-c-brand);
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
</style>

<style>
/* Context menu — unscoped so portal renders correctly */
.ctx-menu {
  min-width: 200px;
  padding: 0.35rem;
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  box-shadow: var(--vp-shadow-3);
  z-index: 100;
  animation: ctx-fade-in 0.12s ease-out;
}

.ctx-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.45rem 0.6rem;
  border-radius: 4px;
  font-size: 0.8rem;
  color: var(--vp-c-text-2);
  cursor: pointer;
  outline: none;
  transition: background 0.1s, color 0.1s;
}

.ctx-item:hover,
.ctx-item[data-highlighted] {
  background: var(--vp-c-brand-soft);
  color: var(--vp-c-brand-dark);
}

@keyframes ctx-fade-in {
  from { opacity: 0; transform: scale(0.96); }
  to { opacity: 1; transform: scale(1); }
}
</style>
