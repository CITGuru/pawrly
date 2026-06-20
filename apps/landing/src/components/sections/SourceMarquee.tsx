import { marqueeSources } from "@/lib/sources";

export function SourceMarquee() {
  const row = [...marqueeSources, ...marqueeSources];
  return (
    <section aria-label="Supported sources" className="relative overflow-hidden py-10">
      <p className="mb-7 text-center text-[11px] font-medium uppercase tracking-[0.22em] text-muted-2">
        One interface over the data your work already lives in
      </p>
      <div className="relative">
        {/* Edge fades so names dissolve into the water */}
        <div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 left-0 z-10 w-24"
          style={{ background: "linear-gradient(90deg, var(--ocean-950), transparent)" }}
        />
        <div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 right-0 z-10 w-24"
          style={{ background: "linear-gradient(270deg, var(--ocean-950), transparent)" }}
        />
        <div className="flex w-max marquee-track">
          {row.map((name, i) => (
            <span
              key={i}
              className="mx-5 inline-flex items-center gap-2 whitespace-nowrap font-display text-2xl text-foam/55"
            >
              {name}
              <span className="text-gold/40">·</span>
            </span>
          ))}
        </div>
      </div>
    </section>
  );
}
