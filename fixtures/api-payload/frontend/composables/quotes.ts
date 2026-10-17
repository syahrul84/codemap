import { quotePayload } from './quotePayload'

export async function saveQuote(id: number, record: { status: string }) {
  const form = new FormData()
  Object.entries(quotePayload(record)).forEach(([key, value]) => form.append(key, value))
  return api.post(`/quotes/${id}`, form)
}
