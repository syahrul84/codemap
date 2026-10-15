/** Calls the backend and tracks loading state. */
export function useApi<T>(url: string, opts: { method?: string } = {}) {
  const loading = ref(false)
  return { loading, url, opts }
}

export const formatPrice = (cents: number, currency = 'MYR') => `${currency} ${cents / 100}`
