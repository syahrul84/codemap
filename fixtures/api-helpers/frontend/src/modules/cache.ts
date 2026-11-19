// Not an HTTP client: a `get` on it is no call to the API.
const client = new Map<string, unknown>()

export const cached = (key: string) => client.get(key)
