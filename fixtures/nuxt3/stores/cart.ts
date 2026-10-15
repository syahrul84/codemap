export const useCartStore = defineStore('cart', {
  state: () => ({ items: [] as number[], coupon: null as string | null }),
  getters: { count: (s) => s.items.length },
  actions: {
    add(id: number) { this.items.push(id) },
  },
})
