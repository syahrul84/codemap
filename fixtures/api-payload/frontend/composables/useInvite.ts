import { invitesApi } from './invites'

export function voidInvite(id: number) {
  return invitesApi.act(id, 'void', { note: 'x', _method: 'PUT' })
}
