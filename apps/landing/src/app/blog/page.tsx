import type { Metadata } from "next";
import Link from "next/link";
import { Nav } from "@/components/Nav";
import { Grain } from "@/components/Grain";
import { Footer } from "@/components/sections/Footer";
import { ArrowRight } from "@/components/UI";
import { getPosts } from "@/lib/posts";

export const metadata: Metadata = {
  title: "Blog",
  description: "Notes on helping agents query real systems without writing integrations.",
};

export default function BlogIndex() {
  const posts = getPosts();

  return (
    <>
      <Grain />
      <Nav />
      <main className="mx-auto w-full max-w-4xl px-6 pt-36 pb-24 md:pt-44">
        <p className="text-[11px] font-medium uppercase tracking-[0.22em] text-gold/90">Writing</p>
        <h1 className="font-display mt-4 text-5xl tracking-tight text-cream md:text-6xl">
          From the Pawrly team
        </h1>
        <p className="mt-4 max-w-xl text-lg leading-relaxed text-muted">
          Notes on connecting real systems once, querying them clearly, and giving agents
          something safer than raw tools.
        </p>

        <div className="mt-14 flex flex-col">
          {posts.map((p, i) => (
            <Link
              key={p.slug}
              href={`/blog/${p.slug}`}
              className={`group flex flex-col gap-3 py-8 ${i > 0 ? "border-t border-line" : ""}`}
            >
              {p.image ? (
                <div
                  aria-label={p.image.alt}
                  role="img"
                  className="mb-3 aspect-[16/9] rounded-2xl border border-line bg-cover bg-center"
                  style={{ backgroundImage: `url(${p.image.src})` }}
                />
              ) : null}
              <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-gold/70">
                {p.readTime}
              </span>
              <h2 className="font-display text-3xl leading-snug text-cream transition-colors group-hover:text-gold-2">
                {p.title}
              </h2>
              <p className="max-w-2xl text-[15px] leading-relaxed text-muted">{p.excerpt}</p>
              <span className="inline-flex items-center gap-1.5 pt-1 text-sm font-medium text-foam transition-colors group-hover:text-cream">
                Read
                <ArrowRight />
              </span>
            </Link>
          ))}
        </div>
      </main>
      <Footer />
    </>
  );
}
