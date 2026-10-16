<script setup lang="ts">
import type { Mall } from '~/composables/api/malls'

const props = defineProps<{ mall: Mall }>()
const link = computed(() => `/malls/${props.mall.slug}`)
</script>

<template>
  <NuxtLink :to="link" data-testid="mall-card">
    <h3>{{ mall.name }}</h3>
    <p v-if="mall.city">{{ mall.city }}</p>
  </NuxtLink>
</template>
