<script setup lang="ts">
import type { Mall } from '~/composables/api/malls'
import { fetchMalls } from '~/composables/api/malls'

const malls = ref<Mall[]>([])
const total = ref(0)
const page = ref(1)

async function load() {
  const res = await fetchMalls(page.value)
  malls.value = [...malls.value, ...res.malls]
  total.value = res.total
}
</script>

<template>
  <section>
    <p>{{ total }}</p>
    <MallCard v-for="m in malls" :key="m.id" :mall="m" />
    <button @click="load">More</button>
  </section>
</template>
