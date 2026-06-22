import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { DocsShell } from "@/components/docs/DocsShell";
import { CopyPageButton } from "@/components/docs/CopyPageButton";
import { CodeCopyEnhancer } from "@/components/docs/CodeCopyEnhancer";
import { DocsPrevNext } from "@/components/docs/DocsPrevNext";
import { getDoc } from "@/lib/docs";
import { docList } from "@/lib/docs-config";

export function generateStaticParams() {
  return docList.map((d) => ({ slug: d.slug }));
}

export async function generateMetadata({
  params,
}: {
  params: Promise<{ slug: string }>;
}): Promise<Metadata> {
  const { slug } = await params;
  const meta = docList.find((d) => d.slug === slug);
  if (!meta) return { title: "Documentation" };
  return {
    title: meta.title,
    description: meta.blurb,
    // Advertise the raw markdown so agents can fetch /docs/<slug>.md directly.
    alternates: { types: { "text/markdown": `/docs/${slug}.md` } },
    // Per-doc OG card comes from docs/[slug]/opengraph-image.tsx (file convention).
    openGraph: { title: `${meta.title} — Pawrly docs`, description: meta.blurb, type: "article" },
  };
}

export default async function DocPage({
  params,
}: {
  params: Promise<{ slug: string }>;
}) {
  const { slug } = await params;
  const doc = await getDoc(slug);
  if (!doc) notFound();

  return (
    <DocsShell toc={doc.toc}>
      <div className="flex items-start justify-between gap-4 border-b border-line pb-6">
        <h1 className="font-display text-4xl tracking-tight text-cream md:text-5xl">{doc.title}</h1>
        <CopyPageButton markdown={doc.raw} />
      </div>
      <article className="doc-prose mt-8" dangerouslySetInnerHTML={{ __html: doc.html }} />
      <DocsPrevNext slug={doc.slug} />
      <CodeCopyEnhancer />
    </DocsShell>
  );
}
