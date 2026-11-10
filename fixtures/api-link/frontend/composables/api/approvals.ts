// Each module passes its own endpoint: the URL starts with a variable.
export function approvalSummary(endpoint: string, group: string, id: number) {
  return useApi()(`${endpoint}/${group}/${id}/summary`)
}

export function legacySync() {
  // codemap-ignore: served by the old gateway, not this backend
  return useApi()('/legacy/sync', { method: 'POST' })
}
