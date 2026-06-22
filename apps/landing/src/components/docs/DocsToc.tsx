"use client";

import { useEffect, useState } from "react";
import type { TocEntry } from "@/lib/docs-config";

export function DocsToc({ toc }: { toc: TocEntry[] }) {
  const [active, setActive] = useState<string>("");

  useEffect(() => {
    if (!toc.length) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) setActive(entry.target.id);
        }
      },
      { rootMargin: "-90px 0px -70% 0px", threshold: 0 }
    );
    for (const t of toc) {
      const el = document.getElementById(t.id);
      if (el) observer.observe(el);
    }
    return () => observer.disconnect();
  }, [toc]);

  if (!toc.length) return null;

  return (
    <aside className="hidden xl:block">
      <div className="sticky top-20 max-h-[calc(100vh-6rem)] overflow-y-auto pb-8">
        <p className="mb-3 text-[11px] font-medium uppercase tracking-[0.16em] text-muted-2">
          On this page
        </p>
        <ul className="flex flex-col">
          {toc.map((t) => (
            <li key={t.id}>
              <a
                href={`#${t.id}`}
                className={`block border-l py-1 text-sm transition-colors ${
                  t.depth === 3 ? "pl-6" : "pl-3"
                } ${
                  active === t.id
                    ? "border-gold text-cream"
                    : "border-line text-muted hover:text-cream"
                }`}
              >
                {t.text}
              </a>
            </li>
          ))}
        </ul>
      </div>
    </aside>
  );
}
