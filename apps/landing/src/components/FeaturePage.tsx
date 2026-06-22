import { ReactNode } from "react";
import Link from "next/link";
import { Nav } from "./Nav";
import { Grain } from "./Grain";
import { Footer } from "./sections/Footer";
import { Install } from "./sections/Install";
import { Eyebrow, ArrowRight } from "./UI";

const DOCS = "/docs";

/** Shared chrome for a feature page: hero + body + closing CTA. */
export function FeaturePage({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  return (
    <>
      <Grain />
      <Nav />
      <main className="flex w-full flex-col">
        <section className="bg-water relative overflow-hidden pt-36 pb-16 md:pt-44 md:pb-20">
          <div
            aria-hidden
            className="animate-caustics pointer-events-none absolute inset-0 opacity-60"
            style={{
              background:
                "radial-gradient(38% 26% at 50% 4%, rgba(241,220,176,0.22), transparent 70%), radial-gradient(30% 22% at 78% 22%, rgba(150,200,235,0.14), transparent 72%)",
            }}
          />
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 bottom-0 h-32"
            style={{ background: "linear-gradient(180deg, transparent, var(--ocean-950))" }}
          />
          <div className="relative mx-auto flex max-w-3xl flex-col items-center gap-5 px-6 text-center">
            <Link
              href="/"
              className="inline-flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-[0.22em] text-gold/90"
            >
              <span aria-hidden>←</span> Home
            </Link>
            <h1 className="font-display text-5xl leading-[1.05] tracking-tight text-cream md:text-6xl">
              {title}
            </h1>
            <p className="max-w-2xl text-lg leading-relaxed text-muted">{description}</p>
            <div className="mt-1 flex flex-col items-center gap-3 sm:flex-row">
              <Link
                href={DOCS}
                className="inline-flex items-center gap-2 rounded-full bg-sand px-5 py-3 text-sm font-semibold text-ocean-950 transition-colors hover:bg-gold-2"
              >
                Read the docs
                <ArrowRight />
              </Link>
            </div>
          </div>
        </section>

        <div className="mx-auto w-full max-w-5xl px-6 pb-8">{children}</div>

        <Install />
      </main>
      <Footer />
    </>
  );
}

/** One content block: heading + prose on the left, code/visual on the right. */
export function FeatureBlock({
  eyebrow,
  title,
  body,
  children,
  reverse = false,
}: {
  eyebrow?: string;
  title: string;
  body: ReactNode;
  children?: ReactNode;
  reverse?: boolean;
}) {
  return (
    <section className="border-t border-line py-14 first:border-t-0 md:py-16">
      <div className="grid items-start gap-8 lg:grid-cols-2 lg:gap-12">
        <div className={`flex flex-col gap-4 ${reverse ? "lg:order-2" : ""}`}>
          {eyebrow ? <Eyebrow>{eyebrow}</Eyebrow> : null}
          <h2 className="font-display text-3xl leading-tight tracking-tight text-cream md:text-4xl">
            {title}
          </h2>
          <div className="space-y-3 text-base leading-relaxed text-muted">{body}</div>
        </div>
        {children ? (
          <div className={`flex flex-col gap-4 ${reverse ? "lg:order-1" : ""}`}>{children}</div>
        ) : null}
      </div>
    </section>
  );
}
