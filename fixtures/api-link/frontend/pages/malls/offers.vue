<script setup lang="ts">
import { recordsApi } from '~/composables/api/records'

const endpoint = '/malls'
const route = useRoute()
const offers = await recordsApi.get(endpoint, route.params.slug, '/offers')
</script>

<template>
  <p v-for="o in offers" :key="o.id">{{ o.price }}</p>
</template>
