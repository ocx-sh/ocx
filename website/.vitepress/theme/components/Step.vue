<script setup lang="ts">
/** Declarative marker for <Steps>. Never rendered to DOM — Steps reads its VNodes directly.
 *  Slot content is used as the collapsible detail panel body (markdown is rendered by VitePress). */
defineProps<{
  title: string
  description?: string
  status?: 'complete' | 'current' | 'upcoming'
}>()
</script>

<template><slot /></template>
