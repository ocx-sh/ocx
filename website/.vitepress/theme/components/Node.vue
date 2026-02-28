<script setup lang="ts">
/** Declarative marker for <Tree>. Never rendered to DOM — Tree reads its VNodes directly. */
defineProps<{
  name: string
  icon?: string
  /** Icon shown when this directory is expanded. Falls back to icon. */
  openIcon?: string
  description?: string
  /** Expanded on first render (default true for directories). */
  open?: boolean
}>()
</script>

<template><slot /></template>
