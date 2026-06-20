import { Nav } from "@/components/Nav";
import { Grain } from "@/components/Grain";
import { Footer } from "@/components/sections/Footer";
import { Mark } from "@/components/Mark";
import { GhostCTA, ArrowRight } from "@/components/UI";

export default function NotFound() {
  return (
    <>
      <Grain />
      <Nav />
      <main className="flex w-full flex-col">
        <section className="bg-water relative overflow-hidden pt-36 pb-28 md:pt-44 md:pb-36">
          <div
            aria-hidden
            className="animate-caustics pointer-events-none absolute inset-0 opacity-60"
            style={{
              background:
                "radial-gradient(38% 26% at 50% 6%, rgba(241,220,176,0.24), transparent 70%), radial-gradient(30% 22% at 76% 22%, rgba(150,200,235,0.16), transparent 72%)",
            }}
          />
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 bottom-0 h-40"
            style={{ background: "linear-gradient(180deg, transparent, var(--ocean-950))" }}
          />

          <div className="relative mx-auto flex max-w-3xl flex-col items-center gap-6 px-6 text-center">
            <Mark size={40} className="text-cream" carve="var(--ocean-800)" />
            <span className="font-mono text-[11px] uppercase tracking-[0.22em] text-gold/80">
              error 404
            </span>
            <h1 className="font-display text-7xl leading-none tracking-tight text-cream sm:text-8xl md:text-[150px]">
              404
            </h1>
            <h2 className="font-display text-2xl tracking-tight text-cream md:text-3xl">
              Zero rows returned.
            </h2>
            <p className="max-w-md text-base leading-relaxed text-muted md:text-lg">
              That path didn&apos;t match anything in the workspace. The query came back empty —
              try one of these instead.
            </p>
            <div className="mt-2 flex flex-col items-center gap-3 sm:flex-row">
              <a
                href="/"
                className="inline-flex items-center gap-2 rounded-full bg-sand px-5 py-3 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2"
              >
                Back to home
                <ArrowRight />
              </a>
              <GhostCTA href="/blog">Read the blog</GhostCTA>
            </div>
          </div>
        </section>
      </main>
      <Footer />
    </>
  );
}
