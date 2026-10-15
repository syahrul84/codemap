<script setup lang="ts">
const config = useRuntimeConfig()
const { data: malls } = await useFetch(`${config.public.apiBase}/malls`)
</script>

<template>
  <div v-for="m in malls.data" :key="m.id">
    <NuxtLink :to="`/malls/${m.slug}`">{{ m.name }}</NuxtLink>
  </div>
</template>
