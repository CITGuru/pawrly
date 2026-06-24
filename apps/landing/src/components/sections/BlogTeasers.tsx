import Link from "next/link";
import { SectionHeader, ArrowRight } from "../UI";
import { getPosts } from "@/lib/posts";

export function BlogTeasers() {
  const posts = getPosts().slice(0, 3);
  if (posts.length === 0) return null;

  return (
    <section id="blog" className="bg-surface-soft relative scroll-mt-24 py-24 md:py-32">
      <div className="mx-auto max-w-6xl px-6">
        <div className="flex flex-col items-start justify-between gap-6 md:flex-row md:items-end">
          <SectionHeader
            align="left"
            eyebrow="Writing"
            title="Why we're building Pawrly"
          />
          <Link
            href="/blog"
            className="inline-flex shrink-0 items-center gap-1.5 text-sm font-medium text-gold transition-colors hover:text-gold-2"
          >
            All posts
            <ArrowRight />
          </Link>
        </div>

        <div className="mt-12 grid gap-5 md:grid-cols-3">
          {posts.map((p) => (
            <Link
              key={p.slug}
              href={`/blog/${p.slug}`}
              className="card card-hover group flex flex-col gap-4 overflow-hidden rounded-2xl p-7"
            >
              {p.image ? (
                <div
                  aria-label={p.image.alt}
                  role="img"
                  className="-mx-7 -mt-7 mb-2 aspect-[16/9] border-b border-line bg-cover bg-center"
                  style={{ backgroundImage: `url(${p.image.src})` }}
                />
              ) : null}
              <span className="font-mono text-[11px] uppercase tracking-[0.18em] text-gold/70">
                {p.publishedDate ? `${p.publishedDate} / ${p.readTime}` : p.readTime}
              </span>
              <h3 className="font-display text-2xl leading-snug text-cream">{p.title}</h3>
              <p className="text-sm leading-relaxed text-muted">{p.excerpt}</p>
              <span className="mt-auto inline-flex items-center gap-1.5 pt-2 text-sm font-medium text-foam transition-colors group-hover:text-cream">
                Read
                <ArrowRight />
              </span>
            </Link>
          ))}
        </div>
      </div>
    </section>
  );
}
