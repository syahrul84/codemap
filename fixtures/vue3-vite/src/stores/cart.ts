import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

export const useCartStore = defineStore('cart', () => {
  const items = ref<number[]>([])
  const count = computed(() => items.value.length)
  function add(id: number) {
    items.value.push(id)
  }
  const clear = () => {
    items.value = []
  }
  return { items, count, add, clear }
})
