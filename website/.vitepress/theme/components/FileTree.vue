<script setup lang="ts">
import { ref, provide, toRef } from 'vue'
import type { FileNode } from './FileTreeNode.vue'

const props = withDefaults(defineProps<{
  /** Root-level nodes of the tree. */
  data: FileNode[]
  /**
   * Allow expand/collapse of directory nodes. Set `false` for static reference
   * trees that should always render fully expanded.
   */
  collapsible?: boolean
}>(), {
  collapsible: true,
})

const selectedNode = ref<FileNode | null>(null)
provide('ft-selected', selectedNode)
provide('ft-select', (node: FileNode) => {
  selectedNode.value = selectedNode.value === node ? null : node
})
provide('ft-collapsible', toRef(props, 'collapsible'))
</script>

<template>
  <div class="ft-container">
    <ul class="ft-root">
      <FileTreeNode
        v-for="node in data"
        :key="node.name"
        :node="node"
        :depth="0"
      />
    </ul>
  </div>
</template>

<style scoped>
.ft-container {
  border: 1px solid var(--vp-c-border);
  border-radius: 8px;
  background-color: var(--vp-c-bg-soft);
  padding: 12px 16px;
  margin: 16px 0;
  overflow-x: auto;
}

.ft-root {
  list-style: none;
  padding: 0;
  margin: 0;
}
</style>
