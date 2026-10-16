import { NextResponse } from "next/server";

export async function GET(_req: Request, { params }: { params: { slug: string } }) {
  return NextResponse.json({ slug: params.slug, free: 2 });
}
