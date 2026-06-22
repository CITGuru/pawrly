import { ogImage, OG_SIZE, OG_CONTENT_TYPE } from "@/lib/og";
import { docList } from "@/lib/docs-config";

export const alt = "Pawrly documentation";
export const size = OG_SIZE;
export const contentType = OG_CONTENT_TYPE;

export function generateStaticParams() {
  return docList.map((d) => ({ slug: d.slug }));
}

export default async function Image({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const meta = docList.find((d) => d.slug === slug);
  return ogImage({ eyebrow: "Documentation", title: meta?.title ?? "Documentation" });
}
