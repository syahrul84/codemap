// Posts the body its callers build; the callers set `_method`.
export const invitesApi = {
  act: async (id: number, action: 'resend' | 'void', payload: Record<string, unknown>) =>
    api.post(`/invites/${id}/${action}`, payload),
}
