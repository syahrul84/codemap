import type { Room } from "@/lib/data";

export function RoomCard({ room, compact = false }: { room: Room; compact?: boolean }) {
  return <article>{room.name}{compact ? null : <Badge label={room.slug} />}</article>;
}

function Badge({ label }: { label: string }) {
  return <span>{label}</span>;
}
