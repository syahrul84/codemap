import { createClient } from "@supabase/supabase-js";

export type Room = { id: string; slug: string; name: string };

const supabase = createClient("http://localhost", "anon");

export async function listRooms(): Promise<Room[]> {
  const { data } = await supabase.from("rooms").select("id, slug, name").order("name");
  return data ?? [];
}

export async function bookRoom(roomId: string) {
  return supabase.from("bookings").insert({ room_id: roomId });
}
