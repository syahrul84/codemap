<script setup lang="ts">
import { useAssetHistory } from './logic/assetHistory'

const { load } = useAssetHistory(1)
load()
</script>

<template>
  <div />
</template>
