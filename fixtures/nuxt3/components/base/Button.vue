<script setup lang="ts">
defineProps<{ label: string; disabled?: boolean }>()
defineEmits(['click'])
</script>

<template>
  <UButton :disabled="disabled" @click="$emit('click')">{{ label }}</UButton>
</template>
