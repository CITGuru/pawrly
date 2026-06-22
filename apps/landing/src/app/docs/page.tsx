import type { Metadata } from "next";
import Link from "next/link";
import { DocsShell } from "@/components/docs/DocsShell";
import { ArrowRight } from "@/components/UI";
import { docGroups } from "@/lib/docs-config";

export const metadata: Metadata = {
  title: "Documentation",
  description:
    "Guides and reference for Pawrly — install, connect sources, model your data, and query it with SQL from the CLI, MCP, or an agent.",
};

export default function DocsIndex() {
  return (
    <DocsShell>
      <p className="text-[11px] font-medium uppercase tracking-[0.22em] text-gold/90">
        Documentation
      </p>
      <h1 className="font-display mt-3 text-4xl tracking-tight text-cream md:text-5xl">
        Pawrly documentation
      </h1>
      <p className="mt-4 max-w-2xl text-lg leading-relaxed text-muted">
        Everything to install Pawrly, connect your sources, model your data, and query it with SQL —
        from the CLI, MCP, or an agent.
      </p>

      <div className="mt-12 flex flex-col gap-10">
        {docGroups.map((g) => (
          <section key={g.heading}>
            <h2 className="font-display text-xl text-cream">{g.heading}</h2>
            <div className="mt-4 grid gap-3 sm:grid-cols-2">
              {g.items.map((it) => (
                <Link
                  key={it.slug}
                  href={`/docs/${it.slug}`}
                  className="card card-hover group flex flex-col gap-1.5 rounded-2xl p-5"
                >
                  <span className="flex items-center gap-1.5 text-[15px] font-medium text-cream">
                    {it.title}
                    <span className="text-foam transition-transform group-hover:translate-x-0.5">
                      <ArrowRight size={13} />
                    </span>
                  </span>
                  <span className="text-sm leading-relaxed text-muted">{it.blurb}</span>
                </Link>
              ))}
            </div>
          </section>
        ))}
      </div>
    </DocsShell>
  );
}
