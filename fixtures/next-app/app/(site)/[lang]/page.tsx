import { RoomCard } from "@/components/room-card";
import { listRooms } from "@/lib/data";

export default async function Home() {
  const rooms = await listRooms();
  return <main>{rooms.map((r) => <RoomCard key={r.id} room={r} />)}</main>;
}
