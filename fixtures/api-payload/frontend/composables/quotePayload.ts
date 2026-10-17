export function quotePayload(record: { status: string }) {
  return { status: record.status, _method: 'PATCH' }
}
