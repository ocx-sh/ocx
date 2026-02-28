<script setup lang="ts">
import { computed, useSlots } from 'vue'
import type { FileNode } from './FileTreeNode.vue'
import DescriptionComp from './Description.vue'

const slots = useSlots()

/** Extract plain text from a <Description> VNode's default slot. */
function descText(vnode: any): string | undefined {
  const slot = vnode?.children?.default
  if (typeof slot !== 'function') return undefined
  const text = slot()
    .map((v: any) => (typeof v.children === 'string' ? v.children : ''))
    .join('')
    .trim()
  return text || undefined
}

/** Recursively convert slot VNodes (from <Node> markers) into FileNode objects. */
function vnodesToNodes(vnodes: any[]): FileNode[] {
  const result: FileNode[] = []
  for (const v of vnodes) {
    if (!v?.props?.name) continue   // skip Description and text VNodes
    const p = v.props
    const childVnodes =
      typeof v.children?.default === 'function' ? v.children.default() : []

    // Pull description from a <Description> child element; fall back to attribute
    const descVnode = childVnodes.find((cv: any) => cv?.type === DescriptionComp)
    const description = descVnode ? descText(descVnode) : (p.description ?? undefined)

    const children = vnodesToNodes(childVnodes)
    result.push({
      name: p.name,
      ...(p.icon != null && { icon: p.icon }),
      ...(p['open-icon'] != null && { openIcon: p['open-icon'] }),
      ...(p.openIcon != null && { openIcon: p.openIcon }),
      ...(description != null && { description }),
      ...('open' in p ? { open: p.open !== false && p.open !== 'false' } : {}),
      ...(children.length > 0 ? { children } : {}),
    })
  }
  return result
}

const data = computed(() => vnodesToNodes(slots.default?.() ?? []))
</script>

<template>
  <FileTree :data="data" />
</template>
