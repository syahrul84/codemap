<script setup lang="ts">
const { add, list } = useCartApi()
</script>

<template>
  <button @click="add(1, {}).then(list)">Add</button>
</template>
