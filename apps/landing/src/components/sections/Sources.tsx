import { SectionHeader, Pill } from "../UI";
import { sourceGroups } from "@/lib/sources";

export function Sources() {
  return (
    <section id="sources" className="bg-surface-soft relative scroll-mt-24 py-24 md:py-32">
      <div className="mx-auto max-w-6xl px-6">
        <SectionHeader
          eyebrow="Sources"
          title="Query the data where it already lives."
          description="Bring Stripe, Postgres, CSVs, Snowflake, Linear, and internal MCP tools and APIs into the same SQL question. Pawrly handles the source details so the query can focus on the answer."
        />

        <div className="mt-14 grid gap-5 md:grid-cols-2 lg:grid-cols-3">
          {sourceGroups.map((group, i) => (
            <div
              key={group.kind}
              className={`card card-hover flex flex-col gap-4 rounded-2xl p-6 ${
                i === 0 ? "lg:col-span-2" : ""
              }`}
            >
              <div className="flex items-baseline justify-between gap-3">
                <h3 className="font-display text-2xl text-cream">{group.kind}</h3>
                <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-gold/70">
                  table
                </span>
              </div>
              <p className="text-sm leading-relaxed text-muted">{group.blurb}</p>
              <div className="mt-auto flex flex-wrap gap-2 pt-1">
                {group.items.map((item) => (
                  <Pill key={item}>{item}</Pill>
                ))}
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
