// A bundle of endpoint helpers: callers pick functions, not response fields.
export function useCartApi() {
  const url = (merchant: number, suffix = '') => `/carts/${merchant}/items${suffix}`
  return {
    add: (merchant: number, body: object) => useApi()(url(merchant), { method: 'POST', body }),
    list: () => useApi()('/carts'),
    setQuantity: (merchant: number, item: number, quantity: number) =>
      useApi()(url(merchant, `/${item}`), { method: 'PATCH', body: { quantity } }),
    remove: (merchant: number, item: number) =>
      useApi()(`${url(merchant)}/${item}`, { method: 'DELETE' }),
  }
}
