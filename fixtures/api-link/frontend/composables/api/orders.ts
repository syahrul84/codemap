// Left over from the old checkout: nothing calls it, and its endpoint is gone.
export function reserveListing(id: number) {
  return useApi()(`/listings/${id}/reserve`, { method: 'POST' })
}
