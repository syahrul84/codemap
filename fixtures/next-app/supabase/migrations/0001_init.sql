create table rooms (
  id uuid primary key default gen_random_uuid(),
  slug text not null unique,
  name text not null
);

create table bookings (
  id uuid primary key default gen_random_uuid(),
  room_id uuid not null references rooms(id),
  created_at timestamptz not null default now()
);
