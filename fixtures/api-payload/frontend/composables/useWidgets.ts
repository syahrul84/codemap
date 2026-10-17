import { archiveWidget, retireWidget } from './widgets'
import { archivePayload } from './archivePayload'

export function onArchive(record: { id: number }) {
  return archiveWidget(record.id, archivePayload(record))
}

export function onArchiveLater(record: { id: number }) {
  const payload = archivePayload(record)
  return archiveWidget(record.id, payload)
}

export function onRetire(record: { id: number }) {
  return retireWidget(record.id, { ...record, _method: 'PUT' }, 'old')
}
