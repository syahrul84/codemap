"use client";
import { useEffect, useState } from "react";
import { t } from "@/lib/ui-strings";

export default function RoomPage({ params }: { params: { slug: string } }) {
  const [free, setFree] = useState(0);
  useEffect(() => {
    fetch(`/api/availability/${params.slug}`).then((r) => r.json()).then((d) => setFree(d.free));
  }, [params.slug]);
  return <p>{t("checkIn")}: {free}</p>;
}
