<script setup lang="ts">
import { ref, computed, inject, type Ref } from 'vue'

export interface FileNode {
  /** File or directory name (append `/` to visually mark as directory). */
  name: string
  /** Short annotation shown to the right in muted text. */
  description?: string
  /** Child nodes — presence implies this is a directory. */
  children?: FileNode[]
  /** Whether to render the node expanded on first load (default: true). */
  open?: boolean
  /**
   * Icon shown when the node is collapsed (or always for files).
   * When provided, replaces the ▾/▸ expand toggle.
   */
  icon?: string
  /**
   * Icon shown when this directory is expanded.
   * Falls back to `icon` when not provided.
   */
  openIcon?: string
}

const props = withDefaults(defineProps<{
  node: FileNode
  depth?: number
}>(), {
  depth: 0,
})

const open = ref(props.node.open !== false)
const isDir = computed(() => Array.isArray(props.node.children))
const hasIcon = computed(() => props.node.icon != null)
const displayIcon = computed(() => {
  if (!isDir.value || !open.value) return props.node.icon
  return props.node.openIcon ?? props.node.icon
})

// Selection state shared via provide/inject from parent FileTree
const selectedNode = inject<Ref<FileNode | null>>('ft-selected', ref(null))
const selectNode = inject<(node: FileNode) => void>('ft-select', () => {})
const isSelected = computed(() => selectedNode.value === props.node)

function handleClick() {
  selectNode(props.node)
  if (isDir.value) open.value = !open.value
}
</script>

<template>
  <li class="ft-node">
    <div
      class="ft-row"
      :class="{
        'ft-row--dir': isDir,
        'ft-row--selected': isSelected,
      }"
      @click="handleClick"
    >
      <!-- expand/collapse arrow only when no custom icon is defined -->
      <span v-if="!hasIcon" class="ft-toggle" aria-hidden="true">
        {{ isDir ? (open ? '▾' : '▸') : '' }}
      </span>
      <!-- custom icon replaces the toggle arrow -->
      <span v-if="hasIcon" class="ft-custom-icon" aria-hidden="true">{{ displayIcon }}</span>
      <code class="ft-name" :class="{ 'ft-dir': isDir }">{{ node.name }}</code>
      <span v-if="node.description" class="ft-desc">{{ node.description }}</span>
    </div>
    <ul v-if="isDir && open" class="ft-children">
      <FileTreeNode
        v-for="child in node.children"
        :key="child.name"
        :node="child"
        :depth="depth + 1"
      />
    </ul>
  </li>
</template>

<style scoped>
.ft-node {
  list-style: none;
  padding: 0;
  margin: 0;
}

.ft-row {
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 3px 8px;
  border-radius: 5px;
  line-height: 1.6;
  cursor: pointer;
  transition: background 0.12s;
  user-select: none;
}

.ft-row:hover {
  background-color: var(--vp-c-bg-mute);
}

.ft-row--selected {
  background-color: var(--vp-c-brand-soft);
}

.ft-row--selected:hover {
  background-color: var(--vp-c-brand-soft);
}

.ft-toggle {
  font-size: 10px;
  width: 14px;
  flex-shrink: 0;
  color: var(--vp-c-text-3);
}

.ft-custom-icon {
  font-size: 13px;
  width: 18px;
  text-align: center;
  flex-shrink: 0;
  line-height: 1;
}

.ft-name {
  font-size: 13px;
  font-family: var(--vp-font-family-mono);
  color: var(--vp-c-text-1);
  background: transparent;
  padding: 0;
  border-radius: 0;
}

.ft-dir {
  color: var(--vp-c-brand);
}

.ft-desc {
  font-size: 12px;
  color: var(--vp-c-text-3);
  margin-left: 4px;
  font-style: italic;
}

.ft-children {
  list-style: none;
  padding: 0;
  margin: 0;
  padding-left: 18px;
  border-left: 1px solid var(--vp-c-divider);
  margin-left: 10px;
}
</style>
