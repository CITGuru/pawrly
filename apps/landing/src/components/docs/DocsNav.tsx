"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { docGroups } from "@/lib/docs-config";

/** The grouped docs navigation — shared by the desktop rail and mobile drawer. */
export function DocsNav({ onNavigate }: { onNavigate?: () => void }) {
  const pathname = usePathname();
  return (
    <nav className="flex flex-col gap-6">
      {docGroups.map((g) => (
        <div key={g.heading}>
          <p className="mb-2 px-3 text-[11px] font-medium uppercase tracking-[0.16em] text-muted-2">
            {g.heading}
          </p>
          <ul className="flex flex-col gap-0.5">
            {g.items.map((it) => {
              const href = `/docs/${it.slug}`;
              const active = pathname === href;
              return (
                <li key={it.slug}>
                  <Link
                    href={href}
                    onClick={onNavigate}
                    className={`block rounded-lg px-3 py-1.5 text-sm transition-colors ${
                      active
                        ? "bg-card-2 font-medium text-cream"
                        : "text-foam hover:bg-card hover:text-cream"
                    }`}
                  >
                    {it.title}
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}
