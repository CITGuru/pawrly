import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { Nav } from "@/components/Nav";
import { Grain } from "@/components/Grain";
import { Footer } from "@/components/sections/Footer";
import { getPost, getPosts } from "@/lib/posts";

export function generateStaticParams() {
  return getPosts().map((p) => ({ slug: p.slug }));
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ slug: string }>;
}): Promise<Metadata> {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) return { title: "Not found" };
  // Use the post's own image when it has one; otherwise fall back to the
  // generated card from blog/[slug]/opengraph-image.tsx (file convention).
  const img = post.image?.src;
  return {
    title: post.title,
    description: post.excerpt,
    openGraph: {
      title: post.title,
      description: post.excerpt,
      type: "article",
      ...(img ? { images: [img] } : {}),
    },
    twitter: {
      card: "summary_large_image",
      title: post.title,
      description: post.excerpt,
      ...(img ? { images: [img] } : {}),
    },
  };
}

export default async function BlogPost({
  params,
}: {
  params: Promise<{ slug: string }>;
}) {
  const { slug } = await params;
  const post = getPost(slug);
  if (!post) notFound();

  return (
    <>
      <Grain />
      <Nav />
      <main className="mx-auto w-full max-w-3xl px-6 pt-36 pb-24 md:pt-44">
        <Link
          href="/blog"
          className="inline-flex items-center gap-1.5 text-sm text-foam transition-colors hover:text-cream"
        >
          <span aria-hidden>←</span> All posts
        </Link>

        <p className="mt-10 font-mono text-[11px] uppercase tracking-[0.18em] text-gold/70">
          {post.publishedDate ? `${post.publishedDate} / ${post.readTime}` : post.readTime}
        </p>
        <h1 className="font-display mt-4 text-4xl leading-[1.1] tracking-tight text-cream md:text-5xl">
          {post.title}
        </h1>

        <article
          className="prose-pawrly mt-10"
          dangerouslySetInnerHTML={{ __html: post.html }}
        />
      </main>
      <Footer />
    </>
  );
}
