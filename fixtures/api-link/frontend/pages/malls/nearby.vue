<script setup lang="ts">
const res = await useApi()('/nearby-malls')
</script>

<template>
  <ul>
    <li v-for="m in res.data" :key="m.id">{{ m.name }} · {{ m.city }} · {{ m.is_saved }}</li>
  </ul>
</template>
