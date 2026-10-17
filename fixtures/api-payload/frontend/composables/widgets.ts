export function archiveWidget(id: number, payload: object) {
  return api.post(`/widgets/${id}/archive`, payload)
}

export function retireWidget(id: number, record: object, reason: string) {
  return api.post(`/widgets/${id}/retire`, { ...record, reason })
}
