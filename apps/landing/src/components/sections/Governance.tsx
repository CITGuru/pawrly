import { SectionHeader } from "../UI";

const guards = [
  {
    k: "01",
    title: "Bad joins fail loudly",
    body: "If a join would double-count a metric, Pawrly stops the query instead of returning a polished wrong answer.",
  },
  {
    k: "02",
    title: "Access rules travel with the data",
    body: "Required filters are applied every time, whether the query comes from a person, script, or agent.",
  },
  {
    k: "03",
    title: "Rules are visible up front",
    body: "Agents can inspect the available tables, fields, joins, and required filters before they write a query.",
  },
];

export function Governance() {
  return (
    <section className="relative py-24 md:py-32">
      <div className="mx-auto max-w-6xl px-6">
        <SectionHeader
          eyebrow="Governed, not just connected"
          title="Answers you can review."
          description="Give important fields approved names, define which joins are allowed, and keep required filters attached to the data. Agents get useful access without direct access to every system."
        />

        <div className="mt-14 grid gap-px overflow-hidden rounded-2xl border border-line bg-line md:grid-cols-3">
          {guards.map((g) => (
            <div key={g.k} className="flex flex-col gap-4 bg-ocean-900/60 p-8">
              <span className="font-display text-3xl text-gold/50">{g.k}</span>
              <h3 className="text-lg font-semibold text-cream">{g.title}</h3>
              <p className="text-sm leading-relaxed text-muted">{g.body}</p>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
