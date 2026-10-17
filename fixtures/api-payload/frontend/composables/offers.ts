// Calls whose payload is checked against what the backend action accepts.
export function createOffer(price: number, text: string) {
  return $fetch('/offers', { method: 'POST', body: { price, desc: text } })
}
export function renameOffer(id: number, title: string) {
  return $fetch(`/offers/${id}`, { method: 'PATCH', body: { title, note: '' } })
}
export function editOffer(id: number, form: Record<string, unknown>) {
  return $fetch(`/offers/${id}`, { method: 'PATCH', body: { ...form } })
}
export function addNote(body: string) {
  return $fetch('/notes', { method: 'POST', body: { body, pinned: true, tags: [], color: 'red', project_id: 1 } })
}
export function runImport(file: File) {
  const form = new FormData()
  form.append('file', file)
  form.append('anything', 'x')
  return $fetch('/imports', { method: 'POST', body: form })
}
export function listNotes(q: string) {
  return $fetch('/notes', { query: { search: q, filter: 'all', page: 2 } })
}
