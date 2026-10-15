<script setup lang="ts">
const route = useRoute()
const mall = await useApi()(`/malls/${route.params.slug}`)
const history = await useApi()('/orders/history')
async function create() {
  await useApi()('/malls', { method: 'POST', body: {} })
}
</script>

<template>
  <h1>{{ mall.data.title }}</h1>
  <p v-for="o in mall.data.offers" :key="o.id">{{ o.price }}</p>
</template>
