// Helper used by the mall list: reshapes the paginated response.
export interface Mall {
  id: number
  slug: string
  name: string
  city: string | null
}

export function fetchMalls(page = 1): Promise<{ malls: Mall[], total: number }> {
  return useApi()<{ data: Mall[], meta: { total: number } }>('/malls', { query: { page } })
    .then(r => ({
      malls: r.data,
      total: r.meta?.total ?? 0,
    }))
}
