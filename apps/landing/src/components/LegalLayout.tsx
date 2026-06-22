import { ReactNode } from "react";
import { Nav } from "./Nav";
import { Grain } from "./Grain";
import { Footer } from "./sections/Footer";

/** Shared chrome for legal / policy pages: title + last-updated + prose. */
export function LegalLayout({
  title,
  updated,
  intro,
  children,
}: {
  title: string;
  updated: string;
  intro?: string;
  children: ReactNode;
}) {
  return (
    <>
      <Grain />
      <Nav />
      <main className="mx-auto w-full max-w-3xl px-6 pt-36 pb-24 md:pt-44">
        <h1 className="font-display text-4xl tracking-tight text-cream md:text-5xl">{title}</h1>
        <p className="mt-3 font-mono text-xs uppercase tracking-[0.16em] text-muted-2">
          Last updated {updated}
        </p>
        {intro ? <p className="mt-6 text-lg leading-relaxed text-muted">{intro}</p> : null}
        <div className="prose-pawrly mt-10">{children}</div>
      </main>
      <Footer />
    </>
  );
}
