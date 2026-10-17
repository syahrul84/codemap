export function archivePayload(record: object) {
  return { ...record, _method: 'PUT' }
}
