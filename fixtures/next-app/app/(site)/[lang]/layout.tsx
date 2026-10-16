import { SiteNav } from "@/components/site-nav";

export default function SiteLayout({ children }: { children: React.ReactNode }) {
  return (
    <div>
      <SiteNav />
      {children}
    </div>
  );
}
